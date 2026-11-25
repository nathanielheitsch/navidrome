//! BOOT-04 serve-path tests: a real subprocess, a real loopback socket, a real
//! graceful stop and reap.
//!
//! Every assertion here drives the actual `nd-cli` binary as a child process and
//! speaks HTTP/1.1 over a real socket to the port it reports — `exit 0` alone
//! would be a failure of this acceptance (`subplans/boot-api.md:265`
//! "Non-vacuity: subprocess must remain running and serve real request, then
//! graceful stop/reap").
//!
//! The file is `#![cfg(unix)]`: it drives unix signals (`SIGTERM`/`SIGINT`/
//! `SIGABRT`, the set Go registers) at a real child, and the supervisor's signal
//! source is itself unix-only. On non-unix platforms the supervisor reports a
//! missing stop source and exits nonzero, which is asserted in `tests/cli.rs`.
#![cfg(unix)]
//!
//! Isolation: an owned temp root per test, `Address = 127.0.0.1` with `--port 0`
//! so the OS picks an ephemeral loopback port, no real config, no credentials, no
//! TLS, and no non-loopback bind. Every wait is bounded; on any failure the guard
//! kills and reaps **only** its own child.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{kill_process, Pid, Signal};

/// Upper bound on any single wait in this file. A healthy run answers in
/// milliseconds, so hitting this is a real failure rather than a flake.
const BOUND: Duration = Duration::from_secs(10);

/// The port the sandbox configs name, used for "did anything bind here" checks.
const CONFIGURED_PORT: u16 = 4599;

/// Owns a spawned child and guarantees it is killed and reaped, so a failing
/// assertion cannot leave a server running or a zombie behind.
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Signal the child and wait for it to exit, bounded. Returns its exit code.
    fn terminate_and_reap(mut self, signal: Signal) -> i32 {
        let child = self.child.take().expect("child present");
        let pid = Pid::from_raw(child.id() as i32).expect("spawned child has a valid pid");
        if let Err(err) = kill_process(pid, signal) {
            panic!("kill pid {}: {err}", child.id());
        }
        bounded_wait(child, true)
    }

    /// Take the child's stderr pipe (only once).
    fn take_stderr(&mut self) -> ChildStderr {
        self.child
            .as_mut()
            .expect("child present")
            .stderr
            .take()
            .expect("stderr piped")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Only reached when a test failed before reaping: kill our own child
            // and reap it so nothing outlives the test.
            let pid = Pid::from_raw(child.id() as i32).expect("spawned child has a valid pid");
            let _ = kill_process(pid, Signal::KILL);
            let _ = child.wait();
        }
    }
}

/// Bounded wait for a child's exit code, reaping it.
///
/// The child is moved into the waiting thread (a scoped thread would block on
/// join, which is not bounded). If the bound is hit, the child is force-killed by
/// pid — the thread's `wait` then returns and reaps it — and the test fails.
///
/// `require_code` is true for a child we signalled with TERM/INT/ABRT: it must
/// exit with a code, so dying by signal there is a shutdown bug, not a pass.
fn bounded_wait(mut child: Child, require_code: bool) -> i32 {
    let pid = Pid::from_raw(child.id() as i32).expect("spawned child has a valid pid");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = child.wait().map(|status| status.code());
        let _ = tx.send(result);
    });

    let result = match rx.recv_timeout(BOUND) {
        Ok(result) => result,
        Err(_) => {
            // Bound hit: force-kill so the waiting thread can reap, then fail.
            let _ = kill_process(pid, Signal::KILL);
            let late = rx
                .recv_timeout(BOUND)
                .ok()
                .and_then(|result| result.ok())
                .flatten();
            panic!(
                "child {pid} did not exit within {BOUND:?}; forced SIGKILL, late result: {late:?}"
            );
        }
    };

    match result {
        Ok(Some(code)) => code,
        // Exited by signal with no code.
        Ok(None) if require_code => {
            panic!("child {pid} was killed by a signal instead of exiting with a code")
        }
        Ok(None) => -1,
        Err(err) => panic!("wait on child {pid}: {err}"),
    }
}

/// Read stderr to EOF in a background thread, so the main test can bound its
/// own waits instead of blocking on a pipe.
struct StderrTap {
    rx: mpsc::Receiver<Option<String>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StderrTap {
    fn new(stderr: ChildStderr) -> Self {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next() {
                    Some(Ok(line)) => {
                        if tx.send(Some(line)).is_err() {
                            return;
                        }
                    }
                    Some(Err(_)) | None => {
                        let _ = tx.send(None);
                        return;
                    }
                }
            }
        });
        Self {
            rx,
            handle: Some(handle),
        }
    }

    /// Next line within `bound`. Panics if nothing arrives in time.
    fn next(&self, bound: Duration) -> Option<String> {
        match self.rx.recv_timeout(bound) {
            Ok(line) => line,
            Err(_) => panic!("no stderr line within {bound:?}"),
        }
    }

    fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().ok();
        }
    }
}

/// An owned temp root, removed by `TempDir` on drop.
struct Sandbox {
    inner: tempfile::TempDir,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        Self {
            inner: tempfile::Builder::new()
                .prefix(&format!("nd-cli-serve-{tag}-"))
                .tempdir()
                .expect("isolated temp root"),
        }
    }
    fn root(&self) -> &Path {
        self.inner.path()
    }
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.root().join(name);
        std::fs::create_dir_all(&p).expect("create sandbox dir");
        p
    }
    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.root().join(name);
        std::fs::write(&p, contents).expect("write sandbox file");
        p
    }
    fn join(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }
}

/// A bootstrap-stage config: loopback, scanner explicitly off, isolated folders.
/// `Port` is a real number so "nothing bound" can be checked against it.
fn serve_toml(sandbox: &Sandbox) -> String {
    let music = sandbox.dir("music");
    format!(
        "MusicFolder = \"{}\"\nDataFolder = \"{}\"\nCacheFolder = \"{}\"\n\
         Address = \"127.0.0.1\"\nPort = {CONFIGURED_PORT}\n\
         [Scanner]\nEnabled = false\nScanOnStartup = false\n",
        music.display(),
        sandbox.join("data").display(),
        sandbox.join("cache").display()
    )
}

/// The minimal environment the child gets: `ND_*` is cleared so it cannot pick
/// up the test process's configuration.
fn child_command(config_path: &Path, extra: &[&str], nobanner: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nd-cli"));
    command.arg("--configfile").arg(config_path).args(extra);
    if nobanner {
        command.arg("--nobanner");
    }
    command
        .env_remove("ND_CONFIGFILE")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("TMPDIR", std::env::var("TMPDIR").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Spawn the real binary with an explicit config path.
fn spawn(config_path: &Path, extra: &[&str]) -> ChildGuard {
    ChildGuard::new(
        child_command(config_path, extra, true)
            .spawn()
            .expect("spawn nd-cli"),
    )
}

/// Wait for the readiness line and return the address it reports.
///
/// The port is never guessed: it comes from the child's own bound socket, which
/// is what "signal readiness only after binding the actual socket" means.
fn wait_ready(tap: &StderrTap) -> SocketAddr {
    let started = Instant::now();
    while started.elapsed() < BOUND {
        let remaining = BOUND - started.elapsed();
        match tap.next(remaining) {
            Some(line) => {
                if let Some(rest) = line.strip_prefix("ready: listening on http://") {
                    let addr = rest
                        .trim_end_matches('/')
                        .parse::<SocketAddr>()
                        .unwrap_or_else(|err| panic!("unparsable ready line {line:?}: {err}"));
                    assert_eq!(
                        addr.ip().to_string(),
                        "127.0.0.1",
                        "must bind loopback: {addr}"
                    );
                    assert_ne!(addr.port(), 0, "readiness reports the real bound port");
                    return addr;
                }
                if line.contains("nd-cli:") {
                    panic!("child reported a failure before readiness: {line}");
                }
            }
            None => panic!("child closed stderr without reporting readiness"),
        }
    }
    panic!("no readiness line within {BOUND:?}");
}

/// One real HTTP/1.1 request/response over a loopback socket, with socket-level
/// timeouts so no request can hang the test.
fn http_get(addr: SocketAddr, path: &str) -> String {
    let deadline = Instant::now() + BOUND;
    let mut stream = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            Ok(stream) => break stream,
            Err(err) => {
                assert!(Instant::now() < deadline, "connect to {addr}: {err}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    stream
        .set_read_timeout(Some(BOUND))
        .expect("read timeout on the test socket");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    stream.flush().expect("flush request");

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read reply");
    String::from_utf8_lossy(&buf).into_owned()
}

/// Read a pipe to EOF with a bounded wait, so a stuck child cannot block forever.
fn read_eof(mut pipe: ChildStdout) -> String {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let bytes = rx
        .recv_timeout(BOUND)
        .expect("stdout drained within the bound");
    handle.join().ok();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Drain stderr to EOF (bounded) and wait for the child's exit code.
fn drain_and_reap(mut guard: ChildGuard) -> (Option<i32>, String) {
    let tap = StderrTap::new(guard.take_stderr());
    let stdout = guard
        .child
        .as_mut()
        .expect("child present")
        .stdout
        .take()
        .expect("stdout piped");
    let _stdout_text = read_eof(stdout);

    let mut text = String::new();
    while let Some(line) = tap.rx.recv_timeout(BOUND).ok().flatten() {
        text.push_str(&line);
        text.push('\n');
    }
    tap.join();

    let child = guard.child.take().expect("child present");
    let code = bounded_wait(child, false);
    (Some(code), text)
}

/// The port is free again: a fresh bind on the exact address succeeds.
fn assert_port_free(addr: SocketAddr) {
    match TcpListener::bind(addr) {
        Ok(listener) => drop(listener),
        Err(err) => panic!("port {addr} still held after shutdown: {err}"),
    }
}

/// Nothing listens on the configured port.
fn assert_not_listening(addr: SocketAddr) {
    assert!(
        TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(),
        "nothing should be listening on {addr}"
    );
}

fn configured_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], CONFIGURED_PORT))
}

// ---------------------------------------------------------------------------
// The serve path really serves, then stops gracefully and releases the port
// ---------------------------------------------------------------------------

#[test]
fn subprocess_serves_ping_then_exits_zero_on_sigterm_and_releases_the_port() {
    let sandbox = Sandbox::new("serve-ping");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    // Non-vacuity: the process is alive and answers a real request.
    let reply = http_get(addr, "/ping");
    assert!(
        reply.starts_with("HTTP/1.1 200"),
        "ping must be 200 over a real socket: {reply:?}"
    );
    assert!(
        reply
            .to_ascii_lowercase()
            .contains("content-type: text/plain"),
        "ping must declare text/plain: {reply:?}"
    );
    assert!(reply.ends_with('.'), "Go's ping body is `.`: {reply:?}");

    // Bootstrap effects exist only now — after readiness was reported.
    assert!(
        sandbox.join("data").exists(),
        "bootstrap created the data dir"
    );

    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0, "a signalled, joined drain is exit 0");
    assert_port_free(addr);
}

/// Terminate **immediately** after the readiness line, with no request in
/// between. Signal handlers are registered before startup work begins, so a stop
/// issued at the first instant of readiness must still produce a joined drain and
/// release the port — not a hang, and not an exit that pretends to be graceful.
#[test]
fn terminate_immediately_after_ready_line_drains_cleanly() {
    let sandbox = Sandbox::new("stop-at-ready");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());

    // The readiness line is the synchronization point: nothing is requested and
    // nothing is slept — the stop is sent the moment the socket is reported.
    let addr = wait_ready(&tap);
    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();

    assert_eq!(
        code, 0,
        "a stop at the first moment of readiness still drains"
    );
    assert_port_free(addr);
    // The run was real: bootstrap completed, so the data directory exists.
    assert!(
        sandbox.join("data").exists(),
        "bootstrap ran before readiness"
    );
}

/// `SIGINT` is in the listened set (`cmd/root.go:114-121`) and drains the same way.
#[test]
fn sigint_also_drains_gracefully() {
    let sandbox = Sandbox::new("serve-int");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));

    let code = child.terminate_and_reap(Signal::INT);
    tap.join();
    assert_eq!(
        code, 0,
        "SIGINT is a normal shutdown, like Go's mainContext"
    );
    assert_port_free(addr);
}

/// `SIGABRT` is part of the ported signal set too, so it must also drain.
#[test]
fn sigabrt_is_listened_for_and_drains() {
    let sandbox = Sandbox::new("serve-abrt");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));

    let code = child.terminate_and_reap(Signal::ABORT);
    tap.join();
    assert_eq!(code, 0, "SIGABRT is in Go's signal set, so it drains");
    assert_port_free(addr);
}

/// `--nobanner` suppresses the banner; without it stdout carries it.
#[test]
fn banner_is_suppressed_by_nobanner_and_printed_otherwise() {
    let sandbox = Sandbox::new("banner");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));

    // With --nobanner: stdout stays empty.
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);
    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0);

    // Without --nobanner: stdout carries the banner. Rust block-buffers stdout
    // when it is a pipe, so the banner is read after the child exits (EOF), not
    // while it is serving.
    let mut command = child_command(&config, &["--port", "0"], false);
    let mut child = ChildGuard::new(command.spawn().expect("spawn"));
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);
    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    let stdout = child
        .child
        .as_mut()
        .expect("child present")
        .stdout
        .take()
        .expect("stdout piped");
    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    let banner = read_eof(stdout);
    assert_eq!(code, 0);
    assert!(
        banner.contains("Navidrome"),
        "stdout should carry the banner when --nobanner is absent: {banner:?}"
    );
}

/// Unmatched paths get Go's `http.NotFound` wire from the shell.
#[test]
fn unmatched_path_is_the_go_not_found_wire() {
    let sandbox = Sandbox::new("notfound");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let mut child = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    let reply = http_get(addr, "/api/song/1");
    assert!(reply.starts_with("HTTP/1.1 404"), "expected 404: {reply:?}");
    assert!(reply.contains("404 page not found"), "wire: {reply:?}");

    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0);
    assert_port_free(addr);
}

// ---------------------------------------------------------------------------
// Invalid preflight / refused capability: nonzero, and no bind at all
// ---------------------------------------------------------------------------

#[test]
fn invalid_config_exits_one_and_never_binds_or_writes() {
    let sandbox = Sandbox::new("bad-config");
    let music = sandbox.dir("music");
    // AuthRequestLimit = 0 is invalid (`boot-api.md:257` "auth limit<=0 bootfails").
    let config = sandbox.file(
        "serve.toml",
        &format!(
            "MusicFolder = \"{}\"\nDataFolder = \"{}\"\nCacheFolder = \"{}\"\n\
             Address = \"127.0.0.1\"\nPort = {CONFIGURED_PORT}\nAuthRequestLimit = 0\n\
             [Scanner]\nEnabled = false\nScanOnStartup = false\n",
            music.display(),
            sandbox.join("data").display(),
            sandbox.join("cache").display()
        ),
    );

    let (code, seen) = drain_and_reap(spawn(&config, &[]));
    assert_eq!(code, Some(1), "bootstrap failure exits 1; output: {seen}");
    assert!(seen.contains("nd-cli:"), "the failure is explained: {seen}");
    assert!(
        !seen.contains("ready: listening on"),
        "an invalid config must never report readiness: {seen}"
    );
    assert_not_listening(configured_addr());
    // The refusal precedes every write.
    assert!(
        !sandbox.join("data").exists(),
        "no data directory on failure"
    );
    assert!(
        !sandbox.join("cache").exists(),
        "no cache directory on failure"
    );
}

/// An enabled scanner is refused rather than silently ignored, before any bind.
#[test]
fn enabled_scanner_exits_one_names_the_capability_and_never_binds() {
    let sandbox = Sandbox::new("scanner-refuse");
    let music = sandbox.dir("music");
    // Scanner defaults are Enabled = true / ScanOnStartup = true.
    let config = sandbox.file(
        "serve.toml",
        &format!(
            "MusicFolder = \"{}\"\nDataFolder = \"{}\"\nCacheFolder = \"{}\"\n\
             Address = \"127.0.0.1\"\nPort = {CONFIGURED_PORT}\n",
            music.display(),
            sandbox.join("data").display(),
            sandbox.join("cache").display()
        ),
    );

    let (code, seen) = drain_and_reap(spawn(&config, &[]));
    assert_eq!(code, Some(1), "scanner refusal exits 1; output: {seen}");
    assert!(
        seen.contains("Scanner"),
        "the refusal names the scanner: {seen}"
    );
    assert!(
        !seen.contains("ready: listening on"),
        "no readiness for a refused capability: {seen}"
    );
    assert!(
        !sandbox.join("data").exists(),
        "a refusal creates no data directory"
    );
    assert_not_listening(configured_addr());
}

/// TLS is refused before bind — the transport is never downgraded to TCP.
#[test]
fn tls_config_exits_one_and_never_binds() {
    let sandbox = Sandbox::new("tls-refuse");
    let music = sandbox.dir("music");
    let config = sandbox.file(
        "serve.toml",
        &format!(
            "MusicFolder = \"{}\"\nDataFolder = \"{}\"\nCacheFolder = \"{}\"\n\
             Address = \"127.0.0.1\"\nPort = {CONFIGURED_PORT}\nTLSCert = \"{}\"\nTLSKey = \"{}\"\n\
             [Scanner]\nEnabled = false\nScanOnStartup = false\n",
            music.display(),
            sandbox.join("data").display(),
            sandbox.join("cache").display(),
            sandbox.join("absent.pem").display(),
            sandbox.join("absent.key").display()
        ),
    );

    let (code, seen) = drain_and_reap(spawn(&config, &[]));
    assert_eq!(code, Some(1), "TLS refusal exits 1; output: {seen}");
    assert!(
        !seen.contains("ready: listening on"),
        "TLS must never report ready: {seen}"
    );
    assert!(
        !sandbox.join("data").exists(),
        "a TLS refusal creates nothing"
    );
    assert_not_listening(configured_addr());
}

/// A missing config file is a bootstrap failure (exit 1), never exit 2.
#[test]
fn missing_explicit_config_exits_one() {
    let sandbox = Sandbox::new("missing-config");
    let missing = sandbox.join("nope.toml");
    let (code, seen) = drain_and_reap(spawn(&missing, &[]));
    assert_eq!(
        code,
        Some(1),
        "a missing explicit config exits 1; output: {seen}"
    );
    assert!(!sandbox.join("data").exists(), "nothing is created");
}

// ---------------------------------------------------------------------------
// Usage and side-effect-free answers, as a subprocess
// ---------------------------------------------------------------------------

#[test]
fn unknown_flag_exits_two_and_touches_nothing() {
    let sandbox = Sandbox::new("usage");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let (code, seen) = drain_and_reap(spawn(&config, &["--bogus"]));
    assert_eq!(code, Some(2), "usage error exits 2; output: {seen}");
    assert!(
        !sandbox.join("data").exists(),
        "a usage error creates nothing"
    );
    assert!(
        !sandbox.join("cache").exists(),
        "a usage error creates nothing"
    );
}

#[test]
fn version_and_help_exit_zero_print_output_and_create_nothing() {
    for flag in ["--version", "--help"] {
        let sandbox = Sandbox::new("answer");
        let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
        let mut command = child_command(&config, &[flag], true);
        let mut child = ChildGuard::new(command.spawn().expect("spawn"));

        let tap = StderrTap::new(child.take_stderr());
        let stdout = child
            .child
            .as_mut()
            .expect("child present")
            .stdout
            .take()
            .expect("stdout piped");
        let output = read_eof(stdout);
        tap.join();

        let inner = child.child.take().expect("child present");
        let code = Some(bounded_wait(inner, false));
        assert_eq!(code, Some(0), "{flag} exits 0");
        assert!(!output.is_empty(), "{flag} prints something");
        assert!(
            !sandbox.join("data").exists(),
            "{flag} creates no data directory"
        );
        assert!(
            !sandbox.join("cache").exists(),
            "{flag} creates no cache directory"
        );
    }
}

/// An out-of-range port override is a bootstrap failure, not a silent fallback.
#[test]
fn out_of_range_port_exits_one_without_binding() {
    let sandbox = Sandbox::new("bad-port");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));
    let (code, seen) = drain_and_reap(spawn(&config, &["--port", "99999"]));
    assert_eq!(
        code,
        Some(1),
        "an out-of-range port exits 1; output: {seen}"
    );
    assert!(
        !seen.contains("ready: listening on"),
        "a bad port must not report ready: {seen}"
    );
    assert!(
        !sandbox.join("data").exists(),
        "an override failure creates nothing"
    );
    assert_not_listening(configured_addr());
}
// ---------------------------------------------------------------------------
// Config precedence and restart, through the real binary
// ---------------------------------------------------------------------------

/// An explicit `--configfile` beats `ND_CONFIGFILE`, matching Go's existing-file
/// rule: the flag names an existing path, so the environment value is ignored.
#[test]
fn explicit_configfile_beats_nd_configfile() {
    let sandbox = Sandbox::new("precedence");
    let music = sandbox.dir("music");
    // The env-named config is deliberately broken: if it were honored, the run
    // would fail instead of serving, so a successful run proves the flag won.
    let poisoned = sandbox.file(
        "env.toml",
        &format!(
            "MusicFolder = \"{music}\"\nAuthRequestLimit = 0\n",
            music = music.display()
        ),
    );
    let good = sandbox.file("serve.toml", &serve_toml(&sandbox));

    let mut command = child_command(&good, &["--port", "0"], true);
    command.env("ND_CONFIGFILE", &poisoned);
    let mut child = ChildGuard::new(command.spawn().expect("spawn"));
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0, "the explicit config was used, not ND_CONFIGFILE");
    assert_port_free(addr);
}

/// `ND_CONFIGFILE` is honored when no explicit path is supplied.
#[test]
fn nd_configfile_is_honored_without_an_explicit_flag() {
    let sandbox = Sandbox::new("env-config");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));

    let mut command = Command::new(env!("CARGO_BIN_EXE_nd-cli"));
    command
        .arg("--nobanner")
        .arg("--port")
        .arg("0")
        .env_clear()
        .env("ND_CONFIGFILE", &config)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("TMPDIR", std::env::var("TMPDIR").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard::new(command.spawn().expect("spawn"));
    let tap = StderrTap::new(child.take_stderr());
    let addr = wait_ready(&tap);

    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    let code = child.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0);
    assert_port_free(addr);
}

/// A restart against the same data directory comes back up and serves: the
/// committed auth/session material is reused from the database rather than
/// regenerated into a fresh state (`boot-api.md:272` "Restart must reuse
/// auth/session/public secrets").
#[test]
fn restart_reuses_the_persisted_state_and_serves_again() {
    let sandbox = Sandbox::new("restart");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));

    let mut first = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(first.take_stderr());
    let addr = wait_ready(&tap);
    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    let code = first.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0, "first run drains cleanly");
    assert_port_free(addr);

    // Second run over the same DataFolder: it must start from the existing
    // database and serve, not fail because state already exists.
    let mut second = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(second.take_stderr());
    let addr = wait_ready(&tap);
    let reply = http_get(addr, "/ping");
    assert!(
        reply.starts_with("HTTP/1.1 200"),
        "restart must serve: {reply:?}"
    );
    let code = second.terminate_and_reap(Signal::TERM);
    tap.join();
    assert_eq!(code, 0, "second run drains cleanly");
    assert_port_free(addr);
}

/// Read a committed property value from the sandbox database (read-only).
///
/// `JWTSecret`/`JWTPublicSecret` are stored encrypted; comparing the stored
/// ciphertext across a restart proves the committed secret was **reused** rather
/// than regenerated. Nothing here prints a value: the test compares, and only
/// the property *name* can appear in a failure message.
fn committed_property(sandbox: &Sandbox, name: &str) -> String {
    let db = sandbox.join("data").join("navidrome.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|err| panic!("open {} read-only: {err}", db.display()));
    conn.query_row("SELECT value FROM property WHERE id = ?1", [name], |row| {
        row.get::<_, String>(0)
    })
    .unwrap_or_else(|err| panic!("property {name} must exist in the committed DB: {err}"))
}

/// A restart reuses the committed auth/session/public secrets: the stored
/// ciphertext is byte-identical after the second run, so sessions issued before
/// a restart would still validate.
#[test]
fn restart_reuses_committed_secrets_unchanged() {
    let sandbox = Sandbox::new("restart-secrets");
    let config = sandbox.file("serve.toml", &serve_toml(&sandbox));

    let mut first = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(first.take_stderr());
    let addr = wait_ready(&tap);
    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    assert_eq!(first.terminate_and_reap(Signal::TERM), 0);
    tap.join();

    let session_before = committed_property(&sandbox, "JWTSecret");
    let public_before = committed_property(&sandbox, "JWTPublicSecret");
    assert!(
        !session_before.is_empty() && !public_before.is_empty(),
        "secrets are committed"
    );

    let mut second = spawn(&config, &["--port", "0"]);
    let tap = StderrTap::new(second.take_stderr());
    let addr = wait_ready(&tap);
    assert!(http_get(addr, "/ping").starts_with("HTTP/1.1 200"));
    assert_eq!(second.terminate_and_reap(Signal::TERM), 0);
    tap.join();

    assert_eq!(
        committed_property(&sandbox, "JWTSecret"),
        session_before,
        "JWTSecret must be reused, not regenerated"
    );
    assert_eq!(
        committed_property(&sandbox, "JWTPublicSecret"),
        public_before,
        "JWTPublicSecret must be reused, not regenerated"
    );
}
