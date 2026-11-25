//! Resolved ffmpeg binary — `core/ffmpeg/ffmpeg.go` `ffmpegCmd`/`CmdPath`/
//! `IsAvailable`/`IsProbeAvailable`/`Version` (the process-adapter entry
//! points that precede `Transcode`). Go memoises the lookup with
//! `sync.Once` + package globals (`ffmpeg.go:604-622`); the port caches the
//! `(path, ok)` pair in a [`std::sync::OnceLock`].
//!
//! Go delegates binary discovery to `exec.LookPath`, which the stdlib does
//! not provide; `look_path` below emulates it:
//!
//! * an absolute path, a `./`-relative path, or any name containing a
//!   separator → existence check on that path (Go's ErrDot branch covers the
//!   `.`-entry case explicitly);
//! * a bare name → first `PATH` entry that contains it (Go also verifies the
//!   executable bit — see the shortcut note at `exec_bit`).
//!
//! // shortcut: exec-bit unchecked — `std::fs` exposes no portable mode check
//! without pulling in a deps crate, so a non-executable match is accepted.
//! The consequence is that a PATH hit on a non-executable file resolves as
//! "available" and the later spawn fails with ENOTEXEC instead of Go's
//! "not found"; the resolution contract (issue #07) only pins the lookup
//! order, so this is left as a deviation rather than a port-level fix.

/// Test-only override of the resolved binary, mirroring the Go suite's
/// direct `ffmpegPath = ...` package-global mutation
/// (`core/ffmpeg/ffmpeg_test.go:37`, `:67`, `:72`). Read once, exactly like
/// the `sync.Once` it stands in for.
const ND_TEST_FFMPEG_PATH: &str = "ND_TEST_FFMPEG_PATH";

use crate::args::ffprobe_path;
use crate::errors::FFmpegError;

/// Go's `os.Getenv`: the process environment value for `key`, empty when
/// unset.
fn env(key: String) -> String {
    std::env::var(&key).unwrap_or_default()
}

/// `exec.LookPath` emulation: `(resolved_path, ok)`. `name` is what Go was
/// asked to look up; the bool is Go's `err == nil`.
fn look_path(name: String) -> (String, bool) {
    let has_sep = name.contains("/");
    if has_sep || name.starts_with("./") {
        let ok = std::fs::exists(&name);
        return (name, ok.unwrap_or(false));
    }

    let path = env("PATH".to_string());
    for dir in path.split(":") {
        if dir.is_empty() {
            continue;
        }
        let candidate = dir.to_string() + "/" + &name;
        if std::fs::exists(&candidate).unwrap_or(false) {
            return (candidate.to_string(), true);
        }
    }

    (name, false)
}

/// One synchronized resolution record for the whole resolver, replacing the two
/// independent `OnceLock`s (`FFMPEG_ONCE`, `PROBE_ONCE`). It holds the loaded
/// configured path, the resolved ffmpeg pair, and the probe-availability cache in
/// a single locked value, so configuration injection, ffmpeg resolution, and probe
/// availability can never disagree across threads or across a second `initialize`.
///
/// Recursion safety: the guard is held only across filesystem lookups
/// ([`look_path`], [`nd_ffmpeg::args::ffprobe_path`] is pure string work). It is
/// **never** held while calling a public resolver function, [`version`], or
/// anything in `encoder_probe` (`has`/`encoders_for`), because those either
/// re-enter `ffmpeg_cmd` or spawn a child process.
struct Resolution {
    /// The resolved `(path, ok)` pair — Go's `ffmpegPath, ffmpegErr`.
    ffmpeg: (String, bool),
    /// Probe-sibling availability, computed lazily on first
    /// [`is_probe_available`] (Go's `probeOnce`). `None` = not yet computed.
    probe: Option<bool>,
    /// Which configuration produced this resolution, so a repeat or conflicting
    /// `initialize_from_loaded_config` can be classified without silent overwrite.
    source: Source,
}

/// Where a [`Resolution`] came from.
enum Source {
    /// Resolved lazily by the legacy call path (empty config +
    /// `ND_TEST_FFMPEG_PATH` override + `PATH`), before any config was injected.
    LazyEmpty,
    /// Resolved from a loaded config with this `ffmpeg_path` (possibly empty).
    Loaded(String),
}

/// The single resolver state. `None` = nothing resolved yet.
static RESOLVER: std::sync::Mutex<Option<Resolution>> = std::sync::Mutex::new(None);

/// Why [`initialize_from_loaded_config`] refused. Every variant is static text:
/// no path, no `Config`, no environment value, and no competing configured path
/// ever crosses this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FfmpegConfigError {
    /// A resolution was already produced by the legacy lazy path, and the config
    /// now being injected names a binary (non-empty `FFmpegPath`). The lazy
    /// winner is kept; the configured path is **not** silently ignored — the
    /// caller is told to inject config before any resolver call.
    #[error("ffmpeg binary was already resolved before config was injected")]
    LateInitialization,
    /// The resolver is already initialized from a **different** loaded
    /// `FFmpegPath`. Boot config is set exactly once; a conflicting path is
    /// refused rather than swapped.
    #[error("ffmpeg config was already initialized from a different path")]
    ConflictingConfig,
    /// The resolver lock was poisoned by a panicking holder. Fail closed: this is
    /// reported rather than recovered, and no panic text is surfaced.
    #[error("ffmpeg resolver is in an inconsistent state")]
    Poisoned,
}

/// The legacy lazy resolution — the empty-config path, byte-for-byte the prior
/// behavior: `ND_TEST_FFMPEG_PATH` override, else `PATH` lookup of `ffmpeg`, else
/// the `ErrDot` retry on the literal `./ffmpeg`. Filesystem only; spawns nothing.
fn resolve_legacy() -> (String, bool) {
    let test_override = env(ND_TEST_FFMPEG_PATH.to_string());
    if !test_override.is_empty() {
        return look_path(test_override);
    }
    let r = look_path("ffmpeg".to_string());
    if r.1 {
        return r;
    }
    // Go's `errors.Is(ffmpegErr, exec.ErrDot)`: found only in cwd '.'.
    look_path("./ffmpeg".to_string())
}

/// Resolve an explicitly configured binary path. A non-empty path is checked
/// **only** at that path — `look_path` performs no `PATH` fallback for a
/// path-shaped or bare configured value that misses. This is what makes a
/// configured-but-missing binary report unavailable instead of silently using a
/// default-path ffmpeg (`decisions.md` B2). Filesystem only.
fn resolve_configured(path: &str) -> (String, bool) {
    look_path(path.to_string())
}

/// `ffmpegCmd` (`ffmpeg.go:604-622`): the memoized resolved `(path, ok)` pair.
/// If a resolution already exists it is returned verbatim; otherwise the legacy
/// lazy path is computed and cached under the same guard.
///
/// Lock poisoning fails **closed**: a poisoned lock returns an unavailable pair
/// `("", false)` rather than panicking or surfacing panic text.
pub fn ffmpeg_cmd() -> (String, bool) {
    let mut guard = match RESOLVER.lock() {
        Ok(g) => g,
        Err(_) => return (String::new(), false),
    };
    if let Some(resolution) = guard.as_ref() {
        return resolution.ffmpeg.clone();
    }
    let pair = resolve_legacy();
    *guard = Some(Resolution {
        ffmpeg: pair.clone(),
        probe: None,
        source: Source::LazyEmpty,
    });
    pair
}

/// `CmdPath` (`ffmpeg.go:590-592`): the resolved path, or
/// [`FFmpegError::NotAvailable`] carrying the unresolved path when the
/// binary could not be found (Go returns `ffmpegPath, ffmpegErr`).
pub fn cmd_path() -> Result<String, FFmpegError> {
    let (path, ok) = ffmpeg_cmd();
    if ok {
        return Ok(path);
    }
    Err(FFmpegError::NotAvailable(path))
}

/// `IsAvailable` (`ffmpeg.go:303-306`): the ffmpeg binary resolved.
pub fn is_available() -> bool {
    ffmpeg_cmd().1
}

/// `IsProbeAvailable` (`ffmpeg.go:308-319`): ffmpeg must resolve, and the ffprobe
/// sibling derived from the resolved path ([`args::ffprobe_path`]) must resolve
/// too. Cached inside the same [`Resolution`] record, so it always reflects the
/// currently resolved binary and never a superseded one.
///
/// Runs only filesystem lookups under the guard. Poisoning fails closed to
/// `false`.
pub fn is_probe_available() -> bool {
    let mut guard = match RESOLVER.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    // Ensure a resolution exists (legacy lazy path) before deriving the sibling.
    if guard.is_none() {
        let pair = resolve_legacy();
        *guard = Some(Resolution {
            ffmpeg: pair,
            probe: None,
            source: Source::LazyEmpty,
        });
    }
    let resolution = guard.as_mut().expect("resolution just ensured");
    if !resolution.ffmpeg.1 {
        return false;
    }
    if let Some(avail) = resolution.probe {
        return avail;
    }
    let probe = ffprobe_path(&resolution.ffmpeg.0);
    let avail = look_path(probe).1;
    resolution.probe = Some(avail);
    avail
}

/// Inject the loaded configuration exactly once (BOOT-01 boot-once seam).
///
/// This replaces the `Config::default()` read that made a configured
/// `FFMPEGPath` invisible at runtime (`decisions.md` B2). It resolves ffmpeg from
/// `config.ffmpeg_path` using the preserved precedence rules and records the
/// result; existing free-function callers then observe the configured binary.
///
/// Precedence is preserved exactly as the legacy path: a non-empty loaded
/// `FFmpegPath` wins; a missing configured binary resolves to **unavailable with
/// no `PATH` fallback**; an empty loaded config falls back to the same legacy
/// resolution (`ND_TEST_FFMPEG_PATH`, then `PATH`).
///
/// Repeat/compatibility rules (never silent):
/// * identical loaded path → accepted (`Ok`), no re-resolve;
/// * empty loaded config against a prior empty-config legacy resolution → accepted
///   (same resolution, compatible);
/// * a configured (non-empty) path after a lazy lookup → [`LateInitialization`];
/// * a loaded path differing from the already-loaded one → [`ConflictingConfig`].
///
/// **Availability decision (reported):** `initialize` records the resolution and
/// returns `Ok` even when the configured binary is missing — matching Go's
/// `sync.Once`, which never errors on a lookup miss. Availability is then surfaced
/// by `cmd_path()` returning [`FFmpegError::NotAvailable`] and `is_available()`
/// returning `false`. A later CLI/BOOT-01 integration is expected to require
/// availability explicitly; init itself does not fail on a missing binary.
///
/// Only `config.ffmpeg_path` is read; the `Config` is never formatted, so secret
/// config fields are never disclosed.
///
/// # Errors
/// [`FfmpegConfigError`] with static text. A poisoned lock returns
/// [`FfmpegConfigError::Poisoned`] and leaves the resolver unusable rather than
/// recovering it.
pub fn initialize_from_loaded_config(config: &nd_conf::Config) -> Result<(), FfmpegConfigError> {
    let mut guard = match RESOLVER.lock() {
        Ok(g) => g,
        Err(_) => return Err(FfmpegConfigError::Poisoned),
    };
    let path = config.ffmpeg_path.clone();
    match guard.as_ref() {
        None => {
            let (ffmpeg, source) = if path.is_empty() {
                (resolve_legacy(), Source::LazyEmpty)
            } else {
                (resolve_configured(&path), Source::Loaded(path))
            };
            *guard = Some(Resolution {
                ffmpeg,
                probe: None,
                source,
            });
            Ok(())
        }
        Some(resolution) => match &resolution.source {
            // Already loaded: identical path is compatible, a differing one is a
            // conflict.
            Source::Loaded(prev) if *prev == path => Ok(()),
            // Lazy legacy resolution: compatible only with an empty config that
            // reuses the same resolution; a configured path cannot be retrofitted.
            Source::LazyEmpty if path.is_empty() => Ok(()),
            Source::LazyEmpty => Err(FfmpegConfigError::LateInitialization),
            Source::Loaded(_) => Err(FfmpegConfigError::ConflictingConfig),
        },
    }
}

/// `Version` (`ffmpeg.go:321-337`): run `<resolved ffmpeg> -version` and
/// return the third space-separated token of the combined output. ffmpeg
/// writes its banner to STDERR, so stdout and stderr are joined (Go's
/// `CombinedOutput`). "N/A" when the binary is missing, the spawn fails, or
/// fewer than three tokens are produced.
// shortcut: sync capture — `std::process::Command` blocks for the child; the
// tokio async form (with the cancel/kill-group contract) lives in the
// transcode lane.
pub fn version() -> String {
    let (path, ok) = ffmpeg_cmd();
    if !ok {
        return "N/A".to_string();
    }

    let res = std::process::Command::new(path).arg("-version").output();
    let whole: String = match res {
        Ok(o) => {
            let mut combined = o.stdout;
            combined.extend(o.stderr);
            String::from_utf8_lossy(&combined).into_owned()
        }
        Err(_) => return "N/A".to_string(),
    };

    // Go's `strings.Split(out, " ")`: empty pieces are kept, so the token
    // index matches Go even for a padded banner.
    let parts: Vec<String> = whole.split(" ").map(|p| p.to_string()).collect();
    if parts.len() < 3 {
        return "N/A".to_string();
    }

    parts[2].to_string()
}
