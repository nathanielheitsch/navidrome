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

/// Go's `sync.Once` for the ffmpeg lookup (`ffmpeg.go:604-622`): the
/// resolved `(path, ok)` pair, computed at most once per process.
static FFMPEG_ONCE: std::sync::OnceLock<(String, bool)> = std::sync::OnceLock::new();

/// Go's `sync.Once` + `probeAvail` globals (`ffmpeg.go:604-622`, used by
/// `IsProbeAvailable`).
static PROBE_ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// `ffmpegCmd` (`ffmpeg.go:604-622`): config-set `ffmpeg_path` wins, else
/// PATH lookup of "ffmpeg", with the ErrDot retry on the literal "./ffmpeg".
pub fn ffmpeg_cmd() -> (String, bool) {
    if let Some(v) = FFMPEG_ONCE.get().cloned() {
        return v;
    }

    let configured = nd_conf::Config::default().ffmpeg_path;
    let test_override = env(ND_TEST_FFMPEG_PATH.to_string());
    let configured = if configured.is_empty() {
        test_override
    } else {
        configured
    };

    let mut r: (String, bool);
    if !configured.is_empty() {
        r = look_path(configured);
    } else {
        r = look_path("ffmpeg".to_string());
        if !r.clone().1 {
            // Go's `errors.Is(ffmpegErr, exec.ErrDot)`: found only in cwd '.'.
            r = look_path("./ffmpeg".to_string());
        }
    }

    let _ = FFMPEG_ONCE.set(r.clone());
    r
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
    let (_, ok) = ffmpeg_cmd();
    ok
}

/// `IsProbeAvailable` (`ffmpeg.go:308-319`): ffmpeg must resolve, and the
/// ffprobe sibling derived from the resolved path
/// ([`args::ffprobe_path`]) must resolve too — memoised in its own `OnceLock`
/// exactly as Go's `probeOnce.Do` does.
pub fn is_probe_available() -> bool {
    let (path, ok) = ffmpeg_cmd();
    if !ok {
        return false;
    }

    if let Some(v) = PROBE_ONCE.get().cloned() {
        return v;
    }

    let probe = ffprobe_path(&path);
    let (_, avail) = look_path(probe);
    let _ = PROBE_ONCE.set(avail);
    avail
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
