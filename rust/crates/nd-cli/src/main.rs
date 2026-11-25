// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Service entrypoint: the typed service structs built by the other crates get
//! wired here. (Rust replaces Go's DI codegen — `cmd/wire_gen.go`,
//! `cmd/wire_injectors.go` — with explicit construction, decided once in #S1.)
//!
//! ## BOOT-04: bootstrap-only serve path
//!
//! The binary now does something: it resolves configuration (`--configfile` →
//! `ND_CONFIGFILE` → loader tiers), runs the BOOT-01 preflight and bootstrap,
//! binds the public HTTP shell, prints readiness **after** the socket exists, and
//! serves until SIGINT/SIGHUP/SIGTERM/SIGABRT — the set Go registers
//! (`cmd/root.go:114-121`) — then performs a bounded stop-and-join.
//!
//! Exit codes are the `boot-api.md:263` contract: usage → 2, bootstrap/bind/
//! runtime/incomplete-shutdown → 1, signalled graceful drain → 0, `--help` and
//! `--version` → 0 without touching the database, music root, listener, or
//! secrets.
//!
//! ## What this stage does NOT claim
//!
//! No protected/authenticated route, no scanner, no streaming, no API handler,
//! no static UI, no scheduler/backup/analyzer/insights/plugins/artwork, and no
//! TLS or Unix transport (both are refused before bind rather than downgraded to
//! insecure TCP). `--musicfolder`, `--datafolder`, `--cachefolder`, `--loglevel`,
//! `--logfile`, and the `scan`/`backup`/`doctor`/`plugin`/`analyze` subcommands
//! are rejected with exit 2 and named in `--help` — never accepted and ignored.
//! The `nd-api` router is not injected here: API-02 supplies it, and `nd-cli`
//! remains the D1 wiring point (`decisions.md:521`).
//!
//! ## Config resolution note (BOOT-01-owned, already closed)
//!
//! `nd_conf::Config::load(None)` does not consult `ND_CONFIGFILE`
//! (`nd-conf/src/config.rs:946-986`); the preflight resolves
//! flag → `ND_CONFIGFILE` → default and hands the loader a concrete path
//! (`nd-cli/src/preflight.rs:499`). `nd_conf::Dir::path()` creates directories
//! and is used only for data/cache paths, never for the music root
//! (`nd-conf/src/dir.rs:56-70`).

use nd_cli::args::{parse, version_text, EXIT_FAILURE, EXIT_USAGE};
use nd_cli::supervisor::{run, ServeOutcome};

/// Map an outcome to a process exit code, printing the reason for a nonzero
/// run. Diagnostics go to stderr so stdout stays clean for banner/version output.
fn report(outcome: &ServeOutcome) -> i32 {
    let code = outcome.exit_code();
    if let Some(reason) = outcome.failure_reason() {
        eprintln!("nd-cli: {reason}");
    }
    if code != 0 {
        eprintln!("nd-cli: exiting {code}");
    }
    code
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        // `--help` / `--version`: answer and leave. No config load, no database,
        // no music root, no listener, no secret.
        nd_cli::args::Action::Help => {
            print!("{}", nd_cli::args::usage_text());
            std::process::exit(0);
        }
        nd_cli::args::Action::Version => {
            print!("{}", version_text());
            std::process::exit(0);
        }
        nd_cli::args::Action::Usage(message) => {
            eprintln!("nd-cli: {message}");
            eprintln!("\n{}", nd_cli::args::usage_text());
            std::process::exit(EXIT_USAGE);
        }
        nd_cli::args::Action::Serve(options) => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    eprintln!("nd-cli: could not start the async runtime: {err}");
                    std::process::exit(EXIT_FAILURE);
                }
            };
            let outcome = runtime.block_on(run(&options, true));
            std::process::exit(report(&outcome));
        }
    }
}
