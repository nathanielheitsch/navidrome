//! B2 boot-config injection tests (`initialize_from_loaded_config`).
//!
//! The resolver state (`RESOLVER`) is process-global, so scenarios **cannot** run
//! in one shared test process — a second scenario would observe the first
//! scenario's cached resolution. Every scenario therefore runs in a fresh child
//! process: the parent builds synthetic fixtures, then re-executes this test
//! binary with `--ignored --exact child_scenario_runner --nocapture` and a
//! scenario name in the environment. `child_scenario_runner` is `#[ignore]` so it
//! never runs in the shared process; it is invoked only via the subprocess.
//!
//! Honesty notes:
//! * The resolver's `PATH` emulation is Unix (`look_path` splits on `:`), and
//!   fixtures set a Unix mode bit, so this file is `#[cfg(unix)]`. On non-Unix no
//!   tests are defined — there is no silently-passing skip.
//! * **No system tool is executed and the parent environment is never mutated.**
//!   Resolution (`initialize`/`cmd_path`/`is_available`/`is_probe_available`) only
//!   stats files via `look_path`; it never spawns a process, and `version()` (which
//!   would spawn) is deliberately not exercised. The child receives a controlled
//!   environment through `Command::env` only; `ND_TEST_FFMPEG_PATH` is forced empty
//!   so it cannot leak from the parent.
//! * Fixtures are real executable files (mode `0o755`) standing in for ffmpeg /
//!   ffprobe, so no external ffmpeg is required. They are never executed here.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nd_conf::Config;

/// Scenario selector passed to the child.
const SCENARIO_ENV: &str = "ND_B2_SCENARIO";
/// Fixture root passed to the child.
const FIXTURE_ENV: &str = "ND_B2_FIXTURE_DIR";

// ---------------------------------------------------------------------------
// parent-side fixture helpers
// ---------------------------------------------------------------------------

/// A temp fixture tree that persists until this guard is dropped.
struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nd-ffmpeg-b2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        FixtureDir(dir)
    }

    /// Write an executable stub file named `name`, return its absolute path.
    fn stub(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make stub executable");
        path
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Re-execute this test binary running only the ignored child runner with the
/// given scenario and a controlled environment. Returns the child's combined
/// stdout. Asserts the child ran exactly one test and it passed, so a scenario
/// that failed to launch cannot pass vacuously.
fn run_scenario(scenario: &str, fixture: &FixtureDir, child_path: &str) -> String {
    let out = Command::new(std::env::current_exe().expect("current exe"))
        .args([
            "--ignored",
            "--exact",
            "child_scenario_runner",
            "--nocapture",
        ])
        .env(SCENARIO_ENV, scenario)
        .env(FIXTURE_ENV, fixture.path())
        // Deterministic PATH supplied by the parent; never the real system PATH.
        .env("PATH", child_path)
        // Neutralize the test override so it cannot leak from the parent env.
        .env("ND_TEST_FFMPEG_PATH", "")
        .output()
        .expect("spawn child scenario");
    let text = format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "child scenario {scenario:?} failed (exit {:?}):\n{text}",
        out.status.code()
    );
    // Non-vacuity: the child ran the one scenario test and it passed.
    assert!(
        text.contains("test result: ok. 1 passed") && text.contains("0 failed"),
        "child scenario {scenario:?} did not run exactly one passing test:\n{text}"
    );
    text
}

fn cfg_with(path: &str) -> Config {
    Config {
        ffmpeg_path: path.to_string(),
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------

#[test]
fn configured_path_is_used_not_default() {
    let fixture = FixtureDir::new("configured");
    let ffmpeg = fixture.stub("ffmpeg");
    // The child receives an empty PATH, so only the explicitly configured binary
    // can resolve; a default or stray PATH binary would fail the child assert.
    let text = run_scenario("configured_used", &fixture, "");
    assert!(
        text.contains("OK configured_used"),
        "child reported success: {text}"
    );
    let _ = ffmpeg;
}

#[test]
fn missing_configured_binary_never_falls_back_to_path() {
    let fixture = FixtureDir::new("missing");
    // A WORKING ffmpeg present on PATH, and a configured path that does not exist.
    let on_path_dir = fixture.path().join("pathbin");
    std::fs::create_dir_all(&on_path_dir).expect("path dir");
    let path_bin = on_path_dir.join("ffmpeg");
    std::fs::write(&path_bin, b"#!/bin/sh\nexit 0\n").expect("path ffmpeg");
    std::fs::set_permissions(&path_bin, std::fs::Permissions::from_mode(0o755))
        .expect("chmod path ffmpeg");
    // Configured path is an absolute nonexistent file under the fixture root.
    let missing = fixture
        .path()
        .join("nope/ffmpeg")
        .to_str()
        .unwrap()
        .to_string();

    // Child env PATH points at the working binary dir, proving no fallback.
    let out = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--ignored",
            "--exact",
            "child_scenario_runner",
            "--nocapture",
        ])
        .env(SCENARIO_ENV, "missing_no_fallback")
        .env(FIXTURE_ENV, fixture.path())
        .env("PATH", on_path_dir.to_str().unwrap())
        .env("ND_TEST_FFMPEG_PATH", "")
        .env("ND_B2_CONFIGURED", &missing)
        .output()
        .expect("spawn");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "missing-fallback scenario failed:\n{text}"
    );
    assert!(
        text.contains("test result: ok. 1 passed") && text.contains("OK missing_no_fallback"),
        "missing-fallback scenario did not assert:\n{text}"
    );
}

#[test]
fn identical_repeat_init_is_accepted() {
    let fixture = FixtureDir::new("identical");
    let ffmpeg = fixture.stub("ffmpeg");
    // Empty PATH: only the configured binary resolves, so a stray PATH hit cannot
    // make the scenario pass vacuously.
    let text = run_scenario("identical_repeat", &fixture, "");
    assert!(
        text.contains("OK identical_repeat"),
        "identical-repeat scenario:\n{text}"
    );
    let _ = ffmpeg;
}

#[test]
fn conflicting_path_is_refused_and_winner_kept() {
    let fixture = FixtureDir::new("conflict");
    let first = fixture.stub("ffmpeg");
    let second = fixture.stub("ffmpeg2");
    let out = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--ignored",
            "--exact",
            "child_scenario_runner",
            "--nocapture",
        ])
        .env(SCENARIO_ENV, "conflict")
        .env(FIXTURE_ENV, fixture.path())
        .env("PATH", "")
        .env("ND_TEST_FFMPEG_PATH", "")
        .env("ND_B2_FIRST", &first)
        .env("ND_B2_SECOND", &second)
        .output()
        .expect("spawn conflict");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "conflict scenario failed:\n{text}");
    assert!(
        text.contains("test result: ok. 1 passed") && text.contains("OK conflict"),
        "conflict scenario did not assert:\n{text}"
    );
}

#[test]
fn lookup_before_init_with_configured_path_is_late_initialization() {
    let fixture = FixtureDir::new("late");
    let ffmpeg = fixture.stub("ffmpeg");
    let out = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--ignored",
            "--exact",
            "child_scenario_runner",
            "--nocapture",
        ])
        .env(SCENARIO_ENV, "late_init")
        .env(FIXTURE_ENV, fixture.path())
        .env("PATH", "")
        .env("ND_TEST_FFMPEG_PATH", "")
        .env("ND_B2_CONFIGURED", &ffmpeg)
        .output()
        .expect("spawn late");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "late-init scenario failed:\n{text}");
    assert!(
        text.contains("test result: ok. 1 passed") && text.contains("OK late_init"),
        "late-init scenario did not assert:\n{text}"
    );
}

#[test]
fn empty_config_after_lazy_lookup_is_compatible() {
    let fixture = FixtureDir::new("compat");
    // No ffmpeg on PATH so the lazy lookup yields an unavailable legacy pair, then
    // an empty-config init must accept the same resolution (not conflict).
    let text = run_scenario("empty_after_lazy", &fixture, "");
    assert!(
        text.contains("OK empty_after_lazy"),
        "empty-config-after-lazy scenario:\n{text}"
    );
}

#[test]
fn probe_sibling_is_derived_from_configured_dir() {
    // Fixture has both ffmpeg and ffprobe -> available; and a dir with only ffmpeg
    // -> unavailable. Two children, one each.
    let with_probe = FixtureDir::new("probe-yes");
    with_probe.stub("ffmpeg");
    with_probe.stub("ffprobe");
    let text = run_scenario("probe_available", &with_probe, "");
    assert!(
        text.contains("OK probe_available"),
        "probe-available scenario:\n{text}"
    );

    let no_probe = FixtureDir::new("probe-no");
    no_probe.stub("ffmpeg"); // no ffprobe sibling
    let text = run_scenario("probe_missing", &no_probe, "");
    assert!(
        text.contains("OK probe_missing"),
        "probe-missing scenario:\n{text}"
    );
}

#[test]
fn concurrent_init_and_resolution_is_consistent() {
    let fixture = FixtureDir::new("concurrent");
    let text = run_scenario("concurrent", &fixture, "");
    assert!(
        text.contains("OK concurrent"),
        "concurrency scenario:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// child runner (ignored in the shared process; runs one scenario per child)
// ---------------------------------------------------------------------------

/// Invoked only by the parent via `--ignored --exact child_scenario_runner`. Reads
/// the scenario name and fixture/extra paths from the environment, then runs the
/// real exported API and asserts. Any assertion failure fails the child, which the
/// parent detects.
#[test]
#[ignore = "run in a fresh child process by the parent scenario tests"]
fn child_scenario_runner() {
    use nd_ffmpeg::ffmpeg_cmd::{
        cmd_path, initialize_from_loaded_config, is_available, is_probe_available,
    };

    let scenario = std::env::var(SCENARIO_ENV).expect("scenario env set by parent");
    let fixture = std::env::var(FIXTURE_ENV).expect("fixture dir env set by parent");
    let configured = std::env::var("ND_B2_CONFIGURED").unwrap_or_default();
    let first = std::env::var("ND_B2_FIRST").unwrap_or_default();
    let second = std::env::var("ND_B2_SECOND").unwrap_or_default();

    match scenario.as_str() {
        "configured_used" => {
            let path = fixture_dir(&fixture).join("ffmpeg");
            initialize_from_loaded_config(&cfg_with(path.to_str().unwrap())).expect("init ok");
            let resolved = cmd_path().expect("configured binary resolves");
            assert_eq!(
                resolved,
                path.to_str().unwrap(),
                "cmd_path uses configured path"
            );
            assert!(is_available());
            println!("OK configured_used");
        }
        "missing_no_fallback" => {
            // A working ffmpeg is on PATH; a configured missing path must NOT fall
            // back to it.
            assert!(
                std::env::var("PATH")
                    .map(|p| p.contains("pathbin"))
                    .unwrap_or(false),
                "child PATH points at the working binary dir"
            );
            initialize_from_loaded_config(&cfg_with(&configured)).expect("init records resolution");
            assert!(!is_available(), "missing configured binary is unavailable");
            let err = cmd_path().expect_err("missing configured binary is not available");
            let msg = err.to_string();
            assert!(
                msg.contains(&configured),
                "error carries the configured path: {msg}"
            );
            assert!(
                !msg.contains("pathbin"),
                "did NOT fall back to the PATH binary: {msg}"
            );
            println!("OK missing_no_fallback");
        }
        "identical_repeat" => {
            let path = fixture_dir(&fixture).join("ffmpeg");
            let cfg = cfg_with(path.to_str().unwrap());
            initialize_from_loaded_config(&cfg).expect("first init ok");
            initialize_from_loaded_config(&cfg).expect("identical repeat accepted");
            assert_eq!(cmd_path().unwrap(), path.to_str().unwrap());
            println!("OK identical_repeat");
        }
        "conflict" => {
            initialize_from_loaded_config(&cfg_with(&first)).expect("first init ok");
            let err = initialize_from_loaded_config(&cfg_with(&second))
                .expect_err("conflicting path is refused");
            assert_eq!(
                format!("{err:?}"),
                "ConflictingConfig",
                "static conflict class, no path disclosed"
            );
            // Winner preserved: the first path is still the resolution.
            assert_eq!(cmd_path().unwrap(), first, "first path kept");
            println!("OK conflict");
        }
        "late_init" => {
            // Force a lazy legacy resolution first (caches LazyEmpty).
            let _lazy = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
            let err = initialize_from_loaded_config(&cfg_with(&configured))
                .expect_err("configured path cannot be retrofitted after a lazy lookup");
            assert_eq!(
                format!("{err:?}"),
                "LateInitialization",
                "static late class"
            );
            println!("OK late_init");
        }
        "empty_after_lazy" => {
            let _lazy = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
            initialize_from_loaded_config(&cfg_with(""))
                .expect("empty config is compatible with the same lazy resolution");
            println!("OK empty_after_lazy");
        }
        "probe_available" => {
            let path = fixture_dir(&fixture).join("ffmpeg");
            initialize_from_loaded_config(&cfg_with(path.to_str().unwrap())).expect("init ok");
            assert!(
                is_probe_available(),
                "ffprobe sibling in the configured dir"
            );
            println!("OK probe_available");
        }
        "probe_missing" => {
            let path = fixture_dir(&fixture).join("ffmpeg");
            initialize_from_loaded_config(&cfg_with(path.to_str().unwrap())).expect("init ok");
            assert!(
                !is_probe_available(),
                "no ffprobe sibling beside configured ffmpeg"
            );
            println!("OK probe_missing");
        }
        "concurrent" => {
            // Concurrent init + resolution must never deadlock and must settle on
            // ONE winner visible to every thread. Whoever reaches the lock first
            // wins: if an `initialize` wins, the configured path is the winner and
            // every later identical init is accepted; if a lazy `ffmpeg_cmd` wins
            // first (empty PATH here, so an unavailable legacy pair), a later
            // configured init is refused with `LateInitialization` and that legacy
            // winner persists. Both outcomes are legitimate — the invariant is a
            // single consistent winner and no silent swap to a different path.
            let path = fixture_dir(&fixture).join("ffmpeg");
            let target = path.to_str().unwrap().to_string();
            let mut handles = Vec::new();
            for _ in 0..8 {
                let p = target.clone();
                handles.push(std::thread::spawn(move || {
                    // Both a resolver read and an init attempt, concurrently.
                    let init = initialize_from_loaded_config(&cfg_with(&p));
                    let pair = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
                    (init, pair)
                }));
            }
            let mut winner: Option<(String, bool)> = None;
            for h in handles {
                let (init, pair) = h.join().expect("thread completed, no deadlock/panic");
                // Every init is accepted (identical loaded path) or honestly
                // refused as late; never a silent accept of a different path and
                // never a conflict against the same configured path.
                let class = init
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_default();
                assert!(
                    init.is_ok() || class == "LateInitialization",
                    "init is Ok or LateInitialization, never silent/conflicting"
                );
                match &winner {
                    None => winner = Some(pair),
                    Some(w) => assert_eq!(*w, pair, "all threads agree on one winner"),
                }
            }
            // The final read agrees with the winner every thread observed.
            assert_eq!(
                nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd(),
                winner.expect("at least one thread observed a winner"),
                "final resolution matches the consistent winner"
            );
            println!("OK concurrent");
        }
        other => panic!("unknown scenario {other:?}"),
    }
}

/// Resolve the fixture dir path from the env string.
fn fixture_dir(value: &str) -> PathBuf {
    PathBuf::from(value)
}
