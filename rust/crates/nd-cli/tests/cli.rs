//! BOOT-04 CLI wiring tests: argument parsing, exit codes, and the resolution
//! gate, all in-process with injected environment and sandboxed temp roots.
//!
//! Scope: no real process environment, no real cwd, no non-loopback bind, no
//! real config file, no credentials. Subprocess behavior is covered by
//! `tests/serve.rs`.

use std::path::{Path, PathBuf};

use nd_cli::args::{
    parse, usage_text, version_text, Action, Override, ADDRESS_FLAG, EXIT_FAILURE, EXIT_OK,
    EXIT_USAGE, PORT_FLAG, UNIMPLEMENTED_FLAGS, UNIMPLEMENTED_SUBCOMMANDS,
};
use nd_cli::preflight::{preflight, PreflightError, PreflightOptions};
use nd_cli::supervisor::{
    validate_resolution, CapabilityRefusal, InactiveServices, ServeError, ServeOutcome,
    ServeReport, StopReason,
};

/// An isolated root; every path the CLI touches lives under it.
struct Sandbox {
    inner: tempfile::TempDir,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        Self {
            inner: tempfile::Builder::new()
                .prefix(&format!("nd-cli-args-{tag}-"))
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
    /// `<root>/<name>` without creating anything.
    fn join(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// A bootstrap-stage config: loopback, ephemeral port, scanner off, isolated
/// data/cache folders.
fn serve_toml(music: &str, data: &str, cache: &str) -> String {
    format!(
        "MusicFolder = \"{music}\"\nDataFolder = \"{data}\"\nCacheFolder = \"{cache}\"\n\
         Address = \"127.0.0.1\"\nPort = 0\n\
         [Scanner]\nEnabled = false\nScanOnStartup = false\n"
    )
}

// ---------------------------------------------------------------------------
// Help / version: exit 0 with no side effects
// ---------------------------------------------------------------------------

#[test]
fn help_and_version_are_exit_zero_and_short_circuit() {
    for argv in [args(&["--help"]), args(&["-h"]), args(&["--version"])] {
        let action = parse(&argv);
        assert_eq!(action.exit_code(), EXIT_OK, "{argv:?} must exit 0");
    }
    assert_eq!(parse(&args(&["--help"])), Action::Help);
    assert_eq!(parse(&args(&["--version"])), Action::Version);
}

/// Non-vacuity: a help/version invocation must not be reachable as a serve
/// action, and the text must actually name both the implemented and the
/// unimplemented surface.
#[test]
fn help_text_publishes_the_unimplemented_surface() {
    let help = usage_text();
    for implemented in [
        "--configfile",
        "--nobanner",
        "--address",
        "--port",
        "--version",
    ] {
        assert!(help.contains(implemented), "help omits {implemented}");
    }
    for (flag, _) in UNIMPLEMENTED_FLAGS {
        assert!(
            help.contains(&format!("--{flag}")),
            "help must disclose the unimplemented --{flag}"
        );
    }
    for (name, _) in UNIMPLEMENTED_SUBCOMMANDS {
        assert!(
            help.contains(name),
            "help must disclose the {name} subcommand"
        );
    }
    assert!(version_text().contains("nd-cli"));
}

/// `--help` with a garbage flag still answers help (cobra behavior), and
/// `--help` alongside a config path never loads anything: the parse result is
/// `Help`, so the caller cannot reach the serve path at all.
#[test]
fn help_wins_over_other_arguments() {
    assert_eq!(
        parse(&args(&["--configfile", "/nonexistent/x.toml", "--help"])),
        Action::Help
    );
    assert_eq!(parse(&args(&["scan", "--help"])), Action::Help);
}

// ---------------------------------------------------------------------------
// Usage errors: exit 2, never a scaffold success
// ---------------------------------------------------------------------------

#[test]
fn unknown_flags_and_subcommands_exit_two() {
    for argv in [
        args(&["--nope"]),
        args(&["-x"]),
        args(&["--configfile"]),
        args(&["--port"]),
        args(&["--port", "--address"]),
        args(&["scan"]),
        args(&["backup"]),
        args(&["doctor"]),
        args(&["plugin"]),
        args(&["analyze"]),
        args(&["somethingelse"]),
        args(&["--nobanner=true"]),
    ] {
        let action = parse(&argv);
        assert_eq!(action.exit_code(), EXIT_USAGE, "{argv:?} must exit 2");
        match &action {
            Action::Usage(message) => {
                assert!(!message.is_empty(), "{argv:?} must explain the refusal")
            }
            other => panic!("{argv:?} must be a usage error, got {other:?}"),
        }
    }
}

/// An unimplemented Go flag is named, not silently accepted.
#[test]
fn unimplemented_flags_are_rejected_by_name() {
    for (flag, reason) in UNIMPLEMENTED_FLAGS {
        let action = parse(&args(&[&format!("--{flag}"), "value"]));
        let Action::Usage(message) = action else {
            panic!("--{flag} must be rejected, got {action:?}");
        };
        assert!(
            message.contains(flag),
            "message must name --{flag}: {message}"
        );
        assert!(message.contains(reason), "message must say why: {message}");
    }
}

#[test]
fn serve_options_parse_both_flag_forms() {
    let action = parse(&args(&[
        "--configfile",
        "/tmp/a.toml",
        "--nobanner",
        "--address",
        "127.0.0.1",
        "--port",
        "0",
    ]));
    let Action::Serve(options) = action else {
        panic!("expected serve, got {action:?}");
    };
    assert_eq!(options.config_file.as_deref(), Some("/tmp/a.toml"));
    assert!(options.no_banner);
    assert_eq!(
        options.overrides,
        vec![
            Override {
                key: ADDRESS_FLAG,
                value: "127.0.0.1".to_string()
            },
            Override {
                key: PORT_FLAG,
                value: "0".to_string()
            },
        ]
    );

    let equals = parse(&args(&["--configfile=/tmp/b.toml", "-n", "--port=1"]));
    let Action::Serve(options) = equals else {
        panic!("expected serve, got {equals:?}");
    };
    assert_eq!(options.config_file.as_deref(), Some("/tmp/b.toml"));
    assert!(options.no_banner);
    assert_eq!(options.overrides[0].value, "1");
}

/// A negative port value is an argument, not another flag.
#[test]
fn negative_number_is_a_value_not_a_flag() {
    let action = parse(&args(&["--port", "-1"]));
    let Action::Serve(options) = action else {
        panic!("expected serve, got {action:?}");
    };
    assert_eq!(options.overrides[0].value, "-1");
}

/// No arguments at all means serve — the root command runs the server, exactly
/// like Go's root `RunE`.
#[test]
fn no_arguments_means_serve_not_immediate_success() {
    assert_eq!(parse(&args(&[])), Action::Serve(Default::default()));
}

/// The override value never appears in `Debug`, so a credential-bearing address
/// cannot leak through a panic message or a log line.
#[test]
fn override_debug_withholds_the_value() {
    let sneaky = Override {
        key: ADDRESS_FLAG,
        value: "user:password@127.0.0.1".to_string(),
    };
    let text = format!("{sneaky:?}");
    assert!(
        !text.contains("password"),
        "Debug must withhold the value: {text}"
    );
    assert!(
        !text.contains("user:"),
        "Debug must withhold the value: {text}"
    );
    assert!(text.contains(ADDRESS_FLAG), "the key is still identifiable");
}

#[test]
fn apply_rejects_a_malformed_port() {
    let mut config = nd_conf::Config::default();
    let err = crate_override(
        &mut config,
        &[Override {
            key: PORT_FLAG,
            value: "not-a-number".to_string(),
        }],
    )
    .expect_err("a non-numeric port must be refused");
    assert!(err.contains("--port"), "the message names the flag: {err}");
    assert!(
        !err.contains("not-a-number"),
        "the value is not echoed: {err}"
    );
}

/// Thin wrapper so the test calls the real applier.
fn crate_override(config: &mut nd_conf::Config, overrides: &[Override]) -> Result<(), String> {
    nd_cli::args::apply(config, overrides)
}

#[test]
fn apply_wins_over_loaded_values() {
    let mut config = nd_conf::Config {
        address: "0.0.0.0".to_string(),
        port: 4533,
        ..nd_conf::Config::default()
    };
    crate_override(
        &mut config,
        &[
            Override {
                key: ADDRESS_FLAG,
                value: "127.0.0.1".to_string(),
            },
            Override {
                key: PORT_FLAG,
                value: "8080".to_string(),
            },
        ],
    )
    .expect("overrides apply");
    assert_eq!(config.address, "127.0.0.1");
    assert_eq!(config.port, 8080);
}

// ---------------------------------------------------------------------------
// Resolution gate: capability refusal and revalidation, before any write
// ---------------------------------------------------------------------------

/// Run a preflight against a sandbox, exactly as the supervisor does.
fn preflight_in(
    sandbox: &Sandbox,
    cwd: &Path,
    config_text: &str,
) -> Result<nd_cli::preflight::PreflightOutcome, PreflightError> {
    let config_path = sandbox.file("serve.toml", config_text);
    preflight(PreflightOptions {
        config_file: Some(config_path.to_str().unwrap()),
        env: None,
        startup_cwd: Some(cwd),
        effective_uid: Some(1000),
        allow_zero_port: true,
    })
}

/// A configured scanner is refused **before** any directory or database exists.
#[test]
fn enabled_scanner_is_refused_and_creates_nothing() {
    let sandbox = Sandbox::new("scanner");
    let music = sandbox.dir("music");
    let data = sandbox.join("data");
    let cache = sandbox.join("cache");

    // Scanner left at its default (Enabled = true, ScanOnStartup = true).
    let text = format!(
        "MusicFolder = \"{music}\"\nDataFolder = \"{data}\"\nCacheFolder = \"{cache}\"\nAddress = \"127.0.0.1\"\nPort = 0\n",
        music = music.display(),
        data = data.display(),
        cache = cache.display()
    );
    let mut outcome = preflight_in(&sandbox, sandbox.root(), &text).expect("preflight passes");

    let err = validate_resolution(&mut outcome, &[], false)
        .expect_err("an enabled scanner must be refused");
    assert_eq!(
        err,
        ServeError::Capability(CapabilityRefusal::Scanner),
        "the refusal names the capability"
    );
    assert!(!data.exists(), "a refusal creates no data directory");
    assert!(!cache.exists(), "a refusal creates no cache directory");
    // The refusal text says what to do instead, without inventing a scanner.
    assert!(err.to_string().contains("Scanner.Enabled"));
}

/// Explicitly disabling the scanner passes the gate.
#[test]
fn disabled_scanner_passes_the_gate() {
    let sandbox = Sandbox::new("scanner-off");
    let music = sandbox.dir("music");
    let outcome = preflight_in(
        &sandbox,
        sandbox.root(),
        &serve_toml(
            &music.display().to_string(),
            &sandbox.join("data").display().to_string(),
            &sandbox.join("cache").display().to_string(),
        ),
    )
    .expect("preflight passes");
    let mut outcome = outcome;
    validate_resolution(&mut outcome, &[], false).expect("scanner off passes the gate");
}

/// A bad override value is refused by the gate, and revalidation catches an
/// out-of-range port rather than letting the bind decide.
#[test]
fn override_revalidation_refuses_out_of_range_ports() {
    let sandbox = Sandbox::new("override");
    let music = sandbox.dir("music");
    let base = serve_toml(
        &music.display().to_string(),
        &sandbox.join("data").display().to_string(),
        &sandbox.join("cache").display().to_string(),
    );

    for bad in ["99999", "-1", "nope", ""] {
        let mut outcome = preflight_in(&sandbox, sandbox.root(), &base).expect("preflight passes");
        let err = validate_resolution(
            &mut outcome,
            &[Override {
                key: PORT_FLAG,
                value: bad.to_string(),
            }],
            false,
        )
        .expect_err("--port {bad} must be refused");
        let ServeError::Override { flag, .. } = err else {
            panic!("expected an override error for {bad:?}, got {err:?}");
        };
        assert_eq!(flag, PORT_FLAG);
    }

    // An explicit `--port 0` is a legitimate ephemeral request (Go binds
    // host:0), so it passes the revalidation.
    let mut outcome = preflight_in(&sandbox, sandbox.root(), &base).expect("preflight passes");
    validate_resolution(
        &mut outcome,
        &[Override {
            key: PORT_FLAG,
            value: "0".to_string(),
        }],
        false,
    )
    .expect("--port 0 is an ephemeral bind, like Go's host:0");
    assert_eq!(outcome.config.port, 0);
}

/// An override pointing at a Unix address is refused, not downgraded to TCP.
#[test]
fn unix_override_is_refused_not_downgraded() {
    let sandbox = Sandbox::new("unix");
    let music = sandbox.dir("music");
    let base = serve_toml(
        &music.display().to_string(),
        &sandbox.join("data").display().to_string(),
        &sandbox.join("cache").display().to_string(),
    );
    let mut outcome = preflight_in(&sandbox, sandbox.root(), &base).expect("preflight passes");
    let err = validate_resolution(
        &mut outcome,
        &[Override {
            key: ADDRESS_FLAG,
            value: "unix:/tmp/x.sock".to_string(),
        }],
        false,
    )
    .expect_err("a unix address must be refused");
    assert!(matches!(err, ServeError::Override { .. }), "got {err:?}");
    assert!(
        !err.to_string().contains("/tmp/x.sock"),
        "the value is not echoed: {err}"
    );
}

// ---------------------------------------------------------------------------
// Serve outcome mapping
// ---------------------------------------------------------------------------

/// Only an observed drain maps to zero; a failed serve and an incomplete drain
/// are both nonzero (`subplans/boot-api.md:255,263`).
#[test]
fn only_a_joined_drain_exits_zero() {
    let report = ServeReport {
        local_addr: "127.0.0.1:1".parse().expect("loopback address"),
        ready: None,
        inactive: InactiveServices::for_bootstrap_stage(),
    };

    let drained = ServeOutcome::Drained(report.clone());
    assert_eq!(drained.exit_code(), EXIT_OK);
    assert_eq!(drained.failure_reason(), None);
    assert!(drained.report().is_some());

    let failed = ServeOutcome::Failed(ServeError::Bind {
        reason: "refused".to_string(),
    });
    assert_eq!(failed.exit_code(), EXIT_FAILURE);
    assert!(failed
        .failure_reason()
        .unwrap()
        .to_string()
        .contains("refused"));
    assert!(
        failed.report().is_none(),
        "nothing was bound, so there is no report"
    );

    let incomplete = ServeOutcome::Incomplete(
        report.clone(),
        ServeError::IncompleteShutdown("deadline elapsed".to_string()),
    );
    assert_eq!(
        incomplete.exit_code(),
        EXIT_FAILURE,
        "an incomplete drain is never reported as success"
    );
    assert!(incomplete.report().is_some(), "the socket did exist");

    // An unavailable stop source is a distinct reason from a real signal, which
    // is what stops "no handler installed" from becoming a false exit 0.
    assert_ne!(StopReason::Signalled, StopReason::NoSignalSource);
    assert_eq!((EXIT_OK, EXIT_FAILURE, EXIT_USAGE), (0, 1, 2));
}

// ---------------------------------------------------------------------------
// The supervisor entry, in-process with injected environment
// ---------------------------------------------------------------------------

/// Without a stop source the run must **not** claim a graceful exit: the socket
/// is bound and drained, but the outcome is reported nonzero. Exiting 0 here
/// would assert a shutdown that was never requested.
#[tokio::test]
async fn serve_without_a_stop_source_is_nonzero_not_a_fake_graceful_exit() {
    use nd_cli::supervisor::{serve_with, ServeEnv};

    let sandbox = Sandbox::new("no-stop");
    let music = sandbox.dir("music");
    let config = sandbox.file(
        "serve.toml",
        &serve_toml(
            &music.display().to_string(),
            &sandbox.join("data").display().to_string(),
            &sandbox.join("cache").display().to_string(),
        ),
    );
    let options = nd_cli::args::ServeOptions {
        config_file: Some(config.to_str().expect("utf-8 path").to_string()),
        ..Default::default()
    };

    let outcome = serve_with(ServeEnv {
        env: None,
        startup_cwd: Some(sandbox.root()),
        effective_uid: Some(1000),
        allow_zero_port: true,
        graceful_deadline: Some(std::time::Duration::from_millis(500)),
        stop_signal: None,
        emit_diagnostics: false,
        options: &options,
    })
    .await;

    assert_eq!(
        outcome.exit_code(),
        EXIT_FAILURE,
        "an undelivered stop request is not a graceful shutdown"
    );
    let reason = outcome
        .failure_reason()
        .expect("a reason is reported for the nonzero exit");
    assert!(
        matches!(reason, ServeError::IncompleteShutdown(_)),
        "the failure is specifically an incomplete shutdown, got {reason:?}"
    );
    let report = outcome.report().expect("the socket was actually bound");
    assert_eq!(report.local_addr.ip().to_string(), "127.0.0.1");
    assert_ne!(report.local_addr.port(), 0, "readiness reports a real port");
    assert!(
        sandbox.join("data").exists(),
        "bootstrap ran before the bind"
    );
}

/// An unusable configuration reaches the same entry and is refused without ever
/// binding or writing anything.
#[tokio::test]
async fn serve_refuses_an_invalid_limit_before_any_write() {
    use nd_cli::supervisor::{serve_with, ServeEnv};

    let sandbox = Sandbox::new("serve-bad");
    let music = sandbox.dir("music");
    let config = sandbox.file(
        "serve.toml",
        &format!(
            "MusicFolder = \"{music}\"\nDataFolder = \"{data}\"\nCacheFolder = \"{cache}\"\n\
             Address = \"127.0.0.1\"\nPort = 0\nAuthRequestLimit = 0\n\
             [Scanner]\nEnabled = false\nScanOnStartup = false\n",
            music = music.display(),
            data = sandbox.join("data").display(),
            cache = sandbox.join("cache").display()
        ),
    );
    let options = nd_cli::args::ServeOptions {
        config_file: Some(config.to_str().expect("utf-8 path").to_string()),
        ..Default::default()
    };

    let outcome = serve_with(ServeEnv {
        env: None,
        startup_cwd: Some(sandbox.root()),
        effective_uid: Some(1000),
        allow_zero_port: true,
        graceful_deadline: None,
        stop_signal: None,
        emit_diagnostics: false,
        options: &options,
    })
    .await;

    assert_eq!(outcome.exit_code(), EXIT_FAILURE);
    assert!(
        outcome.report().is_none(),
        "nothing was bound, so no report"
    );
    assert!(
        matches!(
            outcome.failure_reason(),
            Some(ServeError::Preflight(
                PreflightError::InvalidAuthRequestLimit { .. }
            ))
        ),
        "the refusal is the invalid auth limit, got {:?}",
        outcome.failure_reason()
    );
    assert!(
        !sandbox.join("data").exists(),
        "no data directory is created"
    );
    assert!(
        !sandbox.join("cache").exists(),
        "no cache directory is created"
    );
}
