//! CLI argument parsing for the BOOT-04 bootstrap-only entry.
//!
//! Source shape: `cmd/root.go` — the persistent flags `--configfile/-c` and
//! `--nobanner/-n` (`cmd/root.go:456-457`), version template
//! `cmd/root.go:63`, and "no subcommand runs the server" (`runNavidrome` is the
//! root `RunE`, `cmd/root.go:124-155`). Full Go flag parity is BOOT-F1 work;
//! this increment implements only what BOOT-04 names, and **publishes the
//! unsupported set honestly** instead of accepting a flag and ignoring it.
//!
//! Exit-code contract (`subplans/boot-api.md:263`):
//! usage error → 2, config/bootstrap/bind/runtime error → 1, normal shutdown → 0,
//! `--help`/`--version` → 0 without touching DB, music, listener, or secrets.

use std::fmt;

/// Normal completion (including a graceful shutdown).
pub const EXIT_OK: i32 = 0;
/// Bootstrap, bind, or runtime failure.
pub const EXIT_FAILURE: i32 = 1;
/// Malformed invocation: unknown flag, unknown subcommand, bad override value.
pub const EXIT_USAGE: i32 = 2;

/// What the parsed arguments ask the process to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run the bootstrap + public HTTP shell until a signal or an error.
    Serve(ServeOptions),
    /// Print usage and exit [`EXIT_OK`]. No config, filesystem, or listener work.
    Help,
    /// Print the version and exit [`EXIT_OK`]. No config, filesystem, or listener work.
    Version,
    /// Reject the invocation with [`EXIT_USAGE`]. Never a scaffold success.
    Usage(String),
}

/// A configuration key overridden from the command line.
///
/// Only [`ADDRESS_FLAG`] and [`PORT_FLAG`] are accepted here, because those are
/// the only overrides BOOT-04 approves. Go binds five more persistent flags
/// (`musicfolder`, `datafolder`, `cachefolder`, `loglevel`, `logfile`,
/// `cmd/root.go:458-462`) through viper; those are **not** implemented and are
/// reported by [`UNIMPLEMENTED_FLAGS`] rather than accepted-and-ignored.
#[derive(Clone, PartialEq, Eq)]
pub struct Override {
    /// Dotted config key, as `nd-conf` would accept it.
    pub key: &'static str,
    /// Raw text supplied on the command line.
    pub value: String,
}

/// Manual `Debug`: the value is operator-supplied and can carry credentials
/// (`Address` userinfo is the concrete case — `PreflightOutcome::Debug` withholds
/// `address` for the same reason), so only the key is printed.
impl fmt::Debug for Override {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Override {{ key: {}, value: < withheld > }}", self.key)
    }
}

/// `--address` — bind-address override.
pub const ADDRESS_FLAG: &str = "address";
/// `--port` — bind-port override.
pub const PORT_FLAG: &str = "port";

/// Flags Go accepts that this increment refuses to pretend it implemented.
///
/// Accepting one and ignoring it would silently change what the operator asked
/// for (`subplans/boot-api.md:263` "unsupported user-requested behavior cannot be
/// silently ignored"), so they are a usage error naming the flag and the stage
/// that owns it.
pub const UNIMPLEMENTED_FLAGS: &[(&str, &str)] = &[
    (
        "musicfolder",
        "BOOT-01 owns MusicFolder via configuration; no flag yet",
    ),
    (
        "datafolder",
        "BOOT-01 owns DataFolder via configuration; no flag yet",
    ),
    (
        "cachefolder",
        "BOOT-01 owns CacheFolder via configuration; no flag yet",
    ),
    ("loglevel", "logging setup is not wired yet"),
    ("logfile", "logging setup is not wired yet"),
    (
        "tls",
        "TLS is refused before bind (BOOT-03); no flag exists",
    ),
];

/// Subcommands the Go binary has that this port does not implement yet.
/// Published honestly — a positional argument is never a no-op success.
pub const UNIMPLEMENTED_SUBCOMMANDS: &[(&str, &str)] = &[
    (
        "scan",
        "no scanner orchestration (SC track; EXT-SC-REAL-SCAN)",
    ),
    ("backup", "no backup command (DB track)"),
    ("doctor", "no diagnostic command"),
    ("plugin", "no plugin manager"),
    ("analyze", "no DB analyzer"),
];

/// Parsed options for the serve path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeOptions {
    /// `--nobanner`: suppress the startup banner.
    pub no_banner: bool,
    /// `--configfile PATH` (also `-c`). `None` lets `ND_CONFIGFILE` decide.
    pub config_file: Option<String>,
    /// Approved overrides, applied after config + env load, then revalidated.
    pub overrides: Vec<Override>,
}

impl Action {
    /// Exit code for this action, before any serve attempt.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Action::Serve(_) | Action::Help | Action::Version => EXIT_OK,
            Action::Usage(_) => EXIT_USAGE,
        }
    }
}

/// Parse process arguments (excluding the program name).
///
/// Rules:
/// * `--help`/`-h` and `--version` short-circuit — they are answered without any
///   filesystem, database, listener, or secret access.
/// * `--flag=value` and `--flag value` both work; a flag needing a value with none
///   is a usage error.
/// * Unknown flags and unknown positionals are usage errors (exit 2), never
///   scaffold success.
/// * No arguments at all means serve (Go's root command runs the server).
#[must_use]
pub fn parse(args: &[String]) -> Action {
    let mut serve = ServeOptions::default();
    let mut help = false;
    let mut version = false;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].clone();
        // `-h`/`-c`/`-n` are the Go/cobra-style shorthands.
        if arg == "--help" || arg == "-h" {
            help = true;
            i += 1;
            continue;
        }
        if arg == "--version" {
            version = true;
            i += 1;
            continue;
        }
        if arg == "--nobanner" || arg == "-n" {
            serve.no_banner = true;
            i += 1;
            continue;
        }
        if arg == "--configfile" || arg == "-c" {
            match next_value(args, i, &arg) {
                Ok(value) => serve.config_file = Some(value),
                Err(err) => return Action::Usage(err),
            }
            i += 2;
            continue;
        }
        if arg == "--address" || arg == "--port" {
            let key = if arg == "--address" {
                ADDRESS_FLAG
            } else {
                PORT_FLAG
            };
            match next_value(args, i, &arg) {
                Ok(value) => serve.overrides.push(Override { key, value }),
                Err(err) => return Action::Usage(err),
            }
            i += 2;
            continue;
        }
        // `--flag=value` form for every value-taking flag above.
        if let Some((name, value)) = arg.split_once('=') {
            let key = match name {
                "--configfile" => {
                    serve.config_file = Some(value.to_string());
                    i += 1;
                    continue;
                }
                "--address" => ADDRESS_FLAG,
                "--port" => PORT_FLAG,
                "--nobanner" | "--help" | "--version" => {
                    return Action::Usage(format!("{name} takes no value"));
                }
                other => return unknown_flag(other),
            };
            serve.overrides.push(Override {
                key,
                value: value.to_string(),
            });
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            let name = arg.trim_start_matches('-');
            if let Some((_, reason)) = UNIMPLEMENTED_FLAGS.iter().find(|(flag, _)| *flag == name) {
                return Action::Usage(format!("--{name} is not implemented: {reason}"));
            }
            return unknown_flag(name);
        }
        positional.push(arg);
        i += 1;
    }

    // Help/version win over everything and never reach the serve path.
    if help {
        return Action::Help;
    }
    if version {
        return Action::Version;
    }

    if !positional.is_empty() {
        let name = positional[0].as_str();
        if let Some((_, reason)) = UNIMPLEMENTED_SUBCOMMANDS.iter().find(|(c, _)| *c == name) {
            return Action::Usage(format!("subcommand {name} is not implemented: {reason}"));
        }
        return Action::Usage(format!(
            "unexpected argument {name:?}; the root command takes no subcommand \
             (supported: --configfile/-c, --nobanner/-n, --address, --port, --help/-h, --version)"
        ));
    }

    Action::Serve(serve)
}

fn unknown_flag(name: &str) -> Action {
    Action::Usage(format!("unknown flag --{name}"))
}

/// Read the value that follows a space-separated flag.
fn next_value(args: &[String], i: usize, flag: &str) -> Result<String, String> {
    match args.get(i + 1) {
        Some(value) if !value.starts_with('-') || is_negative_number(value) => Ok(value.clone()),
        Some(value) => Err(format!("{flag} requires a value (got {value:?})")),
        None => Err(format!("{flag} requires a value")),
    }
}

/// A `-1`-style value is a legitimate `--port` argument, not another flag.
fn is_negative_number(value: &str) -> bool {
    value
        .strip_prefix('-')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Apply command-line overrides to an already-loaded configuration.
///
/// Precedence (Go binds flags through viper after the file and environment are
/// loaded, `cmd/root.go:464-468`): file → env → flag. Values are parsed strictly;
/// a malformed override is a **usage** error, while a well-formed value that
/// fails semantic validation stays a bootstrap error (exit 1) — see
/// [`crate::supervisor`].
///
/// # Errors
/// A usage message naming the flag and why its value was refused.
pub fn apply(config: &mut nd_conf::Config, overrides: &[Override]) -> Result<(), String> {
    for item in overrides {
        match item.key {
            ADDRESS_FLAG => config.address = item.value.clone(),
            PORT_FLAG => {
                let port = item.value.trim().parse::<i64>().map_err(|err| {
                    format!("--port requires an integer port number (rejected: {err})")
                })?;
                config.port = port;
            }
            other => return Err(format!("internal error: unapproved override key {other}")),
        }
    }
    Ok(())
}

/// Usage text for `--help`. Names the implemented surface and the unimplemented
/// one, so the operator is never told a flag works when it does nothing.
#[must_use]
pub fn usage_text() -> String {
    let mut out = String::new();
    out.push_str("nd-cli — Navidrome (Rust port)\n\n");
    out.push_str(
        "USAGE:\n    nd-cli [--configfile PATH] [--nobanner] [--address ADDR] [--port PORT]\n\n",
    );
    out.push_str("FLAGS (implemented in this stage):\n");
    out.push_str("    --configfile PATH, -c PATH   config file (also ND_CONFIGFILE)\n");
    out.push_str("    --nobanner, -n               do not print the startup banner\n");
    out.push_str("    --address ADDR               bind-address override\n");
    out.push_str("    --port PORT                  bind-port override\n");
    out.push_str("    --help, -h                   print this help and exit 0\n");
    out.push_str("    --version                    print the version and exit 0\n\n");
    out.push_str("FLAGS NOT IMPLEMENTED YET (rejected with exit 2, never ignored):\n");
    for (flag, reason) in UNIMPLEMENTED_FLAGS {
        out.push_str(&format!("    --{flag}    {reason}\n"));
    }
    out.push_str("\nSUBCOMMANDS NOT IMPLEMENTED YET (rejected with exit 2):\n");
    for (name, reason) in UNIMPLEMENTED_SUBCOMMANDS {
        out.push_str(&format!("    {name}    {reason}\n"));
    }
    out.push_str("\nNOTE: this stage serves the public HTTP shell only (no protected routes,\n");
    out.push_str("      scanner, streaming, or API handlers).\n");
    out
}

/// Version string for `--version` (`cmd/root.go:63` prints `.Version`).
#[must_use]
pub fn version_text() -> String {
    format!("nd-cli {} (Rust port)\n", env!("CARGO_PKG_VERSION"))
}

/// Startup banner (`preRun` prints `resources.Banner()` unless `--nobanner`,
/// `cmd/root.go:67-69`). The Go banner art is not ported; this says the same
/// thing in plain text, with no secret or path material.
#[must_use]
pub fn banner_text() -> String {
    format!(
        "Navidrome {} (Rust port) — bootstrap stage\n",
        env!("CARGO_PKG_VERSION")
    )
}

impl fmt::Display for Override {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The value is operator-supplied and could carry an address with
        // userinfo, so only the key is shown.
        write!(f, "--{}", self.key)
    }
}
