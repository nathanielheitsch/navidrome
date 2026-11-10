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
//! #07 hard requirement — cancel kills the **process group**, leaves no zombie,
//! leaves no orphaned stdout — is carried by [`FFCmd::cancel`]: `process_group`
//! at spawn, SIGKILL to the whole group, then the reap, then both stdio handles
//! taken and dropped so neither pipe survives.
//!
//! The group id is recorded at spawn (`FFCmd::pgid`): `process_group(0)` makes
//! the child the group leader, so pgid == the child's pid (tokio-1.54.1
//! src/process/mod.rs:772-773, "A process group ID of 0 will use the process ID
//! as the PGID") and the recorded pid *is* the group id. Signalling the group
//! rather than only the direct child is what reaps an ffmpeg tree: Go's
//! `exec.CommandContext` signals the direct child alone (`grep setpgid core/` is
//! empty), so a wrapper's grandchildren survive there — the port closes that
//! window instead of reproducing it. `Child::start_kill()` cannot do this,
//! because std/tokio expose no `killpg`; the group signal comes from
//! `rustix::process::kill_process_group` (rustix-1.1.6 `src/process/kill.rs:35`,
//! the safe `kill(-pid, sig)` wrapper). `unsafe_code` stays denied
//! workspace-wide (rust/Cargo.toml:69).
//!
//! Teardown is bounded: stderr is drained concurrently with a hard [`STDERR_CAP`]
//! bound that keeps consuming past the cap so the child never blocks on a full
//! pipe, and the drain plus the reap run under [`TEARDOWN_TIMEOUT_MS`] so a
//! descendant holding a pipe cannot pin the caller.

use std::process::ExitStatus;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::{timeout, Duration};

/// `limitedWriter`'s `limit` (`ffmpeg.go:370`: `limit: 4096`).
pub const STDERR_CAP: usize = 4096;

/// Upper bound on cancel/wait teardown (ms). A descendant that keeps stdout or
/// stderr open would otherwise leave the drain — and with it the reap — waiting
/// forever; past this bound the pipes are dropped and the caller still gets its
/// [`ExitInfo`] with whatever the reap produced.
pub const TEARDOWN_TIMEOUT_MS: u64 = 2000;

/// Signal the child's whole process group, falling back to the direct-child kill
/// when the group signal reports the group is already gone (`ESRCH`).
///
/// `Ok(true)` means the group signal was delivered; `Ok(false)` means only the
/// direct-child fallback ran.
fn kill_group(child: &mut Child, pgid: Option<i32>) -> std::io::Result<bool> {
    if let Some(raw) = pgid {
        if let Some(pid) = rustix::process::Pid::from_raw(raw) {
            match rustix::process::kill_process_group(pid, rustix::process::Signal::KILL) {
                Ok(()) => return Ok(true),
                // ESRCH: the group is already gone, so the direct-child kill is
                // all that is left to do.
                Err(e) if e == rustix::io::Errno::SRCH => return Ok(false),
                Err(e) => return Err(std::io::Error::from_raw_os_error(e.raw_os_error())),
            }
        }
    }
    child.start_kill()?;
    Ok(false)
}

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
    /// Process-group id of the spawned child, recorded at `start`. Private: the
    /// only supported construction path is [`new_ffmpeg_cmd`], so a caller cannot
    /// install a pgid for a process this crate never spawned. `None` before start
    /// and after teardown, which makes `cancel`/`close` idempotent.
    pgid: Option<i32>,
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
        pgid: None,
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

        let child = cmd.spawn()?;
        // process_group(0) makes the child the group leader, so its pid is the
        // group id; record it for the group kill in cancel().
        self.pgid = child.id().map(|id| id as i32);
        self.child = Some(child);
        Ok(())
    }

    /// Go's `ffCmd.wait` (`ffmpeg.go:384-398`) on a child that exits on its own:
    /// drain the capped stderr, reap it, and close the reader. Teardown is
    /// bounded by [`TEARDOWN_TIMEOUT_MS`].
    pub async fn wait(&mut self) -> std::io::Result<ExitInfo> {
        self.teardown(false).await
    }

    /// Kill the child's **process group** and reap it. Go reaches this through
    /// `exec.CommandContext`, which SIGKILLs the direct child only; the port
    /// signals the whole group so a wrapper's descendants die with it (see the
    /// module note). Order matters: signal first, then drain the pipes, then
    /// reap, so a descendant cannot keep a pipe open forever. Idempotent — a
    /// second call on a dead stream returns the empty [`ExitInfo`].
    ///
    /// Drop semantics, stated honestly: dropping [`FFCmd::reader`] does NOT kill
    /// the child, it only closes the stdout pipe. While the caller owns the
    /// `FFCmd`, [`FFCmd::cancel`]/[`FFCmd::close`] is the only thing that ends the
    /// process.
    pub async fn cancel(&mut self) -> std::io::Result<ExitInfo> {
        self.teardown(true).await
    }

    /// Shared teardown for `wait`/`cancel`: optional group kill, bounded
    /// concurrent stderr drain, stdout pipe dropped, bounded reap.
    async fn teardown(&mut self, kill: bool) -> std::io::Result<ExitInfo> {
        self.closed = true;
        let child = self.child.take();
        let Some(mut child) = child else {
            self.pgid = None;
            return Ok(ExitInfo {
                code: -1,
                stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            });
        };
        let pgid = self.pgid.take();

        if kill {
            kill_group(&mut child, pgid)?;
        }

        // Bounded: a descendant holding stderr open must not pin the caller.
        let bounded = timeout(
            Duration::from_millis(TEARDOWN_TIMEOUT_MS),
            drain_stderr(&mut child),
        )
        .await;
        if let Ok(drained) = bounded {
            self.append_capped(&drained);
        }
        child.stdout.take();

        let status = timeout(Duration::from_millis(TEARDOWN_TIMEOUT_MS), child.wait())
            .await
            .unwrap_or_else(|_| Err(std::io::Error::last_os_error()));
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
/// capture at most [`STDERR_CAP`] bytes and **keep consuming the overflow** so a
/// chatty ffmpeg never blocks on a full pipe, then drop the reader so the fd
/// cannot survive as an orphaned pipe.
///
/// Deviation from Go, deliberate: Go wraps the sink in `limitedWriter`, which
/// discards overflow at the writer. Discarding at the *reader* is equivalent
/// here — Go returns `(n, nil)` from `limitedWriter.Write` (`ffmpeg.go:410-417`),
/// so the child sees a full sink either way — and it bounds memory at the cap
/// instead of buffering the whole stream.
async fn drain_stderr(child: &mut tokio::process::Child) -> Vec<u8> {
    let maybe_err = child.stderr.take();
    if maybe_err.is_none() {
        return Vec::new();
    }
    let mut err = maybe_err.unwrap();
    let mut captured: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match err.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if captured.len() >= STDERR_CAP {
                    continue; // drain the overflow, keep nothing
                }
                let room = STDERR_CAP - captured.len();
                captured.extend_from_slice(&chunk[..room.min(n)]);
            }
            Err(_) => break,
        }
    }
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

    /// (a) cancel-soak: the group is killed and reaped (`cancel()` succeeds, so
    /// no zombie remains) and stdout reaches clean EOF. A timeout here is a
    /// FAILURE, not a pass — an un-killed child keeps the pipe open forever.
    #[tokio::test]
    async fn cancel_soak_reaps_and_closes_stdout() {
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", "sleep 30"]));
        cmd.start().await.unwrap();

        let mut out = cmd.reader().unwrap();
        let killed = cmd.cancel().await;
        assert!(killed.is_ok());
        assert_eq!(killed.unwrap().code, -1);
        assert!(cmd.is_closed());

        let mut buf = [0u8; 8];
        let read = timeout(Duration::from_millis(500), out.read(&mut buf)).await;
        let read = read.expect("stdout never reached EOF after cancel");
        assert_eq!(read.unwrap_or(0), 0, "stdout must be at EOF");
    }

    /// Is a pid still alive? `ps -p <pid>` prints a line only for a live one.
    fn pid_alive(pid: u32) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output();
        match out {
            Ok(o) => {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .matches(char::is_numeric)
                    .count()
                    > 0
            }
            Err(_) => false,
        }
    }

    /// (d) the group kill, not just the direct-child kill: a grandchild
    /// outlives the root's exit (it is reparented, and keeps the group id) yet
    /// dies when the group is signalled. This fails without `kill_process_group`.
    #[tokio::test]
    async fn cancel_kills_grandchildren_that_outlived_the_root() {
        let marker = std::env::temp_dir().join(format!("nd-ffmpeg-pgid-{}", std::process::id()));
        // Grandchild in the background, its pid recorded, then the root exits so
        // the grandchild is reparented — exactly the ffmpeg-wrapper leak shape.
        let script = format!("sleep 60 & echo $! > {}; wait", marker.display());
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", script.as_str()]));
        cmd.start().await.unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let text = loop {
            if let Ok(t) = std::fs::read_to_string(&marker) {
                if !t.trim().is_empty() {
                    break t;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild pid never written"
            );
            timeout(Duration::from_millis(50), async {
                tokio::task::yield_now().await
            })
            .await
            .ok();
        };
        let grandchild: u32 = text.trim().parse().expect("grandchild pid is numeric");
        let _ = std::fs::remove_file(&marker);

        let info = cmd.cancel().await.expect("cancel succeeds");
        assert_eq!(info.code, -1, "killed by SIGKILL");

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while pid_alive(grandchild) && std::time::Instant::now() < deadline {
            timeout(Duration::from_millis(50), async {
                tokio::task::yield_now().await
            })
            .await
            .ok();
        }
        assert!(
            !pid_alive(grandchild),
            "grandchild {grandchild} survived the group kill (leaked process)"
        );
    }

    /// (e) stderr far larger than the pipe capacity is bounded at STDERR_CAP and
    /// teardown still completes — the drain must keep consuming the overflow so
    /// the child never blocks, and must not buffer the whole stream.
    #[tokio::test]
    async fn oversized_stderr_is_capped_without_deadlock() {
        // 200000 'x' bytes on stderr: far past the OS pipe capacity, so a reader
        // that stops consuming at the cap would deadlock the child. The `\0` is a
        // shell escape for tr, not a NUL byte in argv.
        let script = "head -c 200000 /dev/zero | LC_ALL=C tr '\\0' x 1>&2";
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", script]));
        cmd.start().await.unwrap();

        let info = timeout(Duration::from_secs(10), cmd.wait()).await;
        let info = info.expect("teardown hung on an oversized stderr stream");
        let info = info.expect("wait returns an io::Result");
        assert_eq!(info.code, 0);
        assert_eq!(info.stderr.len(), STDERR_CAP);
        assert!(
            info.stderr.chars().all(|c| c == 'x'),
            "the cap keeps the FIRST 4096 bytes, exactly like Go's limitedWriter",
        );
    }

    /// (f) cancel/close are idempotent and safe before start.
    #[tokio::test]
    async fn cancel_and_close_are_idempotent() {
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", "sleep 30"]));
        let before_start = cmd.cancel().await;
        assert!(before_start.is_ok());
        assert_eq!(before_start.unwrap().code, -1);

        cmd.start().await.unwrap();
        assert!(cmd.cancel().await.is_ok());
        assert!(cmd.cancel().await.is_ok());
        assert!(cmd.close().await.is_ok());
        assert!(!cmd.is_available());
    }

    /// (g) `wait` on a naturally-exiting child is bounded and returns its code.
    #[tokio::test]
    async fn wait_reaps_a_naturally_exiting_child() {
        let mut cmd = new_ffmpeg_cmd("/bin/sh".to_string(), w(&["-c", "echo hi; exit 7"]));
        cmd.start().await.unwrap();
        let info = cmd.wait().await.unwrap();
        assert_eq!(info.code, 7);
        assert!(cmd.is_closed());
        assert!(!cmd.is_available());
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
