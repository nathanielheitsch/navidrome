//! Process-side of probing — `core/ffmpeg/ffmpeg.go` `ProbeAudioStream`
//! (`ffmpeg.go:150-168`): resolve the ffprobe binary, stat the media file
//! (`fileExists`, `ffmpeg.go:141-149`), launch `<ffprobe> args…` with
//! `exec.CommandContext` + `cmd.Output()` (`ffmpeg.go:161-165`), then hand
//! stdout to [`crate::probe.parse_probe_output`]. The argv is Go's
//! `probeAudioStreamCmd` (`ffmpeg.go:70`) through
//! [`crate::args.create_ffmpeg_command`], whose [`crate::args.fix_cmd`] pass
//! swaps the bare `ffprobe` token for the path derived by
//! [`crate::args.ffprobe_path`] (`ffmpeg.go:411-416`).
//!
//! Every failure is the [`crate::errors.ProbeError`] Go builds at
//! `ffmpeg.go:163` (launch/non-zero exit: reason from
//! [`crate::probe.probe_client_reason`], `ffmpeg.go:220-230`) or
//! `ffmpeg.go:167` (parse failure: reason is `err.Error()`).
//!
//! // shortcut: killpg — `process_group(0)` gives the child a *fresh* group
//! // keyed to its own pid (same tokio rule [`crate::ffcmd`] cites at
//! // process/mod.rs:772-773) and `kill_on_drop(true)` SIGKILLs that group
//! // root when the timed-out await's future — and with it the owned `Child`
//! // — is discarded; neither std nor tokio exposes `killpg`, and Go's
//! // `exec.CommandContext` signals only the direct child.
//! // shortcut: deadline — Go's probe has none; the port bounds the await at
//! // [`PROBE_TIMEOUT_MS`] so a hung ffprobe cannot pin the request.
//! // shortcut: `ProbeError.err` (Go's wrapped cause, read by
//! // `probeDetail`, `ffmpeg.go:211-218`) has no slot in the frozen struct, so
//! // the launch cause is folded into `reason` through
//! // [`crate::probe.probe_client_reason`].

use crate::args::{create_ffmpeg_command, ffprobe_path};
use crate::errors::{file_access_reason, FFmpegError, ProbeError};
use crate::ffmpeg_cmd::ffmpeg_cmd;
use crate::probe::{parse_probe_output, probe_client_reason, AudioProbeResult};

use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// `probeAudioStreamCmd` (`ffmpeg.go:70`), verbatim.
pub const PROBE_AUDIO_STREAM_CMD: &str =
    "ffprobe -v error -select_streams a:0 -print_format json -show_streams -show_format %s";

/// The port's probe deadline (ms); see the shortcut note above.
pub const PROBE_TIMEOUT_MS: u64 = 5000;

/// `ffmpegCmd` (`ffmpeg.go:604-622`) + `ffprobePath` (`ffmpeg.go:411-416`): the
/// ffprobe sibling of the resolved ffmpeg, `(path, ok)` in Go's `LookPath`
/// sense.
pub fn probe_binary() -> (String, bool) {
    let (ffmpeg, ok) = ffmpeg_cmd();
    if !ok {
        return (ffmpeg, false);
    }

    look_path(ffprobe_path(ffmpeg.as_str()))
}

/// Local `exec.LookPath` emulation — same rule as `ffmpeg_cmd.rs`'s private
/// `look_path` (existence on separators/`./`, else first PATH hit).
fn look_path(name: String) -> (String, bool) {
    if name.contains("/") || name.starts_with("./") {
        return (name.clone(), std::fs::exists(&name).unwrap_or(false));
    }
    let path_env = std::env::var("PATH").unwrap_or_default();
    for dir in path_env.split(":") {
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{dir}/{name}");
        if std::fs::exists(&candidate).unwrap_or(false) {
            return (candidate, true);
        }
    }
    (name, false)
}

/// `createFFmpegCommand(probeAudioStreamCmd, filePath, 0, 0)` (`ffmpeg.go:158`):
/// the whole argv, `argv[0]` included, launched by the caller as Go does with
/// `exec.CommandContext(args[0], args[1:]...)` (`ffmpeg.go:161`).
pub fn probe_argv(binary: String, path: String) -> Vec<String> {
    create_ffmpeg_command(PROBE_AUDIO_STREAM_CMD, path.as_str(), 0, 0, binary.as_str())
}

/// Go's `err.Error()` for the two failures `parseProbeOutput` raises
/// (`ffmpeg.go:231-297`), copied verbatim into
/// `ProbeError{Reason: err.Error()}` at `ffmpeg.go:167`.
fn probe_parse_reason(err: FFmpegError) -> String {
    match err {
        FFmpegError::ProbeParse(msg) => msg,
        FFmpegError::NoAudioStream => "no audio stream found in ffprobe output".to_string(),
        _ => err.to_string(),
    }
}

/// `fileExists` (`ffmpeg.go:141-149`) reduced to the token
/// [`crate::errors.file_access_reason`] classifies (`ffmpeg.go:200-209`): ""
/// when the path is a stat-able regular file, "not_found" for Go's
/// `fs.ErrNotExist` case, "permission_denied" for `fs.ErrPermission`, and
/// "inaccessible" for the directory branch (`ffmpeg.go:143`).
pub fn stat_token(path: String) -> String {
    let md = std::fs::metadata(&path);
    match md {
        Ok(m) => if m.is_dir() { "inaccessible" } else { "" }.to_string(),
        Err(e) => {
            let kind = e.kind();
            let token = if kind == std::io::ErrorKind::NotFound {
                "not_found"
            } else if kind == std::io::ErrorKind::PermissionDenied {
                "permission_denied"
            } else {
                "inaccessible"
            };
            token.to_string()
        }
    }
}

/// `cmd.Output()` (`ffmpeg.go:162`) for one argv: `(exit_zero, stdout, stderr)`.
/// A timeout or a spawn failure (Go's `exec` ENOEXEC path, whose `ProbeError`
/// at `ffmpeg.go:163` renders the generic reason) yields empty captures, so
/// [`crate::probe.probe_client_reason`] answers "could not read file".
pub async fn run_probe(binary: String, args: Vec<String>) -> (bool, Vec<u8>, Vec<u8>) {
    let mut cmd = Command::new(&binary);
    cmd.args(&args);
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());

    let bounded = timeout(Duration::from_millis(PROBE_TIMEOUT_MS), cmd.output()).await;
    if bounded.is_err() {
        return (false, Vec::new(), Vec::new());
    }

    let launched = bounded.unwrap();
    if launched.is_err() {
        return (false, Vec::new(), Vec::new());
    }

    let done = launched.unwrap();
    (done.status.success(), done.stdout, done.stderr)
}

/// `ProbeAudioStream` (`ffmpeg.go:150-168`) for an explicitly-supplied ffprobe
/// binary — the port's stand-in for Go's suite mutating the `ffmpegPath`
/// package global (`core/ffmpeg/ffmpeg_test.go:37`, `:67`, `:72`, `:764-768`)
/// so a mock binary can drive the launch paths.
pub async fn probe_audio_stream_with(
    binary: String,
    path: String,
) -> Result<AudioProbeResult, ProbeError> {
    let token = stat_token(path.clone());
    if token == "not_found" {
        return Err(ProbeError {
            path: path.clone(),
            reason: file_access_reason(token),
            not_found: true,
        });
    }
    if !token.is_empty() {
        return Err(ProbeError {
            path: path.clone(),
            reason: file_access_reason(token),
            not_found: false,
        });
    }

    let argv = probe_argv(binary.clone(), path.clone());
    let (ok, stdout, stderr) = run_probe(argv[0].to_string(), argv[1..].to_vec()).await;
    if !ok {
        return Err(ProbeError {
            path: path.clone(),
            reason: probe_client_reason(stderr, path),
            not_found: false,
        });
    }

    let (result, parse_err) = parse_probe_output(stdout.clone());
    match parse_err {
        None => Ok(result),
        Some(err) => Err(ProbeError {
            path,
            reason: probe_parse_reason(err),
            not_found: false,
        }),
    }
}

/// `ProbeAudioStream` (`ffmpeg.go:150-168`) resolving the binary through
/// configuration/PATH by [`probe_binary`].
pub async fn probe_audio_stream(path: String) -> Result<AudioProbeResult, ProbeError> {
    let (binary, ok) = probe_binary();
    if !ok {
        // Go propagates the raw `ffmpegCmd` error here; the port keeps one
        // error shape — [`crate::ffmpeg_cmd.cmd_path`] is the call site that
        // yields [`FFmpegError::NotAvailable`] with the unresolved path.
        return Err(ProbeError {
            path: path.clone(),
            reason: probe_client_reason(Vec::new(), path),
            not_found: false,
        });
    }

    probe_audio_stream_with(binary, path).await
}

#[cfg(test)]
mod tests {
    use crate::ffmpeg_cmd::is_probe_available;
    use crate::probe_exec::probe_audio_stream;
    use crate::probe_exec::probe_audio_stream_with;
    use crate::test_fixtures::fixture;

    #[tokio::test]
    async fn probe_missing_file_is_not_found() {
        // Arrange: Go's `ffmpeg_test.go:623-631` — missing media path, mock
        // binary (`ffmpegPath = /usr/bin/true` stand-in) never even launches.
        // Act
        let res = probe_audio_stream_with(
            "/bin/echo".to_string(),
            "/no/such/dir/really-missing.flac".to_string(),
        )
        .await;
        // Assert: SafeReason "file not found", NotFound true
        let err = res.unwrap_err();
        assert_eq!(err.reason, "file not found".to_string());
        assert!(err.not_found);
    }

    #[tokio::test]
    async fn probe_mock_binary_launch_failure() {
        // Arrange: /bin/sh as the ffprobe stand-in exits 127 with its own
        // usage stderr (direct exec, not `bash -c`).
        let res = probe_audio_stream_with("/bin/sh".to_string(), fixture("test.flac")).await;
        let err = res.unwrap_err();
        // Assert: launch failure maps through probe_client_reason, and the
        // media file exists so NotFound stays false (Go: SafeReason is the
        // ffprobe message, not "file not found").
        assert!(!err.not_found);
        assert!(err.reason.contains("No such file or directory"));
    }

    #[tokio::test]
    async fn probe_garbage_stdout_is_parse_error() {
        // Arrange: /bin/echo prints its argv — never valid ffprobe JSON.
        let res = probe_audio_stream_with("/bin/echo".to_string(), fixture("test.flac")).await;
        let err = res.unwrap_err();
        // Assert: parse failure reason carries Go's "parsing ffprobe output:"
        assert!(err.reason.contains("parsing ffprobe output"));
        assert!(!err.not_found);
    }

    /// Live pins verified against real ffprobe on this box: test.flac →
    /// codec flac, 44100 Hz, 1 channel, depth 16 via bits_per_raw_sample,
    /// 249 kbps from the format-level 249168.
    #[tokio::test]
    async fn probe_live_flac_matches_pins() {
        let media = fixture("test.flac");
        if !is_probe_available() {
            return; // optional external tool, never optional fixtures
        }
        let res = probe_audio_stream(media).await;
        let r = res.unwrap();
        assert_eq!(r.codec, "flac".to_string());
        assert_eq!(r.sample_rate, 44100);
        assert_eq!(r.channels, 1);
        assert_eq!(r.bit_depth, 16);
        assert_eq!(r.bit_rate, 249);
    }

    /// All seven valid container fixtures must exist and successfully probe.
    #[tokio::test]
    async fn probe_live_container_fixtures() {
        let media: Vec<_> = ["mp3", "ogg", "opus", "wav", "m4a", "wma", "aiff"]
            .iter()
            .map(|ext| fixture(&format!("test.{ext}")))
            .collect();
        assert_eq!(media.len(), 7);
        if !is_probe_available() {
            return; // optional external tool, never optional fixtures
        }
        let mut checked = 0;
        for path in media {
            let r = probe_audio_stream(path.clone())
                .await
                .unwrap_or_else(|err| panic!("{path}: {err:?}"));
            assert!(!r.codec.is_empty());
            assert!(r.bit_rate > 0);
            checked += 1;
        }
        assert_eq!(checked, 7);
    }
}
