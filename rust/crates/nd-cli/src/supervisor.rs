//! Supervisor for the BOOT-04 bootstrap-only service graph.
//!
//! Construction order is the one `subplans/boot-api.md:265` prescribes, trimmed
//! to what exists: **immutable loaded config → BOOT-01 bootstrap (`initialize` +
//! `ServiceReady::prepare`) → public HTTP shell → listener → shutdown
//! supervisor**. Services that do not exist yet are *enumerated as inactive* in
//! [`InactiveServices`] rather than silently started (`subplans/boot-api.md:265`:
//! "Do not start schedules/insights/external providers/artwork merely because
//! defaults enable them"; "Clearly enumerate temporarily inactive services in
//! startup diagnostic without secrets").
//!
//! Two rules this module is strict about:
//! * **Readiness is the bound socket.** [`ServeReport::local_addr`] only ever
//!   comes from an actual `BoundListener`, and nothing is printed before the bind
//!   succeeds (`subplans/boot-api.md:249`). No 50 ms proof, no startup sleep.
//! * **Failure is honest.** An unusable capability, a refused transport, a bind
//!   error, a serve error, or an incomplete drain all map to a nonzero exit
//!   ([`ServeOutcome::exit_code`]). A configured scanner is refused rather than
//!   pretended to run, and a TLS/Unix configuration is refused rather than
//!   served as insecure TCP.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use chrono::Utc;

use crate::args::{Override, ServeOptions, EXIT_FAILURE, EXIT_OK};
use crate::preflight::{preflight, PreflightError, PreflightOptions, PreflightOutcome};
use crate::service_ready::ServiceReady;

/// Why the serve path refused to start.
///
/// Variants carry no configuration values and no secret material: the config
/// object holds passwords and API keys, and `Address`/`DbPath` can carry
/// userinfo, so only *stages* and cause *classes* cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeError {
    /// Configuration resolution or validation failed (before any write).
    Preflight(PreflightError),
    /// Bootstrap/secret/database stage failed; carries the stage name only.
    Bootstrap {
        /// `ServiceReadyError::stage()` label.
        stage: String,
    },
    /// A configured capability this stage cannot honor. Refused **before**
    /// `initialize`, so no directory or database is created.
    Capability(CapabilityRefusal),
    /// A command-line override that is well-formed but invalid, or that
    /// revalidation rejects.
    Override {
        /// The flag name (never its value).
        flag: &'static str,
        /// Why it was refused.
        reason: String,
    },
    /// Bind refused — including a configured TLS/Unix transport, which is never
    /// downgraded to insecure TCP. Carries the bind error's own message, which
    /// `nd-http` builds from a numeric address only.
    Bind {
        /// `BindError` display text.
        reason: String,
    },
    /// The accept loop reported an error while running.
    Serve {
        /// I/O error text from the accept loop.
        reason: String,
    },
    /// Shutdown did not complete within the deadline, or the accept task was
    /// never joined. Reported as nonzero/incomplete.
    IncompleteShutdown(String),
    /// A signal handler could not be installed, so no stop source exists. The
    /// run is refused **before** any bind or filesystem write. tokio's own text
    /// names only a signal number, so no configuration value or secret crosses
    /// this boundary.
    SignalSource {
        /// The signal the port tried to register (`"SIGTERM"`, …).
        name: &'static str,
        /// Why registration failed.
        reason: String,
    },
}

/// A configured capability that this stage refuses to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityRefusal {
    /// `Scanner.Enabled` or `Scanner.ScanOnStartup` is set. No scan
    /// orchestration exists, and `decisions.md:1350` (D-BOOT01-LAUNCH-OPEN) has
    /// deliberately left that launch policy undecided — so the boot fails
    /// loudly instead of silently ignoring it or pretending to scan
    /// (`subplans/boot-api.md:265`).
    Scanner,
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::Preflight(err) => write!(f, "configuration failed: {err}"),
            ServeError::Bootstrap { stage } => {
                write!(f, "bootstrap failed at stage {stage}")
            }
            ServeError::Capability(CapabilityRefusal::Scanner) => write!(
                f,
                "Scanner.Enabled/Scanner.ScanOnStartup is set, but no scanner \
                 orchestration exists in this stage. Set Scanner.Enabled = false \
                 (ND_SCANNER_ENABLED=false) to run the bootstrap shell; starting a \
                 scanner is SC-track work and the launch policy is an open decision."
            ),
            ServeError::Override { flag, reason } => {
                write!(f, "invalid --{flag} override: {reason}")
            }
            ServeError::Bind { reason } => write!(f, "bind failed: {reason}"),
            ServeError::Serve { reason } => write!(f, "listener serve error: {reason}"),
            ServeError::IncompleteShutdown(reason) => {
                write!(f, "shutdown incomplete: {reason}")
            }
            ServeError::SignalSource { name, reason } => write!(
                f,
                "could not install a handler for {name}: {reason}; refusing to start,                  because a process that cannot observe a stop signal cannot report a                  graceful shutdown"
            ),
        }
    }
}

impl fmt::Display for CapabilityRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapabilityRefusal::Scanner => write!(f, "scanner requested but not implemented"),
        }
    }
}

/// Services the plan mentions that this stage does not run.
///
/// Printed at startup so an operator is never told the binary does more than it
/// does. Every field is a *capability*, never a path or a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactiveServices {
    /// No authenticated/protected route is mounted (API-02+).
    pub protected_routes: bool,
    /// No scan orchestration, watcher, or startup scan (SC track).
    pub scanner: bool,
    /// No transcoding/streaming handler (API-05), even though `StreamState` exists.
    pub streaming: bool,
    /// No scheduler, backup, analyzer, insights, plugin manager, or artwork worker.
    pub background_services: bool,
    /// No static UI assets.
    pub web_ui: bool,
    /// No TLS or Unix-socket transport; both are refused before bind.
    pub secure_transports: bool,
}

impl InactiveServices {
    /// The fixed list for this stage.
    #[must_use]
    pub fn for_bootstrap_stage() -> Self {
        Self {
            protected_routes: true,
            scanner: true,
            streaming: true,
            background_services: true,
            web_ui: true,
            secure_transports: true,
        }
    }
}

impl fmt::Display for InactiveServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.protected_routes {
            writeln!(f, "  inactive: protected/authenticated routes (API-02+)")?;
        }
        if self.scanner {
            writeln!(f, "  inactive: scanner, watcher, startup scan (SC track)")?;
        }
        if self.streaming {
            writeln!(f, "  inactive: transcoding/streaming handlers (API-05)")?;
        }
        if self.background_services {
            writeln!(
                f,
                "  inactive: scheduler, backup, analyzer, insights, plugins, artwork worker"
            )?;
        }
        if self.web_ui {
            writeln!(f, "  inactive: static web UI assets")?;
        }
        if self.secure_transports {
            writeln!(
                f,
                "  inactive: TLS and Unix-socket transports (refused before bind)"
            )?;
        }
        Ok(())
    }
}

/// What a successful serve produced, with non-secret facts only.
///
/// `Debug` and `PartialEq` are manual: `ServiceReady` deliberately has no derive
/// (its `Debug` withholds signing keys and the DSN), so equality is defined on
/// the observable socket plus the inactive list — which is all a test needs.
#[derive(Clone)]
pub struct ServeReport {
    /// The socket actually bound. Only ever taken from a real listener, which is
    /// what makes "ready" a fact rather than a promise.
    pub local_addr: SocketAddr,
    /// Non-secret bootstrap metadata. Optional so a test can build a report
    /// without bootstrapping a real database; the serve path always fills it.
    pub ready: Option<ServiceReady>,
    /// Services not running in this stage.
    pub inactive: InactiveServices,
}

impl std::fmt::Debug for ServeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeReport")
            .field("local_addr", &self.local_addr)
            .field("ready", &self.ready)
            .field("inactive", &self.inactive)
            .finish()
    }
}

impl PartialEq for ServeReport {
    fn eq(&self, other: &Self) -> bool {
        self.local_addr == other.local_addr && self.inactive == other.inactive
    }
}

impl ServeReport {
    /// One-line readiness line for stderr. Never contains a secret, a DSN, a
    /// music root, or a raw address beyond the bound socket itself.
    #[must_use]
    pub fn ready_line(&self) -> String {
        format!("ready: listening on http://{}/", self.local_addr)
    }
}

/// How a serve attempt ended.
///
/// `Debug` is derived (every field has a disclosure-safe `Debug`); equality is
/// tested through [`ServeOutcome::exit_code`] and [`ServeOutcome::failure_reason`]
/// rather than `PartialEq`, because a report carries service state.
#[derive(Debug, Clone)]
pub enum ServeOutcome {
    /// Accepts stopped and the accept task was joined: normal exit.
    Drained(ServeReport),
    /// The accept loop failed.
    Failed(ServeError),
    /// Shutdown was requested but the drain/join did not complete.
    Incomplete(ServeReport, ServeError),
}

impl ServeOutcome {
    /// Exit code: zero only for an observed, joined drain
    /// (`subplans/boot-api.md:255,263`).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            ServeOutcome::Drained(_) => EXIT_OK,
            ServeOutcome::Failed(_) | ServeOutcome::Incomplete(_, _) => EXIT_FAILURE,
        }
    }

    /// Human-readable reason for a nonzero exit.
    #[must_use]
    pub fn failure_reason(&self) -> Option<&ServeError> {
        match self {
            ServeOutcome::Drained(_) => None,
            ServeOutcome::Failed(err) | ServeOutcome::Incomplete(_, err) => Some(err),
        }
    }

    /// The report, when a socket was actually bound.
    #[must_use]
    pub fn report(&self) -> Option<&ServeReport> {
        match self {
            ServeOutcome::Drained(report) | ServeOutcome::Incomplete(report, _) => Some(report),
            ServeOutcome::Failed(_) => None,
        }
    }
}

/// Injected environment lookup, matching [`crate::preflight::EnvLookup`].
pub type EnvLookup<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;

/// Injected external stop request. Resolving it is the *request* to shut down;
/// the join is still what determines success.
pub type StopSignal<'a> = std::pin::Pin<Box<dyn Future<Output = StopReason> + Send + 'a>>;

/// Why the serve path stopped waiting for work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// An external stop was requested (a process signal, or a test's own handle).
    Signalled,
    /// No stop source could be installed. This is a **failure**, not a graceful
    /// drain: reporting exit zero here would claim a shutdown that never happened.
    NoSignalSource,
}

/// Inputs to [`serve_with`], all injectable so tests need no real process
/// environment, no real cwd, and no privileges.
pub struct ServeEnv<'a> {
    /// Environment lookup, same signature as the preflight's.
    pub env: Option<EnvLookup<'a>>,
    /// Working-directory anchor for relative config/music/data paths.
    pub startup_cwd: Option<&'a std::path::Path>,
    /// Injected effective uid (`None` = read the real one).
    pub effective_uid: Option<u32>,
    /// Accept `Port == 0` as an ephemeral test port.
    pub allow_zero_port: bool,
    /// Graceful-stop deadline. `None` = [`nd_http::DEFAULT_GRACEFUL_DEADLINE`].
    pub graceful_deadline: Option<Duration>,
    /// Fires the shutdown signal from outside (a test's own pipe/signal handle).
    /// `serve_with` awaits it instead of installing process signal handlers.
    pub stop_signal: Option<StopSignal<'a>>,
    /// Print the banner, the ready line, and the inactive-service list to the
    /// process streams. Readiness is emitted **after** the bind, from inside
    /// `serve_with`, so a caller cannot announce it early.
    pub emit_diagnostics: bool,
    /// The command-line options to apply.
    pub options: &'a ServeOptions,
}

impl fmt::Debug for ServeEnv<'_> {
    /// Manual `Debug`: the injected env lookup and stop future are opaque and
    /// could carry credential material, so nothing is printed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServeEnv")
            .field("env", &"<injected lookup>")
            .field("startup_cwd", &self.startup_cwd)
            .field("effective_uid", &self.effective_uid)
            .field("allow_zero_port", &self.allow_zero_port)
            .field("graceful_deadline", &self.graceful_deadline)
            .field("stop_signal", &"<injected future>")
            .field("emit_diagnostics", &self.emit_diagnostics)
            .field("options", &self.options)
            .finish()
    }
}

/// The termination signals this port listens for — exactly Go's `mainContext`
/// set (`cmd/root.go:114-121`). SIGQUIT and SIGPIPE are deliberately **not**
/// included, because Go does not register them; matching that set is fidelity,
/// not an omission.
///
/// `SIGABRT` has no `SignalKind::abort()` in tokio, so its raw signum comes from
/// `libc` (already locked via tokio's unix dependency).
#[cfg(unix)]
pub const SIGNAL_KINDS: [(&str, tokio::signal::unix::SignalKind); 4] = [
    ("SIGINT", tokio::signal::unix::SignalKind::interrupt()),
    ("SIGHUP", tokio::signal::unix::SignalKind::hangup()),
    ("SIGTERM", tokio::signal::unix::SignalKind::terminate()),
    (
        "SIGABRT",
        tokio::signal::unix::SignalKind::from_raw(libc::SIGABRT),
    ),
];

/// The registered signal sources. Held as a struct rather than a `Vec` so the
/// wait selects over all of them without a `futures` dependency, and so
/// "registration completed" is a type: this value exists only if every handler in
/// [`SIGNAL_KINDS`] was installed.
///
/// Non-unix platforms get an empty type, because there is no set to register;
/// [`register_signal_sources`] reports the missing source instead.
#[cfg(unix)]
pub struct UnixSignalSources {
    /// Registered `SIGINT` source.
    pub interrupt: tokio::signal::unix::Signal,
    /// Registered `SIGHUP` source.
    pub hangup: tokio::signal::unix::Signal,
    /// Registered `SIGTERM` source.
    pub terminate: tokio::signal::unix::Signal,
    /// Registered `SIGABRT` source.
    pub abort: tokio::signal::unix::Signal,
}

#[cfg(not(unix))]
pub struct UnixSignalSources;

/// Register a handler for one signal, synchronously.
///
/// Registration is not a promise: `tokio::signal::unix::signal` calls
/// `signal_enable` (`tokio-1.54.1/src/signal/unix.rs:266-289`), which runs
/// `signal_hook_registry::register` before returning. A handler is therefore
/// installed by the time this returns `Ok` — no polling and no spawned task is
/// involved, which is why calling it is itself the evidence of registration.
///
/// # Errors
/// [`ServeError::SignalSource`] if the runtime refuses the signal (tokio rejects
/// forbidden numbers such as `SIGKILL`).
#[cfg(unix)]
pub fn register_one(
    name: &'static str,
    kind: tokio::signal::unix::SignalKind,
) -> Result<tokio::signal::unix::Signal, ServeError> {
    tokio::signal::unix::signal(kind).map_err(|err| ServeError::SignalSource {
        name,
        reason: err.to_string(),
    })
}

/// Install every handler in [`SIGNAL_KINDS`], synchronously and in order.
///
/// [`run`] calls this **before** preflight, bootstrap, bind, and readiness — the
/// same position Go's `signal.NotifyContext` occupies (`cmd/root.go:60-61`,
/// which precedes `ExecuteContext`, where cobra then loads configuration in
/// `preRun`). A stop signal arriving during bootstrap is therefore observed
/// rather than missed, and a registration failure ends the run before anything
/// is written or bound.
///
/// # Errors
/// [`ServeError::SignalSource`] naming the first signal that could not be
/// registered.
#[cfg(unix)]
pub fn register_signal_sources() -> Result<UnixSignalSources, ServeError> {
    let mut interrupt = None;
    let mut hangup = None;
    let mut terminate = None;
    let mut abort = None;
    for (name, kind) in SIGNAL_KINDS {
        let signal = register_one(name, kind)?;
        match name {
            "SIGINT" => interrupt = Some(signal),
            "SIGHUP" => hangup = Some(signal),
            "SIGTERM" => terminate = Some(signal),
            "SIGABRT" => abort = Some(signal),
            other => unreachable!("SIGNAL_KINDS contains an unknown name {other}"),
        }
    }
    Ok(UnixSignalSources {
        interrupt: interrupt.expect("SIGINT registered"),
        hangup: hangup.expect("SIGHUP registered"),
        terminate: terminate.expect("SIGTERM registered"),
        abort: abort.expect("SIGABRT registered"),
    })
}

/// No unix signal set exists on this platform, so there is no stop source. The
/// run is refused before preflight and bind rather than serving unsignalled and
/// later claiming a graceful shutdown.
#[cfg(not(unix))]
pub fn register_signal_sources() -> Result<UnixSignalSources, ServeError> {
    Err(ServeError::SignalSource {
        name: "SIGTERM",
        reason: "this platform has no unix signal set to register".to_string(),
    })
}

/// Wait for any registered source to deliver a stop, as [`StopReason::Signalled`].
///
/// `Signal::recv` is documented cancel-safe (`tokio-1.54.1/src/signal/unix.rs:422-426`):
/// a delivery that arrives while a different branch is being polled is not
/// consumed, so a stop issued during bootstrap, the bind, or the readiness
/// emission is still observed here and terminates the run. Nothing else polls
/// these streams, so no delivery can be consumed and then dropped.
#[cfg(unix)]
pub async fn wait_for_registered_signals(mut sources: UnixSignalSources) -> StopReason {
    tokio::select! {
        got = sources.interrupt.recv() => got.map(|_| StopReason::Signalled),
        got = sources.hangup.recv() => got.map(|_| StopReason::Signalled),
        got = sources.terminate.recv() => got.map(|_| StopReason::Signalled),
        got = sources.abort.recv() => got.map(|_| StopReason::Signalled),
    }
    .unwrap_or(StopReason::NoSignalSource)
}

/// No registered sources exist on this platform, so no stop can ever be observed.
/// Kept for symmetry with [`register_signal_sources`]: if it is ever reached, it
/// reports a missing source, which the supervisor turns into a nonzero outcome
/// rather than a claimed graceful shutdown.
#[cfg(not(unix))]
pub async fn wait_for_registered_signals(_: UnixSignalSources) -> StopReason {
    StopReason::NoSignalSource
}

/// Resolution stages that must pass **before** any filesystem write: the
/// scanner capability gate and the flag overrides (revalidated through the same
/// `validate_boot_values` the preflight uses).
///
/// Runs after config load and before `initialize`, so a refusal leaves no data
/// directory, cache directory, or database file.
///
/// # Errors
/// [`ServeError::Capability`] or [`ServeError::Override`].
pub fn validate_resolution(
    outcome: &mut PreflightOutcome,
    overrides: &[Override],
    allow_zero_port: bool,
) -> Result<(), ServeError> {
    // Scanner policy refusal: an enabled scanner cannot be honored here, and
    // silently ignoring it would claim behavior the binary does not have.
    if outcome.config.scanner.enabled || outcome.config.scanner.scan_on_startup {
        return Err(ServeError::Capability(CapabilityRefusal::Scanner));
    }

    // A zero port supplied on the command line is an explicit request for an
    // ephemeral socket: Go's `net.Listen("tcp", "host:0")` binds one
    // (`server/server.go:89-90`) and readiness then reports the real address, so
    // accepting it is fidelity, not a relaxation. A zero port coming from the
    // config or environment is still refused by the preflight above.
    let explicit_zero_port = overrides
        .iter()
        .any(|item| item.key == crate::args::PORT_FLAG && item.value.trim() == "0");

    if overrides.is_empty() {
        return Ok(());
    }

    // Flag overrides apply after file + env (Go binds them through viper after
    // load), then the whole result is revalidated — not just the flag value.
    crate::args::apply(&mut outcome.config, overrides).map_err(|reason| ServeError::Override {
        flag: crate::args::PORT_FLAG,
        reason,
    })?;

    crate::preflight::validate_boot_values(&outcome.config, allow_zero_port || explicit_zero_port)
        .map_err(|err| match err {
            // A rejected port/limit is the operator's override, so name the flag.
            PreflightError::ZeroPort | PreflightError::InvalidPort { .. } => ServeError::Override {
                flag: crate::args::PORT_FLAG,
                reason: err.to_string(),
            },
            PreflightError::UnsupportedTls => ServeError::Override {
                flag: crate::args::ADDRESS_FLAG,
                reason: "TLS is configured and is refused before bind".to_string(),
            },
            PreflightError::UnsupportedUnixSocket => ServeError::Override {
                flag: crate::args::ADDRESS_FLAG,
                reason: "a unix: address requires a Unix listener that does not exist".to_string(),
            },
            other => ServeError::Override {
                flag: crate::args::ADDRESS_FLAG,
                reason: other.to_string(),
            },
        })?;

    Ok(())
}

/// Run the BOOT-04 bootstrap-only serve path.
///
/// Sequence: preflight (steps 1–2) → scanner/override resolution gate →
/// `initialize` + `ServiceReady::prepare` (BOOT-01 bootstrap) → bind → report
/// ready on the bound socket → serve until `stop_signal` or a serve error →
/// bounded stop-and-join.
///
/// The returned outcome carries the exit code; this function never calls
/// `process::exit` and never panics on a refused configuration.
///
/// # Errors
/// Never returns `Err`: failures are reported through [`ServeOutcome`], so the
/// caller decides how to print them.
pub async fn serve_with(mut env: ServeEnv<'_>) -> ServeOutcome {
    // 1. Preflight: config resolution + validation, no writes.
    let preflight_options = PreflightOptions {
        config_file: env.options.config_file.as_deref(),
        env: env.env.take(),
        startup_cwd: env.startup_cwd,
        effective_uid: env.effective_uid,
        allow_zero_port: env.allow_zero_port,
    };
    let mut outcome = match preflight(preflight_options) {
        Ok(outcome) => outcome,
        Err(err) => return ServeOutcome::Failed(ServeError::Preflight(err)),
    };

    // 2. Resolution gate: capability refusal + flag overrides, revalidated.
    if let Err(err) = validate_resolution(&mut outcome, &env.options.overrides, env.allow_zero_port)
    {
        return ServeOutcome::Failed(err);
    }

    // 3. BOOT-01 bootstrap: directories, database, secrets, auth and cache state.
    let ready = match ServiceReady::prepare(&outcome, Utc::now()).await {
        Ok(ready) => ready,
        Err(err) => {
            return ServeOutcome::Failed(ServeError::Bootstrap {
                stage: err.stage().to_string(),
            });
        }
    };

    // 4. Bind BEFORE anything claims readiness. TLS/Unix are refused here too,
    //    so the transport can never be quietly downgraded.
    let listener = match nd_http::BoundListener::bind_from_config(&outcome.config) {
        Ok(listener) => listener,
        Err(err) => {
            return ServeOutcome::Failed(ServeError::Bind {
                reason: err.to_string(),
            });
        }
    };
    let local_addr = listener.local_addr();

    let report = ServeReport {
        local_addr,
        ready: Some(ready),
        inactive: InactiveServices::for_bootstrap_stage(),
    };

    // 5. Start the accept loop on the bound socket, then serve until a stop is
    //    requested.
    let shutdown = nd_http::Shutdown::new();
    // The injected router is a placeholder until API-02 supplies the real API
    // router. It carries Go's `http.NotFound` wire as its fallback
    // (`net/http/server.go`, reached via chi's `NotFound`) rather than axum's
    // default empty-body 404, so an unmatched path under `BasePath` is already
    // byte-compatible; nd-cli owns this injection point (`decisions.md:521`) and
    // API-02 replaces the whole router, not just the fallback.
    let placeholder_router = axum::Router::new().fallback(axum::routing::any(|| async {
        nd_http::shell::not_found_response()
    }));
    let running = match nd_http::Running::start(
        listener,
        placeholder_router,
        shutdown.clone(),
        &outcome.config.base_path,
    ) {
        Ok(running) => running,
        Err(err) => {
            return ServeOutcome::Failed(ServeError::Bind {
                reason: err.to_string(),
            });
        }
    };
    // 6. Readiness is announced only after the accept loop is running on the
    // socket we bound (`subplans/boot-api.md:249`): banner to stdout, the ready
    // line and the inactive-service list to stderr. Nothing here names a secret,
    // a DSN, a music root, or a credential-bearing address.
    //
    // `Running::local_addr` is the address stored from the same `BoundListener`
    // this loop took (`nd-http/src/server.rs:164-189`), so it cannot disagree
    // with `local_addr`; an equality guard here would be unreachable code that
    // implies an uncertainty the type does not have.
    if env.emit_diagnostics {
        if !env.options.no_banner {
            print!("{}", crate::args::banner_text());
        }
        eprintln!("{}", report.ready_line());
        eprint!("{}", report.inactive);
    }

    let deadline = env
        .graceful_deadline
        .unwrap_or(nd_http::DEFAULT_GRACEFUL_DEADLINE);

    let stop = match env.stop_signal.take() {
        Some(stop) => stop,
        None => {
            // No stop source: stop accepting, and report the run as incomplete
            // rather than as a clean signalled shutdown.
            let join_result = running.stop_and_join(deadline).await;
            return ServeOutcome::Incomplete(
                report,
                ServeError::IncompleteShutdown(format!(
                    "no stop signal source was installed (join outcome: {join_result:?})"
                )),
            );
        }
    };

    // The stop future resolving is the *request*. `stop_and_join` is the only
    // place that fires the signal and awaits the accept task, so a success claim
    // always rests on an observed join (`subplans/boot-api.md:255`).
    let mut stop = stop;
    if stop.as_mut().await != StopReason::Signalled {
        let join_result = running.stop_and_join(deadline).await;
        return ServeOutcome::Incomplete(
            report,
            ServeError::IncompleteShutdown(format!(
                "stop source was unavailable (join outcome: {join_result:?})"
            )),
        );
    }

    let join_result = running.stop_and_join(deadline).await;

    match join_result {
        nd_http::ShutdownOutcome::Drained => ServeOutcome::Drained(report),
        nd_http::ShutdownOutcome::DeadlineElapsed { deadline_ms } => ServeOutcome::Incomplete(
            report,
            ServeError::IncompleteShutdown(format!(
                "graceful deadline of {deadline_ms}ms elapsed while the accept loop was still running"
            )),
        ),
        nd_http::ShutdownOutcome::ServeFailed { reason } => {
            ServeOutcome::Failed(ServeError::Serve { reason })
        }
        nd_http::ShutdownOutcome::NotJoined => ServeOutcome::Incomplete(
            report,
            ServeError::IncompleteShutdown("accept task was not joined".to_string()),
        ),
    }
}

/// The process-level entry used by `main`.
///
/// Order: **register the termination signal sources first**, then preflight →
/// resolution gate → bootstrap → bind → ready line → serve until a registered
/// signal arrives → bounded stop-and-join. Registration is synchronous, so it
/// is complete before any of that work starts (Go does the same:
/// `signal.NotifyContext` at `cmd/root.go:60` precedes configuration loading in
/// `preRun`). A registration failure returns before preflight, so nothing is
/// written and no socket is bound.
///
/// Always returns the outcome; the caller maps it to an exit code with
/// [`ServeOutcome::exit_code`] and prints [`ServeOutcome::failure_reason`].
pub async fn run(options: &ServeOptions, real_env: bool) -> ServeOutcome {
    // Eager, synchronous registration. The constructed streams are moved into
    // the awaited future below, so the handlers outlive every stage of startup.
    let sources = match register_signal_sources() {
        Ok(sources) => sources,
        Err(err) => return ServeOutcome::Failed(err),
    };

    let env = ServeEnv {
        env: if real_env {
            Some(Box::new(|key| std::env::var(key).ok()))
        } else {
            None
        },
        startup_cwd: None,
        effective_uid: None,
        allow_zero_port: false,
        graceful_deadline: None,
        stop_signal: Some(Box::pin(wait_for_registered_signals(sources))),
        emit_diagnostics: true,
        options,
    };
    serve_with(env).await
}
