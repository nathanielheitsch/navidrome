//! BOOT-01 B2 CLI integration: `ServiceReady::prepare` injects the loaded
//! `FFmpegPath` into the nd-ffmpeg resolver before any filesystem setup.
//!
//! The ffmpeg resolver is a process-global single-state record, so scenarios
//! **cannot** share one test process — each runs in a fresh child process re-executed
//! from this binary (`--ignored --exact child_runner --nocapture`) with a scenario
//! name and paths supplied only through the child environment. The parent never
//! mutates its own environment and never runs a real ffmpeg; resolution only stats
//! files. The child receives a controlled `PATH` and a forced-empty
//! `ND_TEST_FFMPEG_PATH` through `Command::env`, nothing else.
//!
//! Child waits are bounded: readers run on `std::thread`s, the parent polls
//! `try_wait` against a deadline, and on timeout it terminates (`kill`) and reaps
//! (`wait`) the owned child rather than hanging. `std` only.
//!
//! Honesty: this file is `#[cfg(unix)]` because the resolver's `PATH` emulation and
//! the executable mode bit are Unix-specific. On non-Unix there are no tests — no
//! silently-passing skip. `#[tokio::test]` is used in-process for the ignored child
//! runner.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Scenario selector for the child.
const SCENARIO_ENV: &str = "ND_B2_SCENARIO";
/// Fixture root for the child.
const FIXTURE_ENV: &str = "ND_B2_FIXTURE";
/// Hard bound on a child run before terminate/reap.
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// A temp fixture tree removed on drop.
struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "nd-cli-configured-ffmpeg-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture root");
        Fixture(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    /// Create a subdirectory, return its path.
    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).expect("fixture dir");
        p
    }
    /// An executable stub file (mode 0755); never executed by resolution.
    fn stub(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("stub parent");
        }
        std::fs::write(&p, b"#!/bin/sh\nexit 0\n").expect("write stub");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
        p
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Spawn the child for a scenario with a controlled environment, run it with a
/// bounded wait, and return its combined output. Terminates and reaps on timeout.
fn run_child(
    scenario: &str,
    fixture: &Fixture,
    child_path: &str,
    extra: &[(&str, String)],
) -> String {
    let mut command = Command::new(std::env::current_exe().expect("current exe"));
    command
        .args(["--ignored", "--exact", "child_runner", "--nocapture"])
        .env(SCENARIO_ENV, scenario)
        .env(FIXTURE_ENV, fixture.path())
        .env("PATH", child_path)
        .env("ND_TEST_FFMPEG_PATH", "")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        command.env(k, v.clone());
    }

    let mut child: Child = command.spawn().expect("spawn child");

    // Drain both pipes on dedicated threads so a hung child cannot block the read.
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let (tx_out, rx_out) = mpsc::channel::<String>();
    let (tx_err, rx_err) = mpsc::channel::<String>();
    let reader_out = thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx_out.send(buf);
    });
    let reader_err = thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        let _ = tx_err.send(buf);
    });

    // Bounded wait: poll try_wait against a deadline; terminate + reap on timeout.
    let start = Instant::now();
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if start.elapsed() > CHILD_TIMEOUT {
            // Terminate the owned child, then reap it, before failing the test.
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader_out.join();
            let _ = reader_err.join();
            panic!("child scenario {scenario:?} exceeded {CHILD_TIMEOUT:?} and was terminated");
        }
        thread::sleep(Duration::from_millis(25));
    }
    let status = child.wait().expect("reap child");
    let out = rx_out
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let err = rx_err
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let _ = reader_out.join();
    let _ = reader_err.join();
    let text = format!("stdout: {out}\nstderr: {err}");
    assert!(
        status.success(),
        "child scenario {scenario:?} failed (exit {:?}):\n{text}",
        status.code()
    );
    // Non-vacuity: exactly one child test ran and passed, and it printed its marker.
    assert!(
        text.contains("test result: ok. 1 passed") && text.contains("0 failed"),
        "child scenario {scenario:?} ran no passing test:\n{text}"
    );
    assert!(
        text.contains(&format!("OK {scenario}")),
        "child scenario {scenario:?} did not report success:\n{text}"
    );
    text
}

#[test]
fn configured_binary_is_observed_through_cmd_path() {
    let fixture = Fixture::new("configured");
    let ffmpeg = fixture.stub("ffmpeg");
    // Empty PATH: only the explicitly configured binary can resolve.
    run_child(
        "configured",
        &fixture,
        "",
        &[("ND_B2_CONFIGURED", ffmpeg.to_string_lossy().into_owned())],
    );
}

#[test]
fn explicit_missing_binary_fails_before_setup_and_no_fallback() {
    let fixture = Fixture::new("missing");
    // A WORKING ffmpeg stub on the child PATH, and a configured path that is absent.
    let path_dir = fixture.dir("pathbin");
    let working = path_dir.join("ffmpeg");
    std::fs::write(&working, b"#!/bin/sh\nexit 0\n").expect("path stub");
    std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let missing = fixture
        .path()
        .join("nope/ffmpeg")
        .to_string_lossy()
        .into_owned();
    run_child(
        "missing",
        &fixture,
        path_dir.to_str().unwrap(),
        &[("ND_B2_CONFIGURED", missing)],
    );
}

#[test]
fn two_different_configured_states_conflict_with_one_db() {
    let fixture = Fixture::new("conflict");
    let first = fixture.stub("ffmpegA");
    let second = fixture.stub("ffmpegB");
    run_child(
        "conflict",
        &fixture,
        "",
        &[
            ("ND_B2_FIRST", first.to_string_lossy().into_owned()),
            ("ND_B2_SECOND", second.to_string_lossy().into_owned()),
        ],
    );
}

#[test]
fn empty_config_keeps_optional_tool_policy() {
    let fixture = Fixture::new("empty");
    // No FFmpegPath and no ffmpeg on PATH: a callable non-stream bootstrap is still
    // ready without a configured binary.
    run_child("empty", &fixture, "", &[]);
}

/// Invoked only by the parent via `--ignored --exact child_runner`. Builds a
/// sandbox, runs the real `preflight -> ServiceReady::prepare`, asserts, prints
/// `OK <scenario>`. Any assertion failure fails the child, which the parent detects.
#[tokio::test]
#[ignore = "run one scenario in a fresh child process via run_child"]
async fn child_runner() {
    use nd_cli::preflight::{preflight, PreflightOptions};
    use nd_cli::service_ready::{FFmpegCause, ReadyStage, ServiceReady};
    use nd_ffmpeg::ffmpeg_cmd;
    use std::error::Error;

    let scenario = std::env::var(SCENARIO_ENV).expect("scenario env");
    let fixture = PathBuf::from(std::env::var(FIXTURE_ENV).expect("fixture env"));
    let configured = std::env::var("ND_B2_CONFIGURED").unwrap_or_default();
    let first = std::env::var("ND_B2_FIRST").unwrap_or_default();
    let second = std::env::var("ND_B2_SECOND").unwrap_or_default();

    // Sandbox: music root + a TOML config carrying the launch values and FFmpegPath.
    std::fs::create_dir_all(fixture.join("music")).expect("music");
    let stamp = chrono::DateTime::parse_from_rfc3339("2024-06-01T10:00:00Z")
        .expect("stamp")
        .with_timezone(&chrono::Utc);
    let cwd = std::fs::canonicalize(&fixture).expect("cwd");

    let preflight_for = |ffmpeg_path: &str| {
        let mut toml = String::from(
            "MusicFolder = \"music\"\nDataFolder = \"data\"\nCacheFolder = \"cache\"\nPort = 4611\n\
             SessionTimeout = \"3600s\"\nAuthWindowLength = \"20s\"\nAuthRequestLimit = 5\n\
             TranscodingCacheSize = \"10kB\"\n",
        );
        if !ffmpeg_path.is_empty() {
            toml.push_str(&format!("FFmpegPath = \"{ffmpeg_path}\"\n"));
        }
        toml.push_str("DevAutoCreateAdminPassword = \"synthetic-dev-password\"\n");
        toml.push_str("[Transcoding]\nMaxConcurrent = 2\nMaxConcurrentPerUser = 1\n");
        let file = fixture.join("conf.toml");
        std::fs::write(&file, toml).expect("write conf");
        preflight(PreflightOptions {
            config_file: Some("conf.toml"),
            env: Some(Box::new(|_key| None)),
            startup_cwd: Some(&cwd),
            effective_uid: Some(1000),
            allow_zero_port: false,
        })
        .expect("preflight")
    };

    match scenario.as_str() {
        "configured" => {
            let outcome = preflight_for(&configured);
            let ready = ServiceReady::prepare(&outcome, stamp)
                .await
                .expect("configured binary resolves, prepare succeeds");
            assert!(ready.music_root.is_dir(), "ready state produced");
            // The configured path is observed through the real resolver entry point.
            assert_eq!(
                ffmpeg_cmd::cmd_path().expect("configured binary resolves"),
                configured,
                "cmd_path observes the configured FFmpegPath"
            );
            assert!(ffmpeg_cmd::is_available());
            println!("OK configured");
        }
        "missing" => {
            // Confirm a working ffmpeg is present on the child PATH first.
            let path_bin = std::env::var("PATH").expect("PATH");
            assert!(
                path_bin.contains("pathbin"),
                "child PATH points at a working ffmpeg dir"
            );
            let outcome = preflight_for(&configured);
            let err = ServiceReady::prepare(&outcome, stamp)
                .await
                .expect_err("explicit missing configured binary fails before setup");
            assert_eq!(err.stage(), ReadyStage::FFmpeg, "{err}");
            // Static cause class, no raw path in the diagnostic chain.
            let root_cause = err
                .source()
                .and_then(|s| s.downcast_ref::<FFmpegCause>())
                .expect("FFmpegCause source");
            assert!(
                matches!(root_cause, FFmpegCause::ConfiguredBinaryUnavailable),
                "static ConfiguredBinaryUnavailable class"
            );
            // Disclosure: the ServiceReadyError stage/cause chain carries only
            // static text — no raw configured path, fixture root, or task panic.
            let shown = format!("{err:?} | {}", err);
            for canary in [
                &configured,
                fixture.to_str().unwrap(),
                "pathbin",
                "JoinError",
                "panicked",
            ] {
                assert!(!shown.contains(canary), "safe static diagnostic: {shown}");
            }
            // No PATH fallback: the resolver still reports the missing configured
            // path as unavailable, not the working PATH binary.
            let cmd_err = ffmpeg_cmd::cmd_path().expect_err("no fallback to PATH");
            let msg = cmd_err.to_string();
            assert!(msg.contains(&configured), "reports configured path: {msg}");
            assert!(!msg.contains("pathbin"), "no PATH fallback: {msg}");
            // No data/cache/db created: prepare failed before filesystem setup.
            assert!(
                !fixture.join("data").exists() && !fixture.join("cache").exists(),
                "no data/cache created by a failed ffmpeg stage"
            );
            println!("OK missing");
        }
        "conflict" => {
            let outcome_a = preflight_for(&first);
            let ready = ServiceReady::prepare(&outcome_a, stamp)
                .await
                .expect("first configured state succeeds and sets up the db");
            assert!(ready.music_root.is_dir());
            let db = fixture.join("data").join("navidrome.db");
            assert!(db.exists(), "first prepare created the database");
            // Second, DIFFERENT configured path must conflict, not re-setup.
            let outcome_b = preflight_for(&second);
            let err = ServiceReady::prepare(&outcome_b, stamp)
                .await
                .expect_err("differing configured state conflicts");
            assert_eq!(err.stage(), ReadyStage::FFmpeg, "{err}");
            let root_cause = err
                .source()
                .and_then(|s| s.downcast_ref::<FFmpegCause>())
                .expect("FFmpegCause source");
            assert!(
                matches!(root_cause, FFmpegCause::ConflictingConfig),
                "static ConflictingConfig class, globals not reset"
            );
            // Winner preserved and no second DB setup: still the first path, and the
            // database from the first prepare remains (not replaced).
            assert_eq!(ffmpeg_cmd::cmd_path().expect("first path kept"), first);
            assert!(
                db.exists(),
                "database from the first prepare remains intact"
            );
            println!("OK conflict");
        }
        "empty" => {
            let outcome = preflight_for("");
            let ready = ServiceReady::prepare(&outcome, stamp)
                .await
                .expect("empty config keeps optional-tool policy: ready without ffmpeg");
            assert!(
                ready.music_root.is_dir(),
                "ready state produced without a configured binary"
            );
            println!("OK empty");
        }
        other => panic!("unknown scenario {other:?}"),
    }
    let _ = FFmpegCause::TaskFailed; // keep the enum referenced for API coverage
}
