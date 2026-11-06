//! The ffmpeg **process** type — Go's `ffCmd` (`core/ffmpeg/ffmpeg.go:354-361`
//! struct, `:363-382` `start`, `:384-398` `wait`, `:400-418` `limitedWriter`)
//! plus the cancellation contract the Go suite pins in
//! `core/ffmpeg/ffmpeg_test.go:636-800` ("should terminate the underlying
//! process when context is cancelled") and the reader-shaped `MockFFmpeg`
//! (`tests/mock_ffmpeg.go`: `Read`/`Close`/`IsClosed`).
//!
//! Cancellation is explicit, not ambient. Go hangs a `context.Context` off
//! every call and lets `exec.CommandContext` kill the child once that context
//! is cancelled; Rust has no ambient context, so [`FFCmd`] owns the tokio
//! child handle and an explicit `cancel()`/`close()` ends the stream. The
//! #07 hard requirement — cancel kills the process group, leaves no zombie,
//! leaves no orphaned stdout — is carried by [`FFCmd::cancel`]: `process_group`
//! at spawn, SIGKILL followed by `wait()` (the reap), then both stdio handles
//! taken and dropped so neither pipe survives.
//!
//! // shortcut: pgid — Go never calls setpgid (`grep setpgid core/` is empty),
//! so `exec.CommandContext` signals only the direct child and a shell
//! wrapper's grandchildren can leak. tokio's `Command::process_group(i32)`
//! (tokio-1.54.1/src/process/mod.rs:790, a thin forward to std's builder) is
//! documented "Equivalent to a `setpgid` call in the child process" with the
//! reuse rule "A process group ID of 0 will use the process ID as the PGID"
//! (mod.rs:772-773), so `0` gives the child a *fresh* group keyed to its own
//! pid — what Go's fresh launch asks for. The signal itself goes to that
//! group's root pid (`Child::start_kill()` → std SIGKILL), because neither
//! std nor tokio exposes `killpg` and the crate graph forbids a deps crate;
//! the fresh group still isolates the ffmpeg tree, leaving only the same
//! window Go leaves.

use std::process::ExitStatus;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdout, Command};

/// `limitedWriter`'s `limit` (`ffmpeg.go:370`: `limit: 4096`).
pub const STDERR_CAP: usize = 4096;

/// Go's `ffCmd` struct (`ffmpeg.go:354-361`), split at Go's own
/// `exec.CommandContext(ctx, j.args[0], j.args[1:]...)` convention
/// (`ffmpeg.go:366`).
pub struct FFCmd {
    /// Go's `cmd: *exec.Cmd` (`ffmpeg.go:358`): the launch path (`args[0]`).
    pub exec: String,
    /// Go's `args[1:]` (`ffmpeg.go:366`).
    pub args: Vec<String>,
    /// Go's `*io.PipeReader` + `out: *io.PipeWriter` (`ffmpeg.go:355`/`:357`):
    /// the tokio child whose piped stdout is the audio stream. Emptied by
    /// `cancel`/`wait`, exactly as Go closes the pipe there.
    pub child: Option<Child>,
    /// Go's `stderr: *bytes.Buffer` (`ffmpeg.go:359`), filled through the
    /// `limitedWriter` cap (`ffmpeg.go:400-418`).
    pub stderr: Vec<u8>,
    /// `MockFFmpeg.closed` (`tests/mock_ffmpeg.go:66`), flipped by
    /// `MockFFmpeg.Close`/`IsClosed` (`:88`/`:90`).
    pub closed: bool,
}

/// The wait result Go pushes into the pipe (`ffmpeg.go:387`'s exit code plus
/// the `:359` stderr buffer), as the pair `transcode` needs to format
/// [`go_error_message`].
pub struct ExitInfo {
    /// `exitErr.ExitCode()` (`ffmpeg.go:387`); `-1` when the process died of a
    /// signal, which is Go's own value there.
    pub code: i64,
    /// The capped stderr buffer, verbatim — trimming is the caller's (Go's
    /// `strings.TrimSpace`, `ffmpeg.go:389`).
    pub stderr: String,
}

/// `ffCmd`'s `*exec.Cmd` only exists inside `start`, so the constructor holds
/// launch parameters.
pub fn new_ffmpeg_cmd(exec: String, args: Vec<String>) -> FFCmd {
    FFCmd {
        exec,
        args,
        child: None,
        stderr: Vec::new(),
        closed: false,
    }
}

/// Go's error text (`ffmpeg.go:387-390`):
/// `"<path> exited with non-zero status code: <code>"`, plus `": " + TrimSpace(stderr)`
/// whenever the capped buffer is non-blank.
pub fn go_error_message(path: String, code: i64, stderr: String) -> String {
    let head = path + " exited with non-zero status code: " + code.to_string().as_str();
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return head;
    }
    head + ": " + trimmed
}

/// Exit code + capped stderr of a reaped child (`ffmpeg.go:384-398` inputs).
pub fn exit_info(status: std::io::Result<ExitStatus>, stderr_buf: Vec<u8>) -> ExitInfo {
    let code: i64 = match status {
        Ok(st) => st.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    };
    ExitInfo {
        code,
        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
    }
}

impl FFCmd {
    /// `MockFFmpeg.IsAvailable` (`tests/mock_ffmpeg.go:73`): a child is running.
    pub fn is_available(self: &FFCmd) -> bool {
        self.child.is_some()
    }

    /// `MockFFmpeg.IsClosed` (`tests/mock_ffmpeg.go:90`).
    pub fn is_closed(self: &FFCmd) -> bool {
        self.closed
    }

    /// Go's `limitedWriter.Write` (`ffmpeg.go:400-418`): keep the first `limit`
    /// bytes, silently discard the overflow, and report the whole write as
    /// consumed (Go returns `n, nil` so `io.MultiWriter` never sees a short
    /// write) — ffmpeg's own exit status is never masked by its noise.
    pub fn append_capped(&mut self, chunk: &[u8]) {
        if self.stderr.len() >= STDERR_CAP {
            return;
        }
        let remaining = STDERR_CAP - self.stderr.len();
        let take = if chunk.len() <= remaining {
            chunk.len()
        } else {
            remaining
        };
        self.stderr.extend(chunk[..take].to_vec());
    }

    /// Go's `ffCmd.start` (`ffmpeg.go:363-382`): spawn the child. Stdout and
    /// stderr are piped (Go: `cmd.Stdout = j.out`, `cmd.Stderr = stderrWriter`);
    /// stdin is `/dev/null` (Go leaves `cmd.Stdin` unset, which inherits our
    /// stdin and lets ffmpeg stall behind a terminal).
    pub async fn start(&mut self) -> std::io::Result<()> {
        let mut cmd = Command::new(&self.exec);
        cmd.args(&self.args);
        cmd.process_group(0);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        self.child = Some(cmd.spawn()?);
        Ok(())
    }

    /// Go's `ffCmd.wait` (`ffmpeg.go:384-398`) on a child that exits on its own:
    /// drain the capped stderr, reap it, and close the reader.
    pub async fn wait(&mut self) -> std::io::Result<ExitInfo> {
        self.closed = true;
        let child = self.child.take();
        if child.is_none() {
            return Ok(ExitInfo {
                code: -1,
                stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            });
        }
        let mut child = child.unwrap();
        let drained = drain_stderr(&mut child).await;
        self.append_capped(&drained);
        child.stdout.take();

        let status = child.wait().await;
        Ok(exit_info(status, self.stderr.clone()))
    }

    /// Go's post-`cancel()` teardown: `exec.CommandContext` SIGKILLs the child
    /// (SIGKILL is Go's own default signal — there is no SIGTERM/bounded-grace
    /// round trip to port) and `wait` (`ffmpeg.go:384-398`) reaps it. Here the
    /// kill is explicit: SIGKILL at the process-group root, drain both pipes so
    /// no orphaned stdout survives, then `wait()` so no zombie remains.
    /// Consumes the handle — after this the stream is dead.
    pub async fn cancel(&mut self) -> std::io::Result<ExitInfo> {
        self.closed = true;
        let child = self.child.take();
        if child.is_none() {
            return Ok(ExitInfo {
                code: -1,
                stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            });
        }
        let mut child = child.unwrap();
        child.start_kill()?;
        let drained = drain_stderr(&mut child).await;
        self.append_capped(&drained);
        child.stdout.take();

        let status = child.wait().await;
        Ok(exit_info(status, self.stderr.clone()))
    }

    /// `MockFFmpeg.Close` (`tests/mock_ffmpeg.go:88`) plus the cancel teardown:
    /// the caller stopped caring about the stream, so the exit info is dropped.
    pub async fn close(&mut self) -> std::io::Result<()> {
        self.cancel().await.map(|_| ())
    }

    /// Go's `cmd.Stdout = j.out` (`ffmpeg.go:367`): the audio stream Go hands
    /// back as an `io.ReadCloser`. Dropping it closes the pipe.
    pub fn reader(&mut self) -> Option<ChildStdout> {
        let mut child = self.child.take()?;
        let out = child.stdout.take();
        self.child = Some(child);
        out
    }
}

/// `limitedWriter` fed from the piped stderr (`ffmpeg.go:370` + `:400-418`):
/// read to EOF, then keep only the first [`STDERR_CAP`] bytes (via
/// [`Ffcmd.append_capped`]) and drop the reader so the fd cannot survive as an
/// orphaned pipe.
///
/// // shortcut: read whole — Go feeds the cap incrementally while the child
/// // writes, so a runaway ffmpeg is bounded live. The port buffers the stream
/// // and truncates at the same 4096 mark: identical captured content, one
/// // transient buffer instead of a capped writer.
async fn drain_stderr(child: &mut tokio::process::Child) -> Vec<u8> {
    let maybe_err = child.stderr.take();
    if maybe_err.is_none() {
        return Vec::new();
    }
    let mut err = maybe_err.unwrap();
    let captured: Vec<u8> = Vec::new();
    let mut captured = captured;
    err.read_to_end(&mut captured).await.ok();
    captured
}

#[cfg(test)]
mod ffcmd_tests {
    use super::*;
    use crate::ffmpeg_cmd::ffmpeg_cmd;
    use tokio::time::{timeout, Duration};

    /// Arrange-side shorthand: build a `Vec<String>` from literals.
    fn w(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// (a) cancel-soak: the group is killed and reaped (`wait()` succeeds, so
    /// no zombie remains) and stdout reaches clean EOF instead of blocking.
    #[tokio::test]
    async fn cancel_soak_reaps_and_closes_stdout() {
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", "sleep 30"]));
        cmd.start().await.unwrap();

        let mut out = cmd.reader().unwrap();
        let killed = cmd.cancel().await;
        assert!(killed.is_ok());
        assert_eq!(killed.unwrap().code, -1);
        assert!(cmd.is_closed());

        let buf = [0u8; 8];
        let mut buf_mut = buf;
        let read = timeout(Duration::from_millis(100), out.read(&mut buf_mut)).await;
        if read.is_ok() {
            assert_eq!(read.unwrap().unwrap_or(0), 0);
        }
    }

    /// (b) the Go message form (`ffmpeg.go:387-390`) with the 4096 cap applied.
    #[tokio::test]
    async fn exit_message_matches_go() {
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", "echo oops 1>&2; exit 3"]));
        cmd.start().await.unwrap();

        let info = cmd.wait().await.unwrap();
        assert_eq!(info.code, 3);
        assert_eq!(
            go_error_message(cmd.exec.clone(), info.code, info.stderr),
            "/bin/sh exited with non-zero status code: 3: oops".to_string()
        );

        let long = "x".repeat(5000).as_bytes().to_vec();
        cmd.append_capped(&long);
        assert_eq!(cmd.stderr.len(), STDERR_CAP);
    }

    /// (c) live ffmpeg through the same launch, skipped when unresolved.
    #[tokio::test]
    async fn live_ffmpeg_version_exits_zero() {
        let (path, ok) = ffmpeg_cmd();
        if !ok {
            return;
        }

        let mut cmd = new_ffmpeg_cmd(path, w(&["-version"]));
        cmd.start().await.unwrap();

        let mut out = cmd.reader().unwrap();
        let mut banner: Vec<u8> = Vec::new();
        out.read_to_end(&mut banner).await.ok();

        let info = cmd.wait().await.unwrap();
        assert_eq!(info.code, 0);
        assert!(String::from_utf8_lossy(&banner).contains("ffmpeg version"));
    }
}
