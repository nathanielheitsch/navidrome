// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! BOOT-01: construction of the shared transcoding cache and concurrency state.
//!
//! [`StreamState::new`] turns a resolved cache directory, a typed config budget,
//! and the two concurrency caps into the single object later stream handlers
//! share. Like the other BOOT-01 constructors it is a **library entry point**: it
//! launches no process, serves no request, runs no cleanup timer, and owns no
//! listener, route, or scanner concern.
//!
//! What it guarantees:
//!
//! * **One cache, shared by every clone.** Exactly one [`TranscodeCache`] is
//!   created and held in an [`Arc`]; `TranscodeLimiter` is itself an `Arc`-backed
//!   shareable value, so cloning a [`StreamState`] gives a handler the *same*
//!   global and per-user transcode budget rather than a fresh counter set.
//! * **The cache root is the existing constant, joined to the resolved cache dir.**
//!   `cache_dir.join(TRANSCODING_CACHE_DIR)` — the same spelling Go uses
//!   (`server/subsonic/stream.go` roots the file cache at
//!   `conf.CacheFolder/transcoding`). The directory is built by
//!   [`TranscodeCache::new`]'s own `create_dir_all`, which is correct for a cache
//!   folder and **never** for the music root; nothing here calls `Dir::path` on a
//!   music root or changes the process cwd.
//! * **Fixed item budget and cleanup interval.**
//!   [`DEFAULT_TRANSCODING_CACHE_MAX_ITEMS`] (`0` = unlimited, Go's default) and
//!   [`DEFAULT_CACHE_CLEAN_UP_INTERVAL`]. The port has no background haunter
//!   (`TranscodeCache` documents this: the caller decides when to scrub), so no
//!   timer is started here either.
//! * **Everything validated before any filesystem side effect.** Both caps must
//!   be non-negative and at most `i32::MAX`, converted with `try_from` rather than
//!   a cast or `as`; `0` means unlimited, matching
//!   [`TranscodeLimiter::new`]'s non-positive-disable rule. The typed budget is
//!   re-parsed with the *same* [`nd_conf::parse_bytes`] the loader used and must
//!   agree with [`ByteSizeField::as_bytes`]; a zero budget is refused explicitly.
//!   A bad cap or bad budget therefore creates no directory.
//!
//! ## Why the budget is re-parsed instead of taken as given
//!
//! [`ByteSizeField`] has two ways to exist. Serde deserialization validates it —
//! it rejects an unparseable value and rejects zero. [`ByteSizeField::from_raw`]
//! does **not**: it maps any unparseable spelling to `bytes: 0` silently, and it
//! is how loader defaults are built. So a `ByteSizeField` reaching this boundary
//! may be an unvalidated value, and `as_bytes()` alone can report `0` for what was
//! really a typo. Construction therefore re-derives the number from
//! `field.raw()` with `parse_bytes` and requires it to equal `field.as_bytes()`,
//! then refuses `0` — because Go's file cache treats a parsed size of `0` as
//! *disabled* (`file_caches.go:275-278`), and a config-derived budget of zero in
//! the current contract is an invalid value rather than a request to disable
//! transcoding caching. `nd-conf`'s own zero policy is untouched: this is a
//! consumer-side contract on a config-derived value, not a change to the type.
//!
//! [`TranscodeCache::new`] takes the raw spelling and applies Go's own arms
//! (unparseable → `DefaultCacheSize`, `0` → disabled). Those fallbacks are
//! unreachable through this constructor precisely because of the checks above, and
//! there is deliberately **no** public constructor that accepts a bare `&str`
//! cache size, so nothing here can silently fall back to a default budget.
//!
//! ## Disclosure
//!
//! The cache root is an arbitrary caller-supplied path, so it is not a diagnostic:
//! [`StreamState`] has a manual `Debug` that reports only the numeric caps and the
//! byte budget, and [`StreamStateError`] carries a static class plus a numeric
//! `errno` — no `#[source]`, no `JoinError`, no path text, no OS message.

use std::path::Path;
use std::sync::Arc;

use nd_conf::bytes::ByteSizeField;
use nd_conf::parse_bytes;
use nd_stream::limiter::TranscodeLimiter;
use nd_stream::transcode_cache::{
    TranscodeCache, DEFAULT_CACHE_CLEAN_UP_INTERVAL, DEFAULT_TRANSCODING_CACHE_MAX_ITEMS,
    TRANSCODING_CACHE_DIR,
};

/// Why transcoding-state construction refused.
///
/// Each variant carries a static class and, where applicable, a **numeric** value
/// only. Nothing here holds a path, an OS error message, a config string, or an
/// upstream error payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StreamStateError {
    /// The resolved cache directory must be absolute. Startup anchors `CacheFolder`
    /// to the captured cwd, so a relative path here means the caller skipped that
    /// anchor; creating it would write wherever the process cwd happens to be.
    #[error("cache directory must be absolute")]
    RelativeCacheDir,
    /// A concurrency cap is negative. Go's limiter derives its capacity from
    /// `cmp.Max(0, limit)`, so a negative value is not "unlimited" but a
    /// misconfiguration; the canonical boundary refuses it.
    #[error("transcode concurrency cap must not be negative")]
    NegativeCap,
    /// A concurrency cap exceeds `i32::MAX`, the width [`TranscodeLimiter::new`]
    /// takes. Refused as a checked conversion rather than saturated or truncated.
    #[error("transcode concurrency cap exceeds the supported range")]
    CapOutOfRange,
    /// The budget's raw spelling cannot be parsed by the same parser the loader
    /// uses. Reported because a [`ByteSizeField`] built by `from_raw` can carry an
    /// unparseable spelling with a silent `bytes: 0`.
    #[error("transcoding cache budget is not a valid byte size")]
    UnparsableBudget,
    /// The parsed budget does not equal the value the field reports. A field whose
    /// `bytes` and `raw` disagree describes two different budgets, so there is no
    /// honest value to construct with.
    #[error("transcoding cache budget does not match its configured spelling")]
    BudgetMismatch,
    /// A budget of zero. `TranscodeCache` reads that as *disabled* and would
    /// create no directory at all; for a config-derived value under the current
    /// contract it is an invalid launch value, so it is refused explicitly.
    #[error("transcoding cache budget must be greater than zero")]
    ZeroBudget,
    /// The cache root could not be created. Only the numeric `errno` survives.
    #[error("transcoding cache directory could not be created (errno {errno})")]
    CacheDirUnavailable {
        /// Raw OS error code. Numeric only.
        errno: i32,
    },
    /// The blocking construction task failed before returning (panic or
    /// cancellation). Static: a `JoinError` prints the panic payload.
    #[error("transcoding cache construction failed before completing")]
    ConstructionFailed,
}

/// The shared transcoding cache and concurrency state.
///
/// `Clone` is the intended use: every handler gets a cheap clone that shares the
/// one cache and the one limiter.
#[derive(Clone)]
pub struct StreamState {
    /// The single cache instance. Cloning the `Arc` shares the root, the budgets
    /// and the in-flight entry map.
    pub cache: Arc<TranscodeCache>,
    /// The shareable limiter. `TranscodeLimiter::clone` clones its inner `Arc`, so
    /// every clone of this state shares the same global and per-user counters.
    pub limiter: TranscodeLimiter,
    /// Byte budget actually applied, i.e. the re-parsed and re-checked budget.
    /// Non-secret.
    pub cache_size_bytes: u64,
    /// Global concurrent-transcode cap (`0` = unlimited), as handed to the limiter.
    pub global_limit: i32,
    /// Per-user concurrent-transcode cap (`0` = unlimited).
    pub user_limit: i32,
}

impl StreamState {
    /// Construct the shared transcoding state (BOOT-01 cache/limiter wiring).
    ///
    /// `cache_dir` is the **resolved** cache directory — the absolute path startup
    /// anchored from the captured startup cwd. The cache root built here is
    /// `cache_dir.join(TRANSCODING_CACHE_DIR)`.
    ///
    /// `budget` is the typed config value; it is re-parsed and re-checked (see the
    /// module doc) rather than trusted. `0` caps mean unlimited.
    ///
    /// All validation happens on the calling thread before any directory exists;
    /// only the cache-root creation runs on a blocking worker.
    ///
    /// # Errors
    /// [`StreamStateError`] with a static class and numeric detail. No state and no
    /// directory is produced on failure.
    pub async fn new(
        cache_dir: &Path,
        budget: &ByteSizeField,
        global_limit: i64,
        user_limit: i64,
    ) -> Result<Self, StreamStateError> {
        // --- Pre-checks: no side effect may happen before these pass. ---
        if !cache_dir.is_absolute() {
            return Err(StreamStateError::RelativeCacheDir);
        }
        let global_limit = checked_cap(global_limit)?;
        let user_limit = checked_cap(user_limit)?;
        let cache_size_bytes = checked_budget(budget)?;

        // The root is the existing constant joined to the resolved cache dir; the
        // raw spelling is what TranscodeCache's own parser expects.
        let root = cache_dir.join(TRANSCODING_CACHE_DIR);
        let raw = budget.raw().to_string();

        // Directory creation is the only blocking work, so it goes off the async
        // worker thread (the same boundary startup uses). No cleanup timer, no
        // request, no process is started.
        let cache = tokio::task::spawn_blocking(move || {
            TranscodeCache::new(
                root,
                raw.as_str(),
                DEFAULT_TRANSCODING_CACHE_MAX_ITEMS,
                DEFAULT_CACHE_CLEAN_UP_INTERVAL,
            )
            .map_err(|err| StreamStateError::CacheDirUnavailable {
                errno: err.raw_os_error().unwrap_or(0),
            })
        })
        .await
        .map_err(|_| StreamStateError::ConstructionFailed)??;

        Ok(StreamState {
            cache: Arc::new(cache),
            // Cloning this shares the counters: TranscodeLimiter is Arc-backed.
            limiter: TranscodeLimiter::new(global_limit, user_limit),
            cache_size_bytes,
            global_limit,
            user_limit,
        })
    }

    /// `true` when two states share the one cache — the identity proof the tests
    /// assert, alongside the behavioral consequence.
    #[must_use]
    pub fn shares_cache(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cache, &other.cache)
    }
}

/// Checked conversion of a launch cap into the limiter's `i32` width.
///
/// `0` is preserved (it means unlimited), negatives are refused, and a value above
/// `i32::MAX` is refused by `try_from` rather than truncated by `as`.
fn checked_cap(value: i64) -> Result<i32, StreamStateError> {
    if value < 0 {
        return Err(StreamStateError::NegativeCap);
    }
    i32::try_from(value).map_err(|_| StreamStateError::CapOutOfRange)
}

/// Re-derive and check the typed budget: same parser as the loader, agreement with
/// the field's own value, and non-zero.
fn checked_budget(field: &ByteSizeField) -> Result<u64, StreamStateError> {
    let parsed = parse_bytes(field.raw()).map_err(|_| StreamStateError::UnparsableBudget)?;
    if parsed.as_bytes() != field.as_bytes() {
        return Err(StreamStateError::BudgetMismatch);
    }
    if parsed.as_bytes() == 0 {
        return Err(StreamStateError::ZeroBudget);
    }
    Ok(parsed.as_bytes())
}

/// Manual `Debug`: numeric launch facts only.
///
/// The cache root is an arbitrary caller-supplied path and the cache itself
/// reports it (`TranscodeCache` derives `Debug`), so neither is printed; there is
/// no secret in this type, but a path in a log line is still a disclosure surface.
impl std::fmt::Debug for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamState")
            .field("cache_size_bytes", &self.cache_size_bytes)
            .field("global_limit", &self.global_limit)
            .field("user_limit", &self.user_limit)
            // Deliberately withheld: cache root path and its Debug.
            .field("cache", &"<transcode cache>")
            .field("limiter", &"<shared limiter>")
            .finish()
    }
}
