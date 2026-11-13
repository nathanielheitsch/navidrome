//! Transcoding **file cache** — the Rust port of `utils/cache/file_caches.go`
//! (`fileCache` + `spreadFS` + `fileHaunter`) wired to the transcode producer
//! (`core/stream/media_streamer.go:224-282`), and the missing single-flight that
//! makes two identical pending requests launch one ffmpeg process.
//!
//! Go's `cache.Get` is the **only** path to a transcode
//! (`media_streamer.go:104-125`): the rate-limit rejection travels through it
//! unchanged, and `media_streamer.go:114-117` deliberately does **not** log
//! `ErrTooManyTranscodes` as a cache failure. This lane preserves that by returning
//! [`GateError::Rejected`] from [`TranscodeSource::open`] untouched, so the
//! handler's existing 429 mapping (`crate::handler::StreamError::TooManyTranscodes`)
//! is reached exactly as in Go.
//!
//! ## Observable contract (the Go tests are the spec)
//!
//! `utils/cache/file_caches_test.go` pins `Cached` semantics, fan-out, and the
//! failure arms; this port reproduces all of them:
//!
//!   - **`Cached` means "not the writer", not "complete"**
//!     (`file_caches.go:169` — `cached := w == nil`). First `Get` → `Cached =
//!     false` (this caller got the writer). A second `Get` while the entry is in
//!     flight → `Cached = **true**` but **non-seekable**: the writer is gone and the
//!     final size is unknown, so `getFinalCachedSize` (`:244-253`) returns `-1` and
//!     no `SectionReader` is built (`:196-208`). Steady state → `Cached = true`
//!     **and** seekable. The handler keys on seekable-vs-pipe, so both arms stay
//!     correct.
//!   - **Fan-out from offset 0.** A reader that joins mid-write sees **all** bytes
//!     from 0, because Go's `fscache.CacheReader` follows file growth
//!     (`file_caches_test.go:286-308`). File-backed here: the producer appends
//!     chunks to the entry's data file and every reader holds its **own** handle with
//!     its own cursor, waiting on a `Notify` until `written > cursor`. Memory stays
//!     bounded because the entry lives on disk, as in Go.
//!   - **Background drain.** Go runs `go fc.copyAndClose(...)`
//!     (`file_caches.go:180-189`), so ffmpeg is drained into the cache file
//!     regardless of readers. Mirrored by a spawned task that drives
//!     [`GatedTranscode::serve`] into the entry file and then finalizes: complete +
//!     completion marker, or the `CloseWithError` rule (`file_caches.go:249-258`) —
//!     record the cause and invalidate (remove the file, **no** marker). Attached
//!     readers drain the remaining bytes and **then** surface the cause rather than
//!     reaching a clean EOF.
//!   - **Only the first requester's disconnect drives ffmpeg.** Go's producer runs
//!     inside the first `Get` with *that* request's context
//!     (`media_streamer.go:243-260`), so a later reader hanging up cannot kill the
//!     shared producer. See [`SharedDisconnect`] for how the port reproduces that
//!     with a single poller of the disconnect future.
//!   - **Never release or double-acquire a slot.** The drain task owns the
//!     [`GatedTranscode`] and reaps it at copy end, which is the only release point
//!     (see the permit-lifetime notes in `crate::transcode_body`: this port frees
//!     capacity only on a **confirmed** reap, a deliberate documented deviation from
//!     Go's `limiter.go:63-66`).
//!
//! ## Key and layout
//!
//! Go's `spreadFS.KeyMapper` (`spread_fs.go:177-186`) maps the key to
//! `root/xx/xx/<40-hex>` with `sha1(key)`, and the key is the `streamJob` fields
//! (`media_streamer.go:59-61`): `id.updatedAtRFC3339Nano.bitRate.sampleRate.
//! bitDepth.channels.format.offset`. [`stream_cache_key`] and [`key_mapper`]
//! reproduce both.
//!
//! ## Declared divergences
//!
//!   - **`updated_at` formats as UTC, not in the local zone.** Go's key uses
//!     `mf.UpdatedAt.Format(time.RFC3339Nano)` with the process's local zone
//!     (`media_streamer.go:60`). The port always formats the UTC instant, e.g.
//!     `2023-11-14T17:13:20Z`. A key is a hash input and a file name — nothing
//!     user-visible depends on it — and within one process the zone is constant, so
//!     entries stay self-consistent; only the on-disk *names* differ from a Go cache
//!     written in a non-UTC zone. Reproducing the zone offset is cross-platform
//!     guesswork here, so this trades a cosmetic name difference for determinism.
//!     The oracle (`tests/data/go_stream_key/`) is generated with `TZ=UTC`.
//!   - **Cache initialization is synchronous, not a background goroutine.** Go's
//!     `NewFileCache` returns immediately and marks `ready` from a goroutine
//!     (`file_caches.go:75-100`), so early calls take the "not initialized" arm and
//!     read straight from the reader. [`TranscodeCache::new`] creates the root before
//!     returning, so that arm cannot fire. Explicit construction, no ambient runtime
//!     state, per this workspace's freeze (`rust/Cargo.toml`).
//!   - **A disabled cache delegates instead of returning nil.** Go's `size == 0`
//!     leaves the cache nil (`file_caches.go:275-278`) and every `Get` falls through
//!     to `getReader` uncached (`:145-152`). [`TranscodeCache::disabled`] reports that
//!     state and [`CacheTranscodeSource`] delegates to the uncached gated launch — the
//!     same observable behavior.
//!   - **No migration sentinel.** Go's `spreadFS.Reload` migrates pre-marker files
//!     once via a `.nd-migrated` sentinel (`spread_fs.go:44-86`). This port never
//!     writes an unmarked *complete* file, so a marker-less data file is always a
//!     crash partial; [`TranscodeCache::open_key`] discards it (Go's "Removing
//!     incomplete cache file", `spread_fs.go:60-64`). Adoption needs no `Reload`: the
//!     marker file is the authority, checked per key.
//!   - **Eviction is explicit, not a periodic haunter.** Go's `fileHaunter` runs every
//!     `period` (`file_haunter.go:29-35`, 10 min default,
//!     `consts.DefaultCacheCleanUpInterval`). [`TranscodeCache::scrub`] is the same
//!     algorithm, callable by the caller: invoked on entry completion and by tests.
//!     Nothing here spawns a timer; [`TranscodeCache::cleanup_interval`] reports the
//!     period a scheduler would use.
//!   - **Seam deviation: the writer lease hands back a read-only disconnect observer
//!     instead of `None`.** In Go the writer arm returns a `CachedStream` with no
//!     separate cancellation channel at all — the producer holds the request context
//!     (`media_streamer.go:122-125`) and the handler pumps the body unconditionally.
//!     This port's handler cannot: its `Cached` arm only pumps when a signal is
//!     present (`handler.rs:1364`) and `TranscodeLease::Cached` reports no pid
//!     (`:907`), so a writer lease with `signal: None` answers 200 with **0 bytes**.
//!     The writer lease therefore hands back [`SharedDisconnect`] — a read-only
//!     *observer* that does not own the caller's future. The invariants that matter
//!     are preserved: **exactly one** task polls the caller's `D`, the **first
//!     requester's** disconnect still drives ffmpeg cancellation inside the gate, and
//!     attached readers never kill the shared producer. Follow-up (handler side, not
//!     this lane): a `Cached` arm that pumps without a signal, after which the writer
//!     lease can return `None`.
//!   - **In-use protection is counted by leases, not `fscache.Entry.InUse()`.** Go's
//!     haunter skips entries in use (`file_haunter.go:41-43`). The port counts live
//!     leases per data file plus the in-flight entry set, covering the same window: a
//!     file a caller is holding cannot be reaped.
//!   - **A 0-byte transcode is marked complete and then reaped.** Go's `copyAndClose`
//!     succeeds on an empty stream and marks it complete
//!     (`file_caches.go:246-248`); the haunter removes it as an "invalid empty file"
//!     (`file_haunter.go:53-56`). Same here: completion is recorded, and `scrub`
//!     removes the file. An empty transcode is not an error in Go either
//!     (`media_streamer.go:190-193`).

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{watch, Notify};

use crate::archiver::BoxFuture;
use crate::handler::{CachedTranscode, TranscodeLease, TranscodeSource};
use crate::limiter::TranscodeLimiter;
use crate::transcode_body::{
    open_transcode_gated, Disconnect, GateError, GatedTranscode, TranscodeBodyError, TranscodeJob,
};
use crate::types::MediaFile;

/// `consts.TranscodingCacheDir` (`consts/consts.go:133`).
pub const TRANSCODING_CACHE_DIR: &str = "transcoding";
/// `consts.DefaultTranscodingCacheMaxItems` (`consts/consts.go:134`): `0` =
/// unlimited, exactly how Go's haunter treats it (`file_haunter.go:95-107`).
pub const DEFAULT_TRANSCODING_CACHE_MAX_ITEMS: usize = 0;
/// `consts.DefaultCacheSize` (`consts/consts.go:139`): 100 MB.
pub const DEFAULT_CACHE_SIZE_BYTES: u64 = 100 * 1024 * 1024;
/// `consts.DefaultCacheCleanUpInterval` (`consts/consts.go:140`): 10 minutes.
pub const DEFAULT_CACHE_CLEAN_UP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// `cache.completeMarkerSuffix` (`spread_fs.go:16`).
pub const COMPLETE_MARKER_SUFFIX: &str = ".complete";
/// Length of the sha1 hex digest Go uses as the data file name
/// (`spread_fs.go:183-185`).
pub const KEY_HASH_HEX_LEN: usize = 40;
/// Bytes one `CachedTranscode::read_chunk` pulls by default when the handler does
/// not choose a size: the `io.Copy` internal buffer Go uses (`io.copyLoop`).
pub const CACHE_READ_CHUNK_BYTES: usize = 32 * 1024;

/// `streamJob.Key()` (`media_streamer.go:59-61`):
/// `id.updatedAt.bitRate.sampleRate.bitDepth.channels.format.offset`.
///
/// Note Go's field order: `format` comes **after** `channels` and before `offset`.
/// `updated_at` is epoch seconds (Go's `time.Time` is flattened to seconds
/// throughout this port, `lib.rs`), rendered as RFC3339Nano — which for a
/// whole-second instant is the second-precision form. See the module doc for the
/// UTC divergence.
#[must_use]
pub fn stream_cache_key(mf: &MediaFile, job: &TranscodeJob) -> String {
    format!(
        "{}.{}.{}.{}.{}.{}.{}.{}",
        mf.id,
        rfc3339_utc(mf.updated_at),
        job.bit_rate,
        job.sample_rate,
        job.bit_depth,
        job.channels,
        job.format,
        job.offset,
    )
}

/// `time.Unix(secs, 0).Format(time.RFC3339Nano)` in UTC.
///
/// RFC3339Nano drops trailing zeros in the fraction; this port only ever receives
/// whole-second instants (`MediaFile.UpdatedAt` is epoch seconds, and the token
/// half truncates to seconds as Go's `Truncate(time.Second)` does), so the output
/// is the second-precision form. Instants before the epoch render backwards as in
/// Go (`1969-12-31T19:00:00Z`-style).
#[must_use]
pub fn rfc3339_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

/// Hinnant's `civil_from_days`: days since 1970-01-01 → `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// `spreadFS.KeyMapper` (`spread_fs.go:177-186`): `sha1(key)` as 40 hex chars laid
/// out `root/xx/xx/<40-hex>`, so no single directory holds every entry.
///
/// Go's idempotence arm is reproduced: a key already under `root` is returned
/// unchanged, because the haunter passes mapped paths rather than keys.
#[must_use]
pub fn key_mapper(root: &Path, key: &str) -> PathBuf {
    if Path::new(key).starts_with(root) {
        return PathBuf::from(key);
    }
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(KEY_HASH_HEX_LEN);
    for byte in digest.iter() {
        hash.push_str(&format!("{byte:02x}"));
    }
    // Go's `hash[0:2]` / `hash[2:4]` are slices of the hex string.
    root.join(&hash[0..2]).join(&hash[2..4]).join(hash)
}

/// Path of an entry's completion marker (`spreadFS.markerPath`,
/// `spread_fs.go:141-143`).
#[must_use]
pub fn marker_path(data_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", data_path.display(), COMPLETE_MARKER_SUFFIX))
}

/// Whether a file name is a cache data file: `<40 lowercase hex>`
/// (`spread_fs.go:114-119` rejects anything else in the layout).
#[must_use]
fn is_data_file_name(name: &str) -> bool {
    name.len() == KEY_HASH_HEX_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// What [`TranscodeCache::open_key`] resolved, mirroring `fscache.Cache.Get` plus
/// the marker check.
#[derive(Debug)]
pub enum Opened {
    /// Complete on disk: seekable, final size known. Go's `SectionReader` arm
    /// (`file_caches.go:196-205`).
    Complete {
        /// The cached transcoded file.
        data_path: PathBuf,
        /// Its size in bytes.
        size: u64,
    },
    /// This caller is the **writer**; `Cached` is `false` here and only here.
    Writer {
        /// The live entry to produce into.
        entry: Arc<Entry>,
    },
    /// Another caller is producing this entry: `Cached = true`, **non-seekable**.
    Attached {
        /// A reader that follows the entry's growth from offset 0.
        item: CacheItem,
    },
}

/// A cache entry: data file, state, fan-out notification, and in-use count.
#[derive(Debug)]
pub struct Entry {
    /// The `streamJob` key.
    key: String,
    /// `root/xx/xx/<40hex>` ([`key_mapper`]).
    data_path: PathBuf,
    /// `<data_path>.complete` ([`marker_path`]).
    marker_path: PathBuf,
    /// Written bytes and completion state, shared by producer and readers.
    state: Mutex<State>,
    /// Wake-up for readers — Go's `CacheReader` following file growth.
    notify: Notify,
    /// Live leases on this entry (Go's `fscache.Entry.InUse()`).
    in_use: AtomicUsize,
    /// Pid of the ffmpeg process producing this entry, once launched. Callers that
    /// share an entry observe the **same** pid, which is what the single-flight
    /// test asserts.
    pid: Mutex<Option<u32>>,
}

#[derive(Debug)]
struct State {
    /// Bytes appended to the data file so far.
    written: u64,
    status: Status,
}

#[derive(Debug)]
enum Status {
    /// Still being produced.
    Writing,
    /// Written in full; the marker file exists.
    Complete { size: u64 },
    /// Production failed with this cause. Readers drain what was written and then
    /// fail with it, never a clean EOF (`file_caches.go:249-258`).
    Failed { cause: String },
}

impl Entry {
    /// The `streamJob` key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Data file path.
    #[must_use]
    pub fn data_path(&self) -> &Path {
        &self.data_path
    }

    /// Completion marker path.
    #[must_use]
    pub fn marker_path(&self) -> &Path {
        &self.marker_path
    }

    /// Bytes appended so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.state.lock().expect("cache entry state").written
    }

    /// Final size of a complete entry, `None` while it is in flight or failed.
    ///
    /// This is Go's `getFinalCachedSize` (`file_caches.go:244-253`): `-1` unless the
    /// entry is settled, which is precisely when a `SectionReader` (a seekable body)
    /// may be handed back.
    #[must_use]
    pub fn final_size(&self) -> Option<u64> {
        match self.state.lock().expect("cache entry state").status {
            Status::Complete { size } => Some(size),
            _ => None,
        }
    }

    /// Whether the entry is complete in memory.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(
            self.state.lock().expect("cache entry state").status,
            Status::Complete { .. }
        )
    }

    /// Whether production failed, with the cause.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        match &self.state.lock().expect("cache entry state").status {
            Status::Failed { cause } => Some(cause.clone()),
            _ => None,
        }
    }

    /// Pid of the producing ffmpeg process, if launched.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        *self.pid.lock().expect("cache entry pid")
    }

    fn set_pid(&self, pid: Option<u32>) {
        *self.pid.lock().expect("cache entry pid") = pid;
    }

    /// Live leases (Go's `InUse`).
    #[must_use]
    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Acquire)
    }

    /// Append `bytes` to the data file, publish the new size, and wake readers.
    ///
    /// This is the body of Go's `io.Copy` into the cache file
    /// (`file_caches.go:236`). The size is published only after the write syscall
    /// completed, so a reader that sees `written > cursor` can always read those
    /// bytes. Exposed so tests can drive an entry without launching ffmpeg; the
    /// production path uses it through [`CacheSink`].
    pub async fn append(&self, bytes: &[u8]) -> io::Result<usize> {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.data_path)
            .await?;
        let n = file.write(bytes).await?;
        file.flush().await?;
        if n > 0 {
            self.state.lock().expect("cache entry state").written += n as u64;
            self.notify.notify_waiters();
        }
        Ok(n)
    }
}

/// A reader on a cache entry: its own file handle and cursor, so it sees every byte
/// from offset 0 no matter when it joined (`file_caches_test.go:286-308`).
///
/// This is Go's `fscache.CacheReader` arm of `CachedStream`: `Cached = true`, no
/// `Seeker` while the entry is in flight, and one once it is complete.
#[derive(Debug)]
pub struct CacheItem {
    entry: Arc<Entry>,
    /// `None` after [`CachedTranscode::close`].
    file: Option<tokio::fs::File>,
    cursor: u64,
    /// Go's `CachedStream.Cached`: `false` only for the writer-side lease.
    cached: bool,
    /// Set when the entry was already complete when this lease opened.
    seekable: bool,
    /// Keeps `scrub` from reaping the file this lease is reading.
    protect: Option<LeaseGuard>,
}

/// Counts a live lease on a complete entry's data file for the in-use skip in
/// [`TranscodeCache::scrub`] — the port's equivalent of Go's `e.InUse()`
/// (`file_haunter.go:41-43`).
#[derive(Debug)]
struct LeaseGuard {
    cache: Arc<TranscodeCache>,
    path: PathBuf,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.cache.release_lease(&self.path);
    }
}

/// Result of [`CacheTranscodeSource::get`] at the cache level: a lease plus the
/// `Cached` flag Go reports to the handler (`media_streamer.go:121`).
#[derive(Debug)]
pub struct CacheLease {
    /// The body the caller reads.
    pub item: CacheItem,
    /// Go's `r.Cached`: `false` only for the writer.
    pub cached: bool,
    /// Pid of the shared producer, when one is running.
    pub pid: Option<u32>,
}

/// What one [`TranscodeCache::scrub`] removed and what is left.
#[derive(Debug, Default)]
pub struct ScrubReport {
    /// Data files removed (empty files, reaped entries, orphan markers).
    pub reaped: Vec<PathBuf>,
    /// Complete entries left in the cache.
    pub remaining_items: usize,
    /// Bytes left in the cache.
    pub remaining_size: u64,
}

/// The transcoding file cache: a root directory, a byte budget, and an item budget.
///
/// Explicit construction (workspace freeze in `rust/Cargo.toml`): no ambient
/// configuration, no background timer. [`Self::scrub`] is the eviction step; the
/// caller decides when to run it.
#[derive(Debug)]
pub struct TranscodeCache {
    root: PathBuf,
    max_items: usize,
    max_size: u64,
    cleanup_interval: Duration,
    disabled: bool,
    /// In-flight entries only. Complete entries are authority **on disk** (the
    /// marker file), so there is no index to keep warm across restarts.
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    /// Live leases on complete entries, keyed by data file.
    open_leases: Mutex<HashMap<PathBuf, usize>>,
}

impl TranscodeCache {
    /// Create the cache rooted at `root` with `cache_size` (Go's humanized form,
    /// parsed by `nd-conf`'s port of `humanize.ParseBytes`;
    /// `file_caches.go:268-278`) and `max_items` (`0` = unlimited).
    ///
    /// Go's arms: an unparseable size falls back to `consts.DefaultCacheSize`
    /// (`:269-273`), and a size of `0` disables the cache (`:275-278`).
    pub fn new(
        root: impl AsRef<Path>,
        cache_size: &str,
        max_items: usize,
        cleanup_interval: Duration,
    ) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let size = match nd_conf::parse_bytes(cache_size) {
            Ok(size) => size.0,
            Err(_) => DEFAULT_CACHE_SIZE_BYTES,
        };
        let cache = TranscodeCache {
            root,
            max_items,
            max_size: size,
            cleanup_interval,
            disabled: size == 0,
            entries: Mutex::new(HashMap::new()),
            open_leases: Mutex::new(HashMap::new()),
        };
        if cache.disabled {
            return Ok(cache);
        }
        std::fs::create_dir_all(&cache.root)?;
        Ok(cache)
    }

    /// Go's `Disabled` (`file_caches.go:118-122`).
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    /// Go's `Available` (`file_caches.go:112-116`): ready **and** not disabled.
    /// Construction is synchronous, so readiness is always true here.
    #[must_use]
    pub fn available(&self) -> bool {
        !self.disabled
    }

    /// Cache root holding the `xx/xx/<40hex>` layout.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Item budget (`0` = unlimited).
    #[must_use]
    pub fn max_items(&self) -> usize {
        self.max_items
    }

    /// Byte budget.
    #[must_use]
    pub fn max_size(&self) -> u64 {
        self.max_size
    }

    /// The haunter period this cache would use (`consts.DefaultCacheCleanUpInterval`).
    #[must_use]
    pub fn cleanup_interval(&self) -> Duration {
        self.cleanup_interval
    }

    /// Map a key to its data path (`spreadFS.KeyMapper`).
    #[must_use]
    pub fn data_path(&self, key: &str) -> PathBuf {
        key_mapper(&self.root, key)
    }

    /// In-flight entry for `key`, if one is being produced.
    #[must_use]
    pub fn in_flight(&self, key: &str) -> Option<Arc<Entry>> {
        self.entries
            .lock()
            .expect("cache entries")
            .get(key)
            .cloned()
    }

    /// `fileCache.invalidate` (`file_caches.go:124-150`) plus `spreadFS.Remove`
    /// (`spread_fs.go:159-165`): drop the entry and remove its file **and** marker.
    pub async fn invalidate(&self, key: &str) {
        self.drop_entry(key);
        let data_path = self.data_path(key);
        tokio::fs::remove_file(marker_path(&data_path)).await.ok();
        tokio::fs::remove_file(&data_path).await.ok();
    }

    fn drop_entry(&self, key: &str) -> Option<Arc<Entry>> {
        self.entries.lock().expect("cache entries").remove(key)
    }

    /// Resolve `key` the way `fscache.Cache.Get` and the marker check do.
    ///
    /// Order mirrors Go: a marked-complete entry hands a seekable reader; an
    /// in-flight entry hands a non-seekable one with `Cached = true`; otherwise the
    /// caller becomes the writer. A data file **without** a marker that is not in
    /// flight is a crash partial and is discarded (`spread_fs.go:60-64`) rather than
    /// served truncated; a marker whose data file is gone is invalidated and retried,
    /// which is Go's "Cache entry lost its data file. Re-fetching" arm
    /// (`file_caches.go:161-168`).
    pub async fn open_key(&self, key: &str) -> io::Result<Opened> {
        if self.disabled {
            // Go's `size == 0` arm leaves the cache nil (`file_caches.go:275-278`);
            // there is no entry to open, and the caller reads from its reader
            // instead (`:145-152`).
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "transcoding cache is disabled",
            ));
        }
        let data_path = self.data_path(key);
        let marker = marker_path(&data_path);

        if tokio::fs::try_exists(&marker).await? {
            match tokio::fs::metadata(&data_path).await {
                Ok(md) => {
                    return Ok(Opened::Complete {
                        data_path,
                        size: md.len(),
                    })
                }
                Err(_) => {
                    // Marker without data: drop both and re-produce.
                    tokio::fs::remove_file(&marker).await.ok();
                    self.drop_entry(key);
                }
            }
        }
        if let Some(entry) = self.in_flight(key) {
            let item = self.attach(entry, true).await?;
            return Ok(Opened::Attached { item });
        }
        if tokio::fs::try_exists(&data_path).await? {
            // Unmarked and not in flight: a partial from a crash or an interrupted
            // drain. Go logs "Removing incomplete cache file".
            tokio::fs::remove_file(&data_path).await.ok();
        }
        if let Some(parent) = data_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // `O_CREAT|O_EXCL`: a second writer for the same key cannot slip in between
        // the in-flight check and the map insert. Go gets the same exclusivity from
        // fscache holding the entry in memory.
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&data_path)
            .await
        {
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // Lost the race: adopt the other producer's entry, or clear the
                // stale file and create it ourselves.
                if let Some(entry) = self.in_flight(key) {
                    let item = self.attach(entry, true).await?;
                    return Ok(Opened::Attached { item });
                }
                tokio::fs::remove_file(&data_path).await.ok();
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&data_path)
                    .await?;
                Ok(self.insert_writer(key, data_path, marker))
            }
            Err(err) => Err(err),
            Ok(_) => Ok(self.insert_writer(key, data_path, marker)),
        }
    }

    /// Register a fresh writer entry. The entry itself counts as a lease, so
    /// [`Self::scrub`] cannot reap a file mid-write; that count is released when the
    /// entry leaves the in-flight map.
    fn insert_writer(&self, key: &str, data_path: PathBuf, marker_path: PathBuf) -> Opened {
        let entry = Arc::new(Entry {
            key: key.to_string(),
            data_path,
            marker_path,
            state: Mutex::new(State {
                written: 0,
                status: Status::Writing,
            }),
            notify: Notify::new(),
            in_use: AtomicUsize::new(1),
            pid: Mutex::new(None),
        });
        self.entries
            .lock()
            .expect("cache entries")
            .insert(key.to_string(), Arc::clone(&entry));
        Opened::Writer { entry }
    }

    /// Open a reader on a live entry: its own handle, cursor at 0.
    ///
    /// `cached` is Go's flag: `true` for a joiner, `false` for the writer-side lease
    /// (`file_caches.go:169`). Seekability follows the entry state, exactly as
    /// `getFinalCachedSize` decides (`:244-253`).
    async fn attach(&self, entry: Arc<Entry>, cached: bool) -> io::Result<CacheItem> {
        let file = tokio::fs::File::open(&entry.data_path).await?;
        entry.in_use.fetch_add(1, Ordering::AcqRel);
        let seekable = entry.is_complete();
        Ok(CacheItem {
            entry,
            file: Some(file),
            cursor: 0,
            cached,
            seekable,
            protect: None,
        })
    }

    /// Open a reader on a complete entry, with the in-use protection its lease needs.
    pub async fn open_complete(
        self: &Arc<Self>,
        data_path: PathBuf,
        size: u64,
    ) -> io::Result<CacheItem> {
        let file = tokio::fs::File::open(&data_path).await?;
        let protect = self.acquire_lease(&data_path);
        Ok(CacheItem {
            entry: Arc::new(Entry {
                key: String::new(),
                marker_path: marker_path(&data_path),
                data_path,
                state: Mutex::new(State {
                    written: size,
                    status: Status::Complete { size },
                }),
                notify: Notify::new(),
                in_use: AtomicUsize::new(1),
                pid: Mutex::new(None),
            }),
            file: Some(file),
            cursor: 0,
            cached: true,
            seekable: true,
            protect: Some(protect),
        })
    }

    fn acquire_lease(self: &Arc<Self>, data_path: &Path) -> LeaseGuard {
        *self
            .open_leases
            .lock()
            .expect("cache open leases")
            .entry(data_path.to_path_buf())
            .or_default() += 1;
        LeaseGuard {
            cache: Arc::clone(self),
            path: data_path.to_path_buf(),
        }
    }

    fn release_lease(&self, data_path: &Path) {
        let mut guard = self.open_leases.lock().expect("cache open leases");
        if let Some(count) = guard.get_mut(data_path) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                guard.remove(data_path);
            }
        }
    }

    /// Whether a data file is being written or read right now.
    fn in_use_path(&self, data_path: &Path) -> bool {
        if self
            .entries
            .lock()
            .expect("cache entries")
            .values()
            .any(|entry| entry.data_path == data_path)
        {
            return true;
        }
        self.open_leases
            .lock()
            .expect("cache open leases")
            .contains_key(data_path)
    }

    /// `spreadFS.MarkComplete` (`spread_fs.go:148-155`): the marker is what
    /// distinguishes a complete entry from a crash partial.
    async fn mark_complete(&self, entry: &Entry) -> io::Result<()> {
        // Creating and dropping the file is the marker: Go writes no content
        // (`O_CREATE|O_WRONLY` then `Close`).
        // `truncate(false)`: the marker is empty content, and an existing one must
        // not be emptied by re-marking.
        tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&entry.marker_path)
            .await?;
        Ok(())
    }

    /// Finalize a finished drain: complete + marker, or fail with the cause and
    /// invalidate. Either way the entry leaves the in-flight map and [`Self::scrub`]
    /// runs, since Go's haunter is triggered by entry close as well as by its period.
    ///
    /// `result` is `Ok` for a full copy — including a 0-byte one, which Go marks
    /// complete and the haunter then removes as an empty file.
    pub async fn finalize(&self, entry: Arc<Entry>, result: Result<(), String>) {
        match result {
            Ok(()) => {
                let size = entry.written();
                {
                    let mut state = entry.state.lock().expect("cache entry state");
                    state.status = Status::Complete { size };
                }
                if let Err(err) = self.mark_complete(&entry).await {
                    // Go logs the marker failure and keeps the entry
                    // (`file_caches.go:283-291`). Without a marker the file cannot be
                    // adopted later, so treat it as failed rather than promise
                    // adoption: readers keep what they already drained, and the next
                    // `Get` re-produces.
                    Self::fail_in_memory(&entry, format!("writing completion marker: {err}"));
                    entry.notify.notify_waiters();
                    self.drop_entry(entry.key.as_str());
                    return;
                }
                entry.notify.notify_waiters();
            }
            Err(cause) => {
                Self::fail_in_memory(&entry, cause);
                entry.notify.notify_waiters();
                // Invalidate after waking readers, matching Go's order: `copyAndClose`
                // returns the error, then `Get`'s goroutine invalidates
                // (`file_caches.go:181-186`). The fd a reader already holds keeps
                // working, so readers drain the partial bytes before failing.
                tokio::fs::remove_file(marker_path(&entry.data_path))
                    .await
                    .ok();
                tokio::fs::remove_file(&entry.data_path).await.ok();
            }
        }
        self.drop_entry(entry.key.as_str());
        self.scrub().await;
    }

    /// Go's `CloseWithError` state (`file_caches.go:249-258`): readers fail with the
    /// cause after draining. **Never** writes a marker
    /// (`file_caches_test.go:309-326`).
    fn fail_in_memory(entry: &Entry, cause: String) {
        let mut state = entry.state.lock().expect("cache entry state");
        state.status = Status::Failed { cause };
    }

    /// `fileHaunter.Scrub` (`file_haunter.go:37-110`), run explicitly.
    ///
    /// Go's rule, reproduced: remove empty (invalid) files unconditionally, skip
    /// entries in use, sort the rest by **access time ascending** and remove the
    /// oldest until the count is within `max_items` and the size is within
    /// `max_size`. `0` means no constraint on that axis (`file_haunter.go:95-107`).
    pub async fn scrub(&self) -> ScrubReport {
        let mut report = ScrubReport::default();
        if self.disabled {
            return report;
        }

        let mut candidates: Vec<ScrubCandidate> = Vec::new();
        let mut count: usize = 0;
        let mut size: u64 = 0;

        // Go enumerates the cache's entries; the port walks the spread layout
        // `root/xx/xx/<40hex>` directly (`spread_fs.go:92-121` walks it the same way).
        let mut dirs = match tokio::fs::read_dir(&self.root).await {
            Ok(dirs) => dirs,
            Err(_) => return report,
        };
        while let Ok(Some(dir)) = dirs.next_entry().await {
            if !dir.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let mut level2 = match tokio::fs::read_dir(dir.path()).await {
                Ok(files) => files,
                Err(_) => continue,
            };
            while let Ok(Some(sub)) = level2.next_entry().await {
                if !sub.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let mut files = match tokio::fs::read_dir(sub.path()).await {
                    Ok(files) => files,
                    Err(_) => continue,
                };
                while let Ok(Some(file)) = files.next_entry().await {
                    let path = file.path();
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };

                    if let Some(data_name) = name.strip_suffix(COMPLETE_MARKER_SUFFIX) {
                        // Orphan marker: its data file is gone (`spread_fs.go:105-112`).
                        if !is_data_file_name(data_name) {
                            continue;
                        }
                        if !tokio::fs::try_exists(sub.path().join(data_name))
                            .await
                            .unwrap_or(false)
                            && tokio::fs::remove_file(&path).await.is_ok()
                        {
                            report.reaped.push(path);
                        }
                        continue;
                    }
                    if !is_data_file_name(name) {
                        continue;
                    }
                    if self.in_use_path(&path) {
                        continue;
                    }
                    let Ok(md) = tokio::fs::metadata(&path).await else {
                        continue;
                    };
                    if md.len() == 0 {
                        // "Removing invalid empty file" (`file_haunter.go:53-56`), and
                        // `spreadFS.Remove` takes the marker with it.
                        if tokio::fs::remove_file(&path).await.is_ok() {
                            tokio::fs::remove_file(marker_path(&path)).await.ok();
                            report.reaped.push(path);
                        }
                        continue;
                    }
                    count += 1;
                    size += md.len();
                    candidates.push(ScrubCandidate {
                        path,
                        size: md.len(),
                        accessed: md.accessed().ok(),
                    });
                }
            }
        }

        // Go sorts by access time ascending (`file_haunter.go:74-81`). A file whose
        // atime cannot be read is reaped last rather than first, since an unknown
        // timestamp must not outrank a real one.
        candidates.sort_by(|a, b| match (a.accessed, b.accessed) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        let mut index = 0usize;
        if self.max_items > 0 {
            while count > self.max_items {
                match self
                    .reap_candidate(&mut candidates, &mut index, &mut report)
                    .await
                {
                    Some(candidate) => {
                        count -= 1;
                        size = size.saturating_sub(candidate);
                    }
                    None => break,
                }
            }
        }
        if self.max_size > 0 {
            while size > self.max_size {
                match self
                    .reap_candidate(&mut candidates, &mut index, &mut report)
                    .await
                {
                    Some(candidate) => {
                        count -= 1;
                        size = size.saturating_sub(candidate);
                    }
                    None => break,
                }
            }
        }
        report.remaining_items = count;
        report.remaining_size = size;
        report
    }

    /// `fileHaunter.removeFirst` (`file_haunter.go:112-122`): take the oldest
    /// candidate and remove its file and marker.
    async fn reap_candidate(
        &self,
        candidates: &mut [ScrubCandidate],
        index: &mut usize,
        report: &mut ScrubReport,
    ) -> Option<u64> {
        if *index >= candidates.len() {
            return None;
        }
        let candidate = candidates[*index].clone();
        *index += 1;
        if tokio::fs::remove_file(&candidate.path).await.is_ok() {
            tokio::fs::remove_file(marker_path(&candidate.path))
                .await
                .ok();
            report.reaped.push(candidate.path);
            Some(candidate.size)
        } else {
            // A removal that failed says nothing about the remaining budget, so
            // stop rather than loop: Go's `collectKeysToReapFn` breaks on an error
            // instead of skipping ahead (`file_haunter.go:83-93`).
            None
        }
    }
}

#[derive(Debug, Clone)]
struct ScrubCandidate {
    path: PathBuf,
    size: u64,
    accessed: Option<std::time::SystemTime>,
}

/// Reader state machine behind [`CacheItem`].
impl CacheItem {
    /// Go's `CachedStream.Cached`.
    #[must_use]
    pub fn cached(&self) -> bool {
        self.cached
    }

    /// Whether the entry is complete and seekable (`getFinalCachedSize`).
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.seekable
    }

    /// Pid of the shared producer, when the entry is being produced.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.entry.pid()
    }

    /// Cached file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.entry.data_path
    }

    /// Bytes this lease has consumed.
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Total bytes written to the entry so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.entry.written()
    }

    /// Whether the entry is complete on disk (or in memory for this lease).
    #[must_use]
    pub fn complete(&self) -> bool {
        self.entry.is_complete()
    }

    /// Final size of a complete entry ([`Entry::final_size`]).
    #[must_use]
    pub fn final_size(&self) -> Option<u64> {
        self.entry.final_size()
    }

    /// Read the next chunk, waiting for the producer as needed.
    ///
    /// `Ok(None)` is a clean EOF: the entry is complete and fully drained. `Err`
    /// carries the producer's cause once the bytes already written have been handed
    /// out, so a reader sees "transcoder died" rather than a truncated clean EOF
    /// (`file_caches_test.go:277-308`).
    pub async fn read_next(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        loop {
            if self.file.is_none() {
                return Err(io::Error::other("cached transcode lease is closed"));
            }
            let (available, failure, complete) = {
                let state = self.entry.state.lock().expect("cache entry state");
                let failure = match &state.status {
                    Status::Failed { cause } => Some(cause.clone()),
                    _ => None,
                };
                (
                    state.written,
                    failure,
                    matches!(state.status, Status::Complete { .. }),
                )
            };

            if available > self.cursor {
                let want = usize::try_from(available - self.cursor)
                    .unwrap_or(usize::MAX)
                    .min(max.max(1));
                let mut buf = vec![0u8; want];
                let read = self
                    .file
                    .as_mut()
                    .expect("checked above")
                    .read(&mut buf[..want])
                    .await?;
                self.cursor += read as u64;
                if read > 0 {
                    buf.truncate(read);
                    return Ok(Some(buf));
                }
                // 0 bytes with `written > cursor` means the file shrank underneath
                // us (invalidated mid-read). Go's CacheReader keeps following the
                // file, so wait rather than claiming EOF.
                if let Some(cause) = failure {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, cause));
                }
                continue;
            }
            if let Some(cause) = failure {
                // Drained everything that was written: now surface the cause.
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, cause));
            }
            if complete {
                return Ok(None);
            }

            // Register before re-checking so a notification between the snapshot and
            // the await cannot be lost (tokio's documented `Notified::enable` pattern).
            // `Notified` is `!Unpin`, so it is pinned in place for this iteration.
            let notified = self.entry.notify.notified();
            tokio::pin!(notified);
            if notified.as_mut().enable() {
                // Someone woke us between the snapshot and registering: re-check.
                continue;
            }
            notified.await;
        }
    }
}

impl CachedTranscode for CacheItem {
    fn seekable(&self) -> bool {
        self.seekable
    }

    fn path(&self) -> Option<&Path> {
        // Go hands back a `SectionReader` over the file only once the entry is
        // complete (`file_caches.go:196-205`); the handler's seekable arm re-plans
        // through `crate::direct::plan_and_open`, so Range/conditional semantics are
        // the same as a raw file.
        self.seekable.then(|| self.entry.data_path.as_path())
    }

    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move { self.read_next(max).await })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.file = None;
            Ok(())
        })
    }
}

impl Drop for CacheItem {
    fn drop(&mut self) {
        self.entry.in_use.fetch_sub(1, Ordering::AcqRel);
        self.protect.take();
    }
}

/// Writer-side sink: append ffmpeg's stdout to the entry file and wake readers after
/// each chunk (`io.Copy` into the cache file, `file_caches.go:236`).
struct CacheSink {
    entry: Arc<Entry>,
    file: tokio::fs::File,
}

impl AsyncWrite for CacheSink {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // `CacheSink` is `Unpin` (both fields are), so the pin is only the trait's
        // shape; the file handle is projected for the syscall.
        let me = self.get_mut();
        match Pin::new(&mut me.file).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    // Published after the write completed, so a woken reader always
                    // finds the bytes it was told about.
                    me.entry.state.lock().expect("cache entry state").written += n as u64;
                    me.entry.notify.notify_waiters();
                }
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_shutdown(cx)
    }
}

/// The read-only **observer** of a client-disconnect future.
///
/// # Why this exists (declared seam deviation)
///
/// Go's writer arm has no separate disconnect channel: the producer runs inside the
/// first `Get` holding that request's context (`media_streamer.go:122-125`,
/// `:243-260`) and the handler pumps the body unconditionally. This port's handler
/// cannot: its `Cached` arm pumps only when a signal is present (`handler.rs:1364`),
/// and `TranscodeLease::Cached` reports no pid (`:907`), so a writer lease with
/// `signal: None` answers 200 with **0 bytes**. The writer lease therefore hands back
/// this observer, which is **not** the caller's future:
///
///   - exactly **one** task owns and polls the caller's `D` ([`Self::spawn`]);
///   - that task feeds a `watch` channel which the gate polls as its
///     `Disconnect::When`, so the **first requester's** disconnect still cancels
///     ffmpeg inside the gate — Go's parity requirement;
///   - the lease hands a *clone* of the receiver to the handler only so its pump runs
///     and races writes. Cloning a `watch::Receiver` cannot poll the caller's future,
///     so there is no double-poll and no second kill path;
///   - attached readers report their **own** signal, so a late reader hanging up stops
///     its own read and cannot kill the shared producer.
///
/// # Lifetime
///
/// The poller task returns when `D` resolves **or** when the transcode teardown is
/// announced ([`Self::finish`]), so it cannot outlive the serve. [`live_pollers`]
/// reports how many are running — how the suite asserts nothing leaked.
pub struct SharedDisconnect {
    /// Set by the poller task when the client is gone (or by [`Self::never`]'s
    /// channel, which is never written).
    cancelled: Arc<AtomicBool>,
    /// Wakes every observer when `cancelled` is set.
    wake: Arc<Notify>,
    /// The observer's own parked future, kept so the waker registered on the first
    /// poll is not discarded on the next one. `OwnedNotified` is `'static`, so it can
    /// be stored without borrowing the observer.
    parked: Option<Pin<Box<tokio::sync::futures::OwnedNotified>>>,
    /// Announced by the drain task once the transcode is torn down.
    finished: Arc<watch::Sender<bool>>,
    /// Set by the poller task as its last act, so an owner can prove its own task
    /// exited. `true` for observers that never spawned one ([`Self::never`]).
    poller_exited: Arc<AtomicBool>,
}

impl SharedDisconnect {
    /// A fresh, unresolved observer with no poller task.
    fn unresolved() -> Self {
        let (tx, _rx) = watch::channel(false);
        SharedDisconnect {
            cancelled: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Notify::new()),
            parked: None,
            finished: Arc::new(tx),
            // No task was spawned, so nothing can leak.
            poller_exited: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Clone for SharedDisconnect {
    /// A new observer on the **same** signal: `cancelled` and `wake` are shared, the
    /// parked future is per-observer. Cloning cannot poll the caller's future, which
    /// is why handing clones to the handler creates no second kill path.
    fn clone(&self) -> Self {
        SharedDisconnect {
            cancelled: Arc::clone(&self.cancelled),
            wake: Arc::clone(&self.wake),
            parked: None,
            finished: Arc::clone(&self.finished),
            poller_exited: Arc::clone(&self.poller_exited),
        }
    }
}

impl Future for SharedDisconnect {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        // Park on the shared `Notify`, keeping the future across polls so the
        // registration survives. Re-checking `cancelled` on every poll makes a lost
        // wake-up impossible even though `notify_waiters` does not store a permit.
        // Cloned first: the `Notify` handle is cheap and holds no borrow of `self`,
        // so the parked future never borrows the observer.
        if self.parked.is_none() {
            self.parked = Some(Box::pin(self.wake.clone().notified_owned()));
        }
        let parked = self
            .parked
            .as_mut()
            .expect("parked future was just inserted");
        match parked.as_mut().poll(cx) {
            Poll::Ready(()) => {
                // A wake-up means the flag was set; confirm rather than assume.
                if self.cancelled.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    self.parked = None;
                    Poll::Pending
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Live [`SharedDisconnect`] poller tasks. Test hook for the leak assertion: it must
/// be `0` once every serve has finished.
#[must_use]
pub fn live_pollers() -> usize {
    LIVE_POLLERS.load(Ordering::Acquire)
}

static LIVE_POLLERS: AtomicUsize = AtomicUsize::new(0);

impl SharedDisconnect {
    /// Spawn the single poller of `D` and return the observer the gate and the
    /// handler pump share. Must be called from within a tokio runtime
    /// (`TranscodeSource::open` is async, so it always is).
    #[must_use]
    pub fn spawn<D>(disconnect: D) -> Self
    where
        D: Future<Output = ()> + Send + 'static,
    {
        let (finish_tx, finish_rx) = watch::channel(false);
        let mut observer = SharedDisconnect::unresolved();
        observer.finished = Arc::new(finish_tx);
        let cancelled = Arc::clone(&observer.cancelled);
        let wake = Arc::clone(&observer.wake);
        observer.poller_exited = Arc::new(AtomicBool::new(false));
        let exited = Arc::clone(&observer.poller_exited);
        LIVE_POLLERS.fetch_add(1, Ordering::AcqRel);
        tokio::spawn(async move {
            tokio::pin!(disconnect);
            let mut finish_rx = finish_rx;
            tokio::select! {
                biased;
                () = &mut disconnect => {
                    cancelled.store(true, Ordering::Release);
                    wake.notify_waiters();
                }
                // The transcode is gone: nothing left to observe.
                Ok(()) = finish_rx.changed() => {}
            }
            LIVE_POLLERS.fetch_sub(1, Ordering::AcqRel);
            exited.store(true, Ordering::Release);
        });
        observer
    }

    /// An observer that can never resolve: Go's uncancelled legacy read
    /// (`media_streamer.go:256`, `EnableCancellation = false`). It spawns no task, so
    /// it does not affect [`live_pollers`], and `finish` deliberately does nothing for
    /// it — there is nothing waiting.
    #[must_use]
    pub fn never() -> Self {
        SharedDisconnect::unresolved()
    }

    /// Announce that the transcode is torn down so the poller task returns.
    /// Idempotent.
    pub fn finish(&self) {
        let _ = self.finished.send(true);
    }

    /// Whether this observer's poller task has finished: the precise leak check, the
    /// one a test can assert without depending on other lanes' tasks.
    #[must_use]
    pub fn poller_exited(&self) -> bool {
        self.poller_exited.load(Ordering::Acquire)
    }

    /// Whether the client has gone away.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// The cache-backed transcode source: Go's `ms.cache.Get` producer
/// (`media_streamer.go:224-282`) behind [`TranscodeSource`].
///
/// One `Get` per request: the first for a key reserves the limiter slot and launches
/// ffmpeg, every later one attaches to the running entry. A rejection returns
/// [`GateError::Rejected`] untouched — that is how the 429 survives without being
/// treated as a cache failure (`media_streamer.go:114-117`).
pub struct CacheTranscodeSource {
    cache: Arc<TranscodeCache>,
    limiter: TranscodeLimiter,
    /// Whether concurrent identical keys share one entry. Disabled **only** so a test
    /// can prove its own non-vacuity: with it off, two identical requests launch two
    /// processes and the single-flight assertion fails. Same role as
    /// `transcode_body::GatedTranscode::release_slot_early`.
    dedupe: bool,
}

impl CacheTranscodeSource {
    /// Wire a cache and the concurrency caps together.
    #[must_use]
    pub fn new(cache: Arc<TranscodeCache>, limiter: TranscodeLimiter) -> Self {
        CacheTranscodeSource {
            cache,
            limiter,
            dedupe: true,
        }
    }

    /// Turn single-flight off. Production callers must never do this: it means one
    /// request per ffmpeg process, exactly the behavior the cache exists to remove.
    #[must_use]
    pub fn with_dedupe(mut self, enabled: bool) -> Self {
        self.dedupe = enabled;
        self
    }

    /// The cache behind this source.
    #[must_use]
    pub fn cache(&self) -> &Arc<TranscodeCache> {
        &self.cache
    }

    /// Cache-level `Get`: resolve `key` into a lease carrying Go's `Cached` flag,
    /// launching nothing. The caller of a `Writer` lease must produce into the entry
    /// (the handler path does it in [`Self::produce`]).
    pub async fn get(&self, key: &str) -> io::Result<CacheLease> {
        match self.cache.open_key(key).await? {
            Opened::Complete { data_path, size } => {
                let item = self.cache.open_complete(data_path, size).await?;
                Ok(CacheLease {
                    pid: None,
                    cached: true,
                    item,
                })
            }
            Opened::Attached { item } => {
                let pid = item.pid();
                Ok(CacheLease {
                    pid,
                    cached: true,
                    item,
                })
            }
            Opened::Writer { entry } => {
                let pid = entry.pid();
                let item = self.cache.attach(entry, false).await?;
                Ok(CacheLease {
                    pid,
                    cached: false,
                    item,
                })
            }
        }
    }

    /// Reserve the slot, launch ffmpeg, and drain it into the entry in the background
    /// (Go's `go fc.copyAndClose`, `file_caches.go:180-189`).
    ///
    /// The spawned task owns the [`GatedTranscode`], so the limiter slot is freed only
    /// by its confirmed reap at copy end. A launch failure drops the entry and removes
    /// its file — Go's `r.Close(); w.Close(); invalidate` arm
    /// (`file_caches.go:171-176`) — and the slot is already released by
    /// `open_transcode_gated` in that arm (`media_streamer.go:273-277`).
    async fn produce(
        &self,
        entry: Arc<Entry>,
        mf: &MediaFile,
        job: &TranscodeJob,
        user: &str,
        observer: &SharedDisconnect,
    ) -> Result<(), GateError> {
        let gate = match open_transcode_gated(
            &self.limiter,
            user,
            mf,
            job,
            Disconnect::When(observer.clone()),
        )
        .await
        {
            Ok(gate) => gate,
            Err(err) => {
                observer.finish();
                self.cache.drop_entry(entry.key.as_str());
                tokio::fs::remove_file(&entry.marker_path).await.ok();
                tokio::fs::remove_file(&entry.data_path).await.ok();
                return Err(err);
            }
        };
        entry.set_pid(gate.pid());
        tokio::spawn(run_producer(
            Arc::clone(&self.cache),
            entry,
            gate,
            observer.clone(),
        ));
        Ok(())
    }
}

/// Join a second failure into the first, keeping the original cause first.
fn join_cause(cause: Option<String>, label: &str, message: &Option<String>) -> String {
    let message = message
        .as_deref()
        .unwrap_or("cleanup did not confirm the reap")
        .to_string();
    match cause {
        Some(previous) => format!("{previous}; {label}: {message}"),
        None => format!("{label}: {message}"),
    }
}

/// Drive ffmpeg into the entry file, then finalize.
///
/// This is `copyAndClose` (`file_caches.go:236-262`): copy, close the source (here:
/// reap the process, which is this port's release point for the limiter slot), then
/// mark complete **or** record the cause and invalidate.
async fn run_producer(
    cache: Arc<TranscodeCache>,
    entry: Arc<Entry>,
    mut gate: GatedTranscode<SharedDisconnect>,
    observer: SharedDisconnect,
) {
    let file = match tokio::fs::OpenOptions::new()
        .append(true)
        .open(&entry.data_path)
        .await
    {
        Ok(file) => file,
        Err(err) => {
            observer.finish();
            cache
                .finalize(entry, Err(format!("opening cache file: {err}")))
                .await;
            return;
        }
    };
    let mut sink = CacheSink {
        entry: Arc::clone(&entry),
        file,
    };

    // `serve` drives stdout into the sink until EOF, the disconnect resolves (the
    // first requester hung up: the group is killed inside the gate), or a write fails.
    // Teardown happens here so the slot is freed by a confirmed reap and no reader has
    // to wait for it.
    let outcome = gate.serve(&mut sink).await;
    let kill = outcome.disconnected || outcome.truncated || outcome.error.is_some();
    let teardown = if kill {
        gate.cancel().await
    } else {
        gate.close().await
    };

    // A killed ffmpeg is a failed cache write, as in Go: the request context dies,
    // `ffCmd.wait` pushes the non-zero exit into the pipe (`ffmpeg.go:384-393`),
    // `io.Copy` returns that error, and `copyAndClose` invalidates the entry rather
    // than adopting a truncated file.
    let mut cause = outcome.error.clone();
    if outcome.disconnected && cause.is_none() {
        cause = Some("client disconnected before the transcode completed".to_string());
    }
    if outcome.cleanup_pending {
        cause = Some(join_cause(
            cause,
            "cleanup from the write loop",
            &outcome.cleanup_error,
        ));
    }
    match teardown {
        // The reap was confirmed: the slot is released and nothing more is owed.
        Ok(_) => {}
        Err(err) => {
            // Unconfirmed reap: the slot stays held (fail-closed) and the entry is
            // incomplete, so it must not be adopted.
            cause = Some(join_cause(cause, "cleanup", &Some(err.to_string())));
        }
    }

    // Release the sink's handle, then stop observing the client: the producer is gone,
    // so the poller task has nothing left to watch.
    drop(sink);
    observer.finish();
    cache.finalize(entry, cause.map_or(Ok(()), Err)).await;
}

/// A cache-backed transcode source for a handler over
/// `StreamHandler<SharedDisconnect>` (Go's `ms.cache.Get` producer,
/// `media_streamer.go:224-282`, behind [`TranscodeSource`]).
///
/// `D` is [`SharedDisconnect`] rather than a raw disconnect future: the cache needs
/// one cancellation signal shared by the gate and every reader, and Go carries
/// exactly that as the single request context inside the stream
/// (`media_streamer.go:66-67`, `:243-260`). A caller with its own future turns it into
/// one with [`SharedDisconnect::spawn`] — that is the one task that polls it — and
/// hands the result to [`crate::handler::StreamHandler::serve`].
///
/// One `Get` per request: the first for a key reserves the limiter slot and launches
/// ffmpeg, every later one attaches to the running entry. A rejection returns
/// [`GateError::Rejected`] untouched, which is how the 429 survives without being
/// treated as a cache failure (`media_streamer.go:114-117`).
impl TranscodeSource<SharedDisconnect> for CacheTranscodeSource {
    fn open<'a>(
        &'a mut self,
        mf: &'a MediaFile,
        job: &'a TranscodeJob,
        user: &'a str,
        disconnect: Disconnect<SharedDisconnect>,
    ) -> BoxFuture<'a, Result<TranscodeLease<SharedDisconnect>, GateError>> {
        Box::pin(async move {
            if self.cache.disabled() || !self.dedupe {
                // Go's disabled-cache arm reads straight from the reader
                // (`file_caches.go:145-152`); `dedupe: false` is the test-only
                // non-vacuity switch that lands here deliberately.
                let gate = open_transcode_gated(&self.limiter, user, mf, job, disconnect).await?;
                return Ok(TranscodeLease::Process(Box::new(gate)));
            }

            let key = stream_cache_key(mf, job);
            let observer = match disconnect {
                Disconnect::When(observer) => observer,
                Disconnect::Never => SharedDisconnect::never(),
            };

            let open = self.cache.open_key(&key).await.map_err(|err| {
                observer.finish();
                GateError::Body(TranscodeBodyError::Process {
                    message: format!("transcoding cache: {err}"),
                })
            })?;

            match open {
                Opened::Writer { entry } => {
                    self.produce(Arc::clone(&entry), mf, job, user, &observer)
                        .await?;
                    let item = self.cache.attach(entry, false).await.map_err(|err| {
                        observer.finish();
                        GateError::Body(TranscodeBodyError::Process {
                            message: format!("transcoding cache: {err}"),
                        })
                    })?;
                    Ok(TranscodeLease::Cached {
                        body: Box::new(item),
                        // A read-only *clone* of the observer: the handler's pump can
                        // race its writes, and polling it cannot poll the caller's
                        // future or kill anything. See the module doc's seam deviation.
                        signal: Some(observer),
                    })
                }
                Opened::Attached { item } => Ok(TranscodeLease::Cached {
                    body: Box::new(item),
                    // The observer, not a producer-owning signal: this reader hanging
                    // up stops its own read and cannot kill the shared producer. The
                    // poller task ends when the producer announces teardown.
                    signal: Some(observer),
                }),
                Opened::Complete { data_path, size } => {
                    let item = self
                        .cache
                        .open_complete(data_path, size)
                        .await
                        .map_err(|err| {
                            observer.finish();
                            GateError::Body(TranscodeBodyError::Process {
                                message: format!("transcoding cache: {err}"),
                            })
                        })?;
                    // No producer exists, so nothing else will announce teardown.
                    observer.finish();
                    Ok(TranscodeLease::Cached {
                        body: Box::new(item),
                        signal: Some(observer),
                    })
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_file(id: &str, updated_at: i64) -> MediaFile {
        MediaFile {
            id: id.to_string(),
            updated_at,
            ..MediaFile::default()
        }
    }

    #[test]
    fn rfc3339_utc_matches_go_format() {
        // Go oracle: `time.Unix(secs,0).Format(time.RFC3339Nano)` under TZ=UTC.
        for (secs, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (1_700_000_000, "2023-11-14T22:13:20Z"),
            (-1, "1969-12-31T23:59:59Z"),
            (2_147_483_647, "2038-01-19T03:14:07Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
        ] {
            assert_eq!(rfc3339_utc(secs), expected, "secs {secs}");
        }
    }

    #[test]
    fn key_mapper_is_go_spread_layout() {
        let root = Path::new("/cache/transcoding");
        // Go oracle: sha1 of the key, split 2/2/40.
        let key = "mf-1.2023-11-14T22:13:20Z.128.0.0.0.mp3.0";
        let path = key_mapper(root, key);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), KEY_HASH_HEX_LEN);
        assert!(is_data_file_name(name));
        assert_eq!(path.parent().unwrap().file_name().unwrap(), &name[2..4]);
        assert_eq!(
            path.parent().unwrap().parent().unwrap(),
            root.join(&name[0..2])
        );

        // Idempotence arm (`spread_fs.go:178-181`): a mapped path maps to itself.
        assert_eq!(key_mapper(root, &path.display().to_string()), path);
    }

    #[test]
    fn cache_key_matches_go_stream_job_fields_in_order() {
        let mf = media_file("mf-1", 1_700_000_000);
        let job = TranscodeJob {
            format: "mp3".to_string(),
            bit_rate: 128,
            sample_rate: 44_100,
            bit_depth: 16,
            channels: 2,
            offset: 7,
            ..TranscodeJob::default()
        };
        assert_eq!(
            stream_cache_key(&mf, &job),
            "mf-1.2023-11-14T22:13:20Z.128.44100.16.2.mp3.7"
        );
    }

    #[tokio::test]
    async fn zero_size_cache_is_disabled() {
        let dir = std::env::temp_dir().join("nd-cache-size0");
        let cache = TranscodeCache::new(&dir, "0", 0, DEFAULT_CACHE_CLEAN_UP_INTERVAL).unwrap();
        assert!(cache.disabled());
        assert!(!cache.available());
        std::fs::remove_dir_all(&dir).ok();
    }
}
