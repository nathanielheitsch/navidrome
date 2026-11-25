//! BOOT-01 stream-state slice: transcoding cache + limiter construction tests.
//!
//! Every case runs the real chain — `preflight` → `startup::initialize` →
//! `StreamState::new` — against an isolated temp tree, so `cache_dir` is the
//! **actual** absolute path startup anchored from the injected startup cwd, and
//! the budget is the **actual** typed `ByteSizeField` from the loaded config.
//!
//! Coverage:
//! * the resolved `cache_dir/transcoding` root is created by construction, and the
//!   configured music root is untouched (nothing inside it, no database there);
//! * the configured budget is observable through the real cache admission/scrub
//!   API — entries are written as complete cache files and `scrub` evicts down to
//!   the configured byte budget — not merely echoed as a scalar;
//! * clones share the one cache (`Arc::ptr_eq`) and the same global and per-user
//!   transcode budgets, proven by holding real permits and observing rejection
//!   through a clone. No subprocess and no sleep for the budget assertions;
//! * `0` caps mean unlimited;
//! * negative and over-`i32::MAX` caps are refused **before** any directory exists;
//! * an unparseable or mismatched typed budget is refused before any directory
//!   exists, and a zero budget is refused explicitly;
//! * failure diagnostics carry only a static class and a numeric errno — no path,
//!   no OS message, no canary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use nd_cli::preflight::{preflight, PreflightOptions};
use nd_cli::startup::initialize;
use nd_cli::stream_state::{StreamState, StreamStateError};
use nd_conf::bytes::ByteSizeField;
use nd_stream::limiter::TooManyTranscodes;
use nd_stream::transcode_cache::{
    marker_path, DEFAULT_CACHE_CLEAN_UP_INTERVAL, DEFAULT_TRANSCODING_CACHE_MAX_ITEMS,
    TRANSCODING_CACHE_DIR,
};

/// The configured transcoding cache budget, spelled as the config would spell it.
const BUDGET_RAW: &str = "10kB";
/// What [`BUDGET_RAW`] parses to (SI kilo = 1000).
const BUDGET_BYTES: u64 = 10_000;

/// An isolated per-test tree.
struct Sandbox {
    inner: tempfile::TempDir,
    cwd: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(&format!("nd-cli-stream-state-{tag}-"))
            .tempdir()
            .expect("isolated temp root");
        let cwd = std::fs::canonicalize(inner.path()).expect("canonicalize temp root");
        Sandbox { inner, cwd }
    }

    fn root(&self) -> &Path {
        self.inner.path()
    }

    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let p = self.root().join(relative);
        std::fs::create_dir_all(&p).expect("create sandbox dir");
        p
    }

    fn file(&self, relative: &str, contents: &str) -> PathBuf {
        let p = self.root().join(relative);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&p, contents).expect("write sandbox file");
        p
    }

    /// Every path under the tree, relative to the root.
    fn entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("entry").path();
                out.push(
                    path.strip_prefix(self.root())
                        .expect("inside root")
                        .to_string_lossy()
                        .into_owned(),
                );
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        out.sort();
        out
    }
}

/// A boot-shaped config with a typed `TranscodingCacheSize`.
fn boot_toml(budget: &str) -> String {
    format!(
        "MusicFolder = \"music\"\nDataFolder = \"data\"\nCacheFolder = \"cache\"\nPort = 4611\n\
         TranscodingCacheSize = \"{budget}\"\n"
    )
}

fn stamp(offset: u32) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("2024-06-01T{:02}:00:00Z", 10 + offset))
        .expect("synthetic stamp")
        .with_timezone(&Utc)
}

/// Real preflight with an injected cwd, empty env, non-root uid.
fn boot_preflight(sandbox: &Sandbox) -> nd_cli::preflight::PreflightOutcome {
    preflight(PreflightOptions {
        config_file: Some("conf.toml"),
        env: Some(Box::new(|_key| None)),
        startup_cwd: Some(sandbox.cwd()),
        effective_uid: Some(1000),
        allow_zero_port: false,
    })
    .expect("preflight of a boot-shaped config")
}

/// Sandbox with a music dir and the given config body.
fn sandbox_with_config(tag: &str, budget: &str) -> Sandbox {
    let sandbox = Sandbox::new(tag);
    sandbox.dir("music");
    sandbox.file("conf.toml", &boot_toml(budget));
    sandbox
}

/// Real startup, then hand back the preflight outcome (for its typed budget) and
/// the initialized state.
async fn boot(
    sandbox: &Sandbox,
    offset: u32,
) -> (
    nd_cli::preflight::PreflightOutcome,
    nd_cli::startup::InitializedState,
) {
    let outcome = boot_preflight(sandbox);
    let state = initialize(&outcome, stamp(offset))
        .await
        .expect("startup of a boot-shaped config");
    (outcome, state)
}

/// Write a complete cache entry (data file + completion marker), the way the
/// existing nd-stream cache tests stage scrub candidates.
fn write_complete(
    cache: &nd_stream::transcode_cache::TranscodeCache,
    key: &str,
    bytes: &[u8],
) -> PathBuf {
    let data = cache.data_path(key);
    std::fs::create_dir_all(data.parent().unwrap()).expect("spread dirs");
    std::fs::write(&data, bytes).expect("data file");
    std::fs::write(marker_path(&data), b"").expect("marker");
    data
}

/// Render an error, its Debug and its whole source chain, for disclosure checks.
fn full_text<E: std::error::Error>(err: &E) -> String {
    let mut text = format!("{err} | debug {err:?}");
    let mut source = <E as std::error::Error>::source(err);
    let mut count = 0;
    while let Some(s) = source {
        text.push_str(&format!(" | source {s} | source-debug {s:?}"));
        count += 1;
        source = s.source();
    }
    let _ = count;
    text
}

#[tokio::test]
async fn resolved_transcoding_subdir_is_created_and_music_root_is_untouched() {
    let sandbox = sandbox_with_config("root", BUDGET_RAW);
    let (outcome, state) = boot(&sandbox, 1).await;
    assert!(
        !state.cache_dir.join(TRANSCODING_CACHE_DIR).exists(),
        "startup itself does not create the transcoding root"
    );

    let stream = StreamState::new(
        &state.cache_dir,
        &outcome.config.transcoding_cache_size,
        0,
        0,
    )
    .await
    .expect("constructed stream state");

    // The existing constant, joined to the resolved cache dir.
    assert_eq!(
        stream.cache.root(),
        state.cache_dir.join(TRANSCODING_CACHE_DIR),
        "cache root is cache_dir/transcoding"
    );
    assert!(stream.cache.root().is_dir(), "root created by construction");
    assert!(
        stream.cache.root().is_absolute(),
        "the root inherits the absolute resolved cache dir"
    );
    assert_eq!(
        stream.cache.max_items(),
        DEFAULT_TRANSCODING_CACHE_MAX_ITEMS
    );
    assert_eq!(
        stream.cache.cleanup_interval(),
        DEFAULT_CACHE_CLEAN_UP_INTERVAL
    );
    assert!(
        !stream.cache.disabled(),
        "a positive budget is not disabled"
    );
    assert!(stream.cache.available());

    // The music root was never created into or used as a cache root.
    let music = sandbox.root().join("music");
    assert_eq!(
        std::fs::read_dir(&music).expect("music dir").count(),
        0,
        "nothing was created inside the configured music root"
    );
    assert!(
        stream.cache.root().starts_with(&state.cache_dir)
            && !stream.cache.root().starts_with(&outcome.music_root),
        "the cache root lives under the cache dir, not the music root"
    );
    // The slice added exactly cache/transcoding; startup owns data/ and the bare
    // dirs. Nothing was created under music/.
    let created = sandbox.entries();
    assert!(
        created.iter().all(|p| {
            p == "conf.toml"
                || p == "music"
                || p == "data"
                || p == "cache"
                || p.starts_with("data/")
                || p.starts_with("cache/transcoding")
        }),
        "the only new artifact is cache/transcoding: {created:?}"
    );
    assert!(
        created.iter().any(|p| p == "cache/transcoding"),
        "the transcoding root is the slice's addition"
    );
}

#[tokio::test]
async fn configured_budget_governs_real_cache_scrub() {
    let sandbox = sandbox_with_config("budget", BUDGET_RAW);
    let (outcome, state) = boot(&sandbox, 2).await;
    let stream = StreamState::new(
        &state.cache_dir,
        &outcome.config.transcoding_cache_size,
        0,
        0,
    )
    .await
    .expect("constructed stream state");

    // The typed budget is applied inside the cache, not just stored on the state.
    assert_eq!(stream.cache_size_bytes, BUDGET_BYTES);
    assert_eq!(
        stream.cache.max_size(),
        BUDGET_BYTES,
        "the cache itself carries the configured byte budget"
    );
    assert_eq!(
        outcome.config.transcoding_cache_size.as_bytes(),
        BUDGET_BYTES,
        "loader parse agrees with the re-derived value"
    );

    // Over the budget: three complete 4000-byte entries total 12000 > 10000.
    let oldest = write_complete(&stream.cache, "oldest", &vec![b'a'; 4000]);
    let middle = write_complete(&stream.cache, "middle", &vec![b'b'; 4000]);
    let newest = write_complete(&stream.cache, "newest", &vec![b'c'; 4000]);

    let report = stream.cache.scrub().await;
    assert!(
        !report.reaped.is_empty(),
        "the scrub admitted the configured budget and evicted"
    );
    assert!(
        report.remaining_size <= BUDGET_BYTES,
        "remaining bytes respect the configured budget: {} vs {BUDGET_BYTES}",
        report.remaining_size
    );
    assert_eq!(report.remaining_items, 2, "one entry was reaped");
    // The oldest-by-atime entry is the one Go's haunter drops first.
    assert!(!oldest.exists(), "the oldest complete entry was reaped");
    assert!(middle.exists() && newest.exists(), "newer entries survive");
}

#[tokio::test]
async fn clones_share_one_cache_and_both_transcode_budgets() {
    let sandbox = sandbox_with_config("budgets", BUDGET_RAW);
    let (outcome, state) = boot(&sandbox, 3).await;
    let global_limit: i32 = 2;
    let user_limit: i32 = 1;
    let stream = StreamState::new(
        &state.cache_dir,
        &outcome.config.transcoding_cache_size,
        i64::from(global_limit),
        i64::from(user_limit),
    )
    .await
    .expect("constructed stream state");

    // Source-level identity: the clone shares the one cache Arc.
    let second = stream.clone();
    assert!(stream.shares_cache(&second), "one Arc<TranscodeCache>");
    assert_eq!(stream.cache.root(), second.cache.root());
    assert_eq!(stream.global_limit, global_limit);
    assert_eq!(stream.user_limit, user_limit);
    assert!(stream.limiter.enabled(), "positive caps are enabled");

    // Per-user cap of 1: the second transcode for the same user is refused, and
    // the refusal is observed through the *clone*, so the counter is shared.
    let first = stream.limiter.acquire("alice").expect("first slot");
    assert!(
        matches!(second.limiter.acquire("alice"), Err(TooManyTranscodes)),
        "the per-user budget is shared across clones"
    );

    // A different user is unaffected until the global cap of 2 is reached.
    let bob = second.limiter.acquire("bob").expect("bob's slot");
    assert!(
        matches!(second.limiter.acquire("carol"), Err(TooManyTranscodes)),
        "the global budget is reached at exactly the configured cap"
    );
    drop(first);
    // Releasing a slot frees the global budget again (still shared).
    let carol = second.limiter.acquire("carol").expect("freed global slot");
    drop(bob);
    drop(carol);
    assert!(stream.limiter.acquire("dave").is_ok(), "slots return");
}

#[tokio::test]
async fn zero_caps_mean_unlimited() {
    let sandbox = sandbox_with_config("zero", BUDGET_RAW);
    let (outcome, state) = boot(&sandbox, 4).await;
    let stream = StreamState::new(
        &state.cache_dir,
        &outcome.config.transcoding_cache_size,
        0,
        0,
    )
    .await
    .expect("0 is unlimited, not a refusal");

    assert!(!stream.limiter.enabled(), "no cap is enabled");
    // Many concurrent permits for one user and many users, all admitted.
    let mut held = Vec::new();
    for i in 0..64 {
        held.push(
            stream
                .clone()
                .limiter
                .acquire(if i % 2 == 0 {
                    "same-user"
                } else {
                    "other-user"
                })
                .unwrap_or_else(|e| panic!("permit {i} must be admitted: {e}")),
        );
    }
    assert_eq!(held.len(), 64);
    drop(held);
}

#[tokio::test]
async fn invalid_caps_are_refused_before_any_directory_exists() {
    let cases = [
        (-1, 5, "negative global"),
        (5, -1, "negative per-user"),
        (i64::from(i32::MAX) + 1, 5, "global above i32::MAX"),
        (5, i64::from(i32::MAX) + 1, "per-user above i32::MAX"),
        (i64::MIN, i64::MIN, "extreme negatives"),
        (i64::MAX, 1, "positive far above the range"),
    ];
    for (global, user, why) in cases {
        let sandbox = sandbox_with_config("bad-cap", BUDGET_RAW);
        let (outcome, state) = boot(&sandbox, 5).await;
        let err = StreamState::new(
            &state.cache_dir,
            &outcome.config.transcoding_cache_size,
            global,
            user,
        )
        .await
        .expect_err(&format!("{why} must be refused"));

        assert!(
            matches!(
                err,
                StreamStateError::NegativeCap | StreamStateError::CapOutOfRange
            ),
            "{why}: static cap class, got {err:?}"
        );
        // No side effect from THIS slice: startup created cache/, but the slice
        // must not have created the transcoding root inside it.
        assert!(
            !state.cache_dir.join(TRANSCODING_CACHE_DIR).exists(),
            "{why}: no transcoding directory was created"
        );
        assert!(
            sandbox
                .entries()
                .iter()
                .all(|p| !p.starts_with("cache/transcoding")),
            "{why}: nothing was created under cache/: {:?}",
            sandbox.entries()
        );
    }
}

#[tokio::test]
async fn unparseable_or_mismatched_budget_is_refused_before_creation() {
    // `ByteSizeField::from_raw` does not validate: an unparseable spelling carries
    // bytes 0 silently. This boundary must catch it.
    let unparseable = ByteSizeField::from_raw("canary-typed-budget");
    assert_eq!(
        unparseable.as_bytes(),
        0,
        "from_raw silently zeroed the unparseable spelling (the bypass this check closes)"
    );

    let cases: Vec<(ByteSizeField, &str)> = vec![
        (unparseable.clone(), "unparseable raw spelling"),
        (ByteSizeField::from_raw(""), "empty spelling"),
        (
            ByteSizeField {
                bytes: 1_000,
                raw: "2MB".to_string(),
            },
            "bytes disagree with the spelling",
        ),
        (
            ByteSizeField {
                bytes: 0,
                raw: "10MB".to_string(),
            },
            "zero reported value against a positive spelling",
        ),
        (ByteSizeField::from_raw("0"), "explicit zero budget"),
        (ByteSizeField::from_raw("0MB"), "zero budget with a unit"),
    ];
    for (field, why) in cases {
        let sandbox = sandbox_with_config("bad-budget", BUDGET_RAW);
        let (_outcome, state) = boot(&sandbox, 6).await;
        let err = StreamState::new(&state.cache_dir, &field, 0, 0)
            .await
            .expect_err(&format!("{why} must be refused"));
        assert!(
            matches!(
                err,
                StreamStateError::UnparsableBudget
                    | StreamStateError::BudgetMismatch
                    | StreamStateError::ZeroBudget
            ),
            "{why}: static budget class, got {err:?}"
        );
        assert!(
            !state.cache_dir.join(TRANSCODING_CACHE_DIR).exists(),
            "{why}: no transcoding directory was created"
        );
        assert!(
            sandbox
                .entries()
                .iter()
                .all(|p| !p.starts_with("cache/transcoding")),
            "{why}: nothing was created under cache/"
        );
        let text = full_text(&err);
        for canary in [
            "canary-typed-budget",
            sandbox.root().to_str().unwrap(),
            "navidrome.db",
            ".db?",
        ] {
            assert!(
                !text.contains(canary),
                "diagnostic carries no canary/path: {text}"
            );
        }
    }
}

#[tokio::test]
async fn relative_cache_dir_is_refused_and_creates_nothing() {
    let sandbox = sandbox_with_config("relative", BUDGET_RAW);
    let err = StreamState::new(
        Path::new("cache"),
        &ByteSizeField::from_raw(BUDGET_RAW),
        0,
        0,
    )
    .await
    .expect_err("a relative cache dir must be refused");
    assert_eq!(err, StreamStateError::RelativeCacheDir);
    assert!(
        !std::path::Path::new("cache/transcoding").exists(),
        "no directory was created relative to the process cwd"
    );
    assert!(
        !sandbox.root().join("cache/transcoding").exists(),
        "nothing was created inside the isolated tree either"
    );
    let text = full_text(&err);
    assert!(text.contains("absolute"), "static class: {text}");
    assert!(!text.contains("transcoding"), "no path fragment: {text}");
}

#[tokio::test]
async fn uncreatable_cache_dir_reports_errno_only() {
    let sandbox = sandbox_with_config("uncreatable", BUDGET_RAW);
    let (_outcome, state) = boot(&sandbox, 7).await;
    // The cache dir startup created becomes read-only, so creating the
    // transcoding root under it fails. Permission limitation: mode bits only deny
    // access for an unprivileged user; under root they are bypassed and this case
    // would instead succeed — the "no cache files written" assertion holds either
    // way.
    std::fs::set_permissions(&state.cache_dir, std::fs::Permissions::from_mode(0o500))
        .expect("make the cache dir read-only");

    let err = StreamState::new(&state.cache_dir, &ByteSizeField::from_raw(BUDGET_RAW), 0, 0)
        .await
        .expect_err("the transcoding root cannot be created");

    std::fs::set_permissions(&state.cache_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore so the TempDir can clean up");

    let StreamStateError::CacheDirUnavailable { errno } = err else {
        panic!("expected a cache-directory failure, got {err:?}");
    };
    assert_ne!(errno, 0, "a numeric errno is reported");
    let text = full_text(&err);
    assert!(
        text.contains(&errno.to_string()),
        "the errno is reported: {text}"
    );
    for forbidden in [
        sandbox.root().to_str().unwrap(),
        "cache/transcoding",
        "Permission denied",
        "navidrome.db",
    ] {
        assert!(!text.contains(forbidden), "no path or OS message: {text}");
    }
    assert!(
        !state.cache_dir.join(TRANSCODING_CACHE_DIR).exists(),
        "no partial cache root survives the failure"
    );
}

#[tokio::test]
async fn stream_state_debug_withholds_the_cache_path() {
    let sandbox = sandbox_with_config("debug", BUDGET_RAW);
    let (outcome, state) = boot(&sandbox, 8).await;
    let stream = StreamState::new(
        &state.cache_dir,
        &outcome.config.transcoding_cache_size,
        3,
        1,
    )
    .await
    .expect("constructed stream state");

    let shown = format!("{stream:?}");
    assert!(
        !shown.contains(sandbox.root().to_str().unwrap()),
        "no cache path in Debug: {shown}"
    );
    assert!(
        !shown.contains("transcoding"),
        "no cache directory name: {shown}"
    );
    assert!(
        shown.contains("<transcode cache>"),
        "cache withheld: {shown}"
    );
    assert!(
        shown.contains("<shared limiter>"),
        "limiter withheld: {shown}"
    );
    assert!(
        shown.contains(&BUDGET_BYTES.to_string()),
        "budget reported: {shown}"
    );
    assert!(shown.contains("global_limit: 3"), "caps reported: {shown}");
}
