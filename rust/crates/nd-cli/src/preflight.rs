// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Startup preflight: the callable, side-effect-free checks BOOT-01 steps 1–2
//! freeze (`docs/rust-port/subplans/boot-api.md:192-193`).
//!
//! This module is a **library entry point for later bootstrap work**, not a
//! binary: [`preflight`] takes the startup cwd, an explicit config-file flag
//! and an injected environment, and returns a [`PreflightOutcome`] carrying
//! the committed `Config` plus the canonical music root and data directory
//! that later increments (DB open/migrate, secrets, library seeding) consume.
//!
//! Guarantees pinned here — each is asserted by `tests/preflight.rs`:
//!
//! * **Config path resolution** mirrors Go `getConfigFile`
//!   (`conf/configuration.go:1195-1206`): the explicit flag wins outright;
//!   otherwise `ND_CONFIGFILE` is used **only when it names an existing
//!   path** (Go's `os.Stat` guard) and a non-existent target is skipped —
//!   not a fatal read error — falling back to the normal loader defaults.
//!   `nd_conf::Config::load` never consults `ND_CONFIGFILE` itself
//!   (`nd-conf/src/config.rs:951-983`), which is exactly the gap this closes.
//! * **Config validation happens before any filesystem creation.** The whole
//!   TOML document is parsed and normalized through
//!   [`nd_conf::Config::load_with_env`] (unknown keys fatal) before the data
//!   directory or database path is touched, and the music root is never
//!   created (see the next bullet).
//! * **The music root is never auto-created.** [`nd_conf::Dir::path`] calls
//!   `create_dir_all` and applies permissions (`nd-conf/src/dir.rs:56-70`) —
//!   correct for data/cache/backup dirs, fatal for the root: a typo'd or
//!   not-yet-mounted `MusicFolder` would boot against an empty directory
//!   (`boot01-scout-memo.md` §2b). So the root is resolved against the
//!   **supplied startup cwd** (`Dir::new` keeps the string lazy, unlike the
//!   loader's `data_folder`/`cache_folder`, which are joined against an
//!   already-absolute `DataFolder` at `nd-conf/src/config.rs:991-1011`),
//!   canonicalized, and required to be an existing readable directory.
//!   The canonical path is returned for later stored-root comparison.
//! * **No listener-blocking feature is silently ignored.** A production port
//!   must be `1..=65535` (`0` is accepted only as an explicit test-injection
//!   port), `AuthRequestLimit`/`AuthWindowLength` must be positive, and the
//!   session/auth durations must convert to whole seconds without truncation
//!   or overflow. Configured TLS (`TLSCert`+`TLSKey`) and a `unix:`
//!   `Address` are supported by Go's server but not by the Rust port yet, so
//!   they are rejected **before** any listening would be attempted, rather
//!   than silently downgraded to plain HTTP on TCP.
//! * **Root-user restriction** reproduces Go `validateEnforceNonRootUser`
//!   (`conf/configuration.go:874-882`, called at `:386` before any filesystem
//!   operation): when `EnforceNonRootUser` is set on a unix platform, euid 0
//!   is fatal. The effective uid is an injectable parameter so the check is
//!   testable without privileges.
//! * **Deferred feature policies are reported, not guessed.** `boot-api.md:192`
//!   also says an enabled scanner must not be silently ignored. This
//!   increment neither starts nor claims a scanner, so a configured
//!   `Scanner.Enabled`/`ScanOnStartup` is carried in the committed `Config`
//!   for BOOT-04/SC to act on; whether a scanner-less binary must *refuse*
//!   `Scanner.Enabled = true` (the default) is an unresolved launch policy
//!   and is reported to the parent rather than decided here. The same applies
//!   to the remaining "cap" integer conversions in step 1 (artwork/inspect
//!   backlog caps), which have no Go-side startup validation to port.
//! * **Secrets stay out of errors and `Debug`.** The outcome deliberately has
//!   no derived `Debug`: printing it exposes only non-resolved-path fields.
//!   Config-load errors are re-labelled with a snippet-free, userinfo-scrubbed
//!   message, because `toml`'s `Display` quotes the offending source line and
//!   `Config` derives `Debug` over `auth.password`,
//!   `last_fm.api_key`/`secret` and `dev_auto_create_admin_password`
//!   (`nd-conf/src/config.rs:157/288-290/583`; no Rust redactor exists).

use std::path::{Path, PathBuf};
use std::time::Duration;

use nd_conf::{Config, ConfigError};

/// The environment variable Go reads as the config-file fallback
/// (`conf/configuration.go:1201`).
pub const CONFIGFILE_ENV: &str = "ND_CONFIGFILE";

/// Environment lookup seam: the same shape
/// [`nd_conf::Config::load_with_env`] takes, so an injected map and a real
/// `std::env::var` closure are interchangeable.
pub type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// How the config file was chosen, reported for startup logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// The explicit flag/path argument.
    Flag,
    /// `ND_CONFIGFILE`, accepted because it names an existing path.
    Env,
    /// Nothing supplied: the loader's default tier (file + `ND_*` env +
    /// defaults, with no TOML file read).
    Defaults,
}

/// Why a startup preflight refused to continue.
///
/// Variants carry no config values and no secret material; the
/// [`PreflightError::ConfigLoad`] variant carries a sanitized message
/// (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// `EnforceNonRootUser` is set and the effective uid is 0 (posix only).
    #[error("EnforceNonRootUser is enabled but Navidrome is running as root")]
    RunningAsRoot,
    /// `ND_CONFIGFILE` names an existing path that is not a regular file.
    ///
    /// A *non-existent* env-named target is **not** an error: Go's
    /// `os.Stat` guard skips it and the loader falls back to its normal
    /// default tier (`conf/configuration.go:1201-1205`). A directory is
    /// fatal because Go also fatals there (`viper.ReadInConfig` fails on a
    /// non-file config target, `conf/configuration.go:1188-1191`).
    #[error("config file {path:?} named by {env:?} is not a regular file")]
    ConfigFileNotAFile {
        /// The path as supplied in the environment variable.
        path: String,
        /// The environment variable that carried it.
        env: &'static str,
    },
    /// The config file could not be read, parsed, or validated.
    ///
    /// The message is snippet-free and userinfo-scrubbed: it identifies the
    /// failure without quoting the source line, so a secret on the offending
    /// line cannot reach a log or a terminal.
    #[error("invalid configuration: {message}")]
    ConfigLoad {
        /// Config file that was read, or `""` when the default tier ran.
        path: String,
        /// Sanitized reason.
        message: String,
    },
    /// `Port` outside `1..=65535`.
    #[error("invalid Server.Port {value}: must be 1..=65535 (0 is accepted only as an explicit test-injection port)")]
    InvalidPort {
        /// The configured value.
        value: i64,
    },
    /// `Port == 0` without `allow_zero_port` (test-injection opt-in).
    #[error("Server.Port 0 is only accepted when allow_zero_port is set (test injection)")]
    ZeroPort,
    /// `AuthRequestLimit` is not positive.
    #[error("invalid Server.AuthRequestLimit {value}: must be greater than zero")]
    InvalidAuthRequestLimit {
        /// The configured value.
        value: i64,
    },
    /// `AuthWindowLength` is zero (the loader already maps negatives to the
    /// default).
    #[error("invalid Server.AuthWindowLength: must be greater than zero")]
    InvalidAuthWindowLength,
    /// A duration cannot be converted to whole seconds without truncation or
    /// overflow.
    #[error("invalid {name}: {duration} cannot be converted to whole seconds without truncation or overflow")]
    DurationConversion {
        /// Canonical config key (`Server.SessionTimeout`, …).
        name: &'static str,
        /// The configured duration as displayed by the port.
        duration: String,
    },
    /// `TLSCert`/`TLSKey` are configured; the Rust port has no TLS listener.
    #[error("unsupported configured transport: Server.TLSCert/Server.TLSKey are set, but the Rust port has no TLS listener (refusing to start rather than serve plain HTTP)")]
    UnsupportedTls,
    /// `Address` selects a unix socket; the Rust port has no unix listener.
    #[error("unsupported configured transport: Server.Address {addr:?} requests a unix socket, which the Rust port does not listen on")]
    UnsupportedUnixSocket {
        /// The configured address.
        addr: String,
    },
    /// `MusicFolder` is empty after normalization.
    #[error("invalid Server.MusicFolder: empty music root")]
    EmptyMusicFolder,
    /// The resolved music root does not exist.
    #[error(
        "music root {path:?} does not exist (Navidrome never creates the configured music folder)"
    )]
    MusicRootMissing {
        /// Path as resolved from the startup cwd.
        path: String,
    },
    /// The resolved music root exists but is not a directory.
    #[error("music root {path:?} is not a directory")]
    MusicRootNotDirectory {
        /// Path as resolved from the startup cwd.
        path: String,
    },
    /// The music root is a directory but cannot be read.
    #[error("music root {path:?} is not readable: {reason}")]
    MusicRootNotReadable {
        /// Path as resolved from the startup cwd.
        path: String,
        /// OS error text (contains the path, never config values).
        reason: String,
    },
    /// The startup cwd could not be canonicalized.
    #[error("startup cwd {path:?} cannot be resolved: {reason}")]
    StartupCwdUnresolved {
        /// The cwd passed to the preflight.
        path: String,
        /// OS error text.
        reason: String,
    },
}

/// Everything later bootstrap increments need from steps 1–2.
///
/// `config` is the fully loaded and normalized `Config`; `music_root` is the
/// canonicalized root for the stored-root comparison in step 5 (never the
/// configured string, and never a created path).
pub struct PreflightOutcome {
    /// The committed configuration (validated, normalized).
    pub config: Config,
    /// How the config file was selected.
    pub config_source: ConfigSource,
    /// Startup cwd, canonicalized.
    pub startup_cwd: PathBuf,
    /// Canonicalized configured music root. Never created by this code.
    pub music_root: PathBuf,
    /// Resolved data folder.
    ///
    /// Reported **without** creating it: `Dir::path` would
    /// `create_dir_all`, which step 2 permits, but a preflight must be
    /// repeatable and read-only. Later bootstrap calls `config.data_folder.path()`
    /// when it actually opens the database.
    pub data_folder: PathBuf,
    /// Resolved SQLite path/URI from the loader (`<DataFolder>/<DefaultDbPath>`
    /// plus the default URI parameters when `DbPath` is empty).
    pub db_path: String,
    /// `Server.SessionTimeout` in whole seconds, checked in this preflight so
    /// step 7 can hand it to `nd_auth::Authenticator::new` without re-deriving.
    pub session_timeout_secs: i64,
    /// `Server.AuthWindowLength` in whole seconds (same check).
    pub auth_window_secs: i64,
}

/// `PreflightOutcome` carries a `Config` that derives `Debug` **including**
/// secret fields (`auth.password`, `last_fm.api_key`/`secret`,
/// `dev_auto_create_admin_password`). A manual impl therefore prints only the
/// non-secret resolution facts and never delegates to `Config`.
impl std::fmt::Debug for PreflightOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreflightOutcome")
            .field("config_source", &self.config_source)
            .field("config_file", &self.config.config_file)
            .field("port", &self.config.port)
            .field("address", &self.config.address)
            .field("startup_cwd", &self.startup_cwd)
            .field("music_root", &self.music_root)
            .field("data_folder", &self.data_folder)
            .field("db_path", &self.db_path)
            .field("session_timeout_secs", &self.session_timeout_secs)
            .field("auth_window_secs", &self.auth_window_secs)
            // Deliberately withheld: Config holds passwords and API credentials.
            .finish_non_exhaustive()
    }
}

/// Inputs to [`preflight`]. Every field is injectable so tests need no real
/// process cwd, no privileges, and no process environment.
#[derive(Default)]
pub struct PreflightOptions<'a> {
    /// Explicit config file (the `--config` flag equivalent). Wins over
    /// [`CONFIGFILE_ENV`]; an empty string counts as "not supplied" (Go's
    /// `cfgFile != ""` test).
    pub config_file: Option<&'a str>,
    /// Environment lookup, same signature as
    /// [`nd_conf::Config::load_with_env`].
    pub env: Option<Box<EnvLookup<'a>>>,
    /// Startup cwd used to resolve relative paths. Production callers pass
    /// `std::env::current_dir()`; tests pass a temp root.
    pub startup_cwd: Option<&'a Path>,
    /// Injected effective uid (`None` = read the real one on unix, skip on
    /// windows). Only consulted when `EnforceNonRootUser` is set.
    pub effective_uid: Option<u32>,
    /// Accept `Port == 0` as a test-injection port.
    pub allow_zero_port: bool,
}

/// Manual `Debug`: the injected environment lookup is opaque and could carry
/// credentials, so only the non-environment fields are shown.
impl std::fmt::Debug for PreflightOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreflightOptions")
            .field("config_file", &self.config_file)
            .field("env", &"<injected lookup>")
            .field("startup_cwd", &self.startup_cwd)
            .field("effective_uid", &self.effective_uid)
            .field("allow_zero_port", &self.allow_zero_port)
            .finish()
    }
}

/// Runs the BOOT-01 steps 1–2 startup checks.
///
/// Order is the pinned one: config-path resolution → root-user restriction →
/// full config load/validation → boot-value validation → cwd capture → music
/// root resolution. Nothing here creates a directory, opens a database, or
/// touches the music root other than to stat/read it.
pub fn preflight(options: PreflightOptions<'_>) -> Result<PreflightOutcome, PreflightError> {
    // 1a. Config path: flag, then ND_CONFIGFILE (existing-path rule), then the
    // loader's normal default tier.
    let resolved = resolve_config_file(options.config_file, options.env.as_deref())?;

    // 1b. Root-user restriction, before any filesystem operation
    // (conf/configuration.go:386).
    if resolved.config.enforce_non_root_user && is_root(options.effective_uid) {
        return Err(PreflightError::RunningAsRoot);
    }

    // 1c. Boot-value validation on the committed config. `load_with_env` has
    // already parsed the whole TOML, rejected unknown keys, applied the env
    // tier, and normalized — so an invalid document stops here, before the
    // data folder or DB path is used for anything.
    let session_timeout_secs =
        checked_secs("Server.SessionTimeout", resolved.config.session_timeout.0)?;
    let auth_window_secs = checked_secs(
        "Server.AuthWindowLength",
        resolved.config.auth_window_length.0,
    )?;
    validate_boot_values(&resolved.config, options.allow_zero_port)?;

    // 2a. Startup cwd, canonicalized, for relative-path resolution.
    let supplied_cwd = match options.startup_cwd {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| PreflightError::StartupCwdUnresolved {
            path: "<process cwd>".to_string(),
            reason: e.to_string(),
        })?,
    };
    let startup_cwd =
        std::fs::canonicalize(&supplied_cwd).map_err(|e| PreflightError::StartupCwdUnresolved {
            path: supplied_cwd.to_string_lossy().into_owned(),
            reason: e.to_string(),
        })?;

    // 2b. Music root: resolved against the startup cwd, canonicalized, and
    // required to be an existing readable directory. `Dir::path` is never
    // called on it.
    let music_root = resolve_music_root(&resolved.config.music_folder, &startup_cwd)?;

    // 2c. Data folder + DB path: reported only. Creation (Dir::path) belongs
    // to the increment that actually opens the database, so a preflight stays
    // repeatable and read-only. A relative DataFolder is resolved against the
    // startup cwd, matching the music-root rule; the SQLite string is handed
    // on verbatim because its URI query must not be treated as path
    // characters (D-OPEN owns that resolution).
    let data_folder = if resolved.config.data_folder.is_empty() {
        startup_cwd.clone()
    } else {
        let raw = Path::new(resolved.config.data_folder.as_str());
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            startup_cwd.join(raw)
        }
    };

    let db_path = resolved.config.db_path.clone();

    Ok(PreflightOutcome {
        config: resolved.config,
        config_source: resolved.source,
        startup_cwd,
        music_root,
        data_folder,
        db_path,
        session_timeout_secs,
        auth_window_secs,
    })
}

/// The config-path resolution rule, exposed for reuse by later bootstrap
/// (BOOT-03/04 own the CLI argument layer, which will call this).
///
/// Returns the loaded `Config` plus the source. Mirrors Go
/// `getConfigFile` (`conf/configuration.go:1195-1206`): the flag wins
/// outright; an env-named path is used only when it exists (the `os.Stat`
/// guard) and a missing one falls back to the normal loader defaults.
pub fn resolve_config_file(
    flag: Option<&str>,
    env: Option<&EnvLookup<'_>>,
) -> Result<ResolvedConfig, PreflightError> {
    let lookup = |k: &str| env.and_then(|f| f(k));

    let (path, source) = match flag.filter(|p| !p.is_empty()) {
        Some(p) => (Some(p.to_string()), ConfigSource::Flag),
        None => {
            let env_path = lookup(CONFIGFILE_ENV).filter(|v| !v.is_empty());
            match env_path {
                Some(p) => {
                    // Go's os.Stat guard: a non-existent target is skipped.
                    match std::fs::metadata(&p) {
                        Err(_) => (None, ConfigSource::Defaults),
                        Ok(md) => {
                            if md.is_file() {
                                (Some(p), ConfigSource::Env)
                            } else {
                                // Go would SetConfigFile(dir) and fatal on the
                                // read error; report it as the same class.
                                return Err(PreflightError::ConfigFileNotAFile {
                                    path: p,
                                    env: CONFIGFILE_ENV,
                                });
                            }
                        }
                    }
                }
                None => (None, ConfigSource::Defaults),
            }
        }
    };

    let used = path.clone().unwrap_or_default();
    let mut config = Config::load_with_env(path.as_deref(), |k| lookup(k)).map_err(|e| {
        PreflightError::ConfigLoad {
            path: used.clone(),
            message: sanitize_config_error(&e),
        }
    })?;
    // `config_file` is `#[serde(skip)]` (`nd-conf/src/config.rs:392`) and the
    // loader re-materializes the config through serde in `apply_pairs`
    // (`:896-918`), which resets it to `""` whenever a file tier ran. Go
    // records the file actually used (`Server.ConfigFile =
    // viper.ConfigFileUsed()`, conf/configuration.go:403), and startup logging
    // needs it, so the resolving side restores it here. The loader-side reset
    // is an `nd-conf` (#02/#03) defect, reported rather than patched in this lane.
    config.config_file = used;
    Ok(ResolvedConfig { config, source })
}

/// Result of [`resolve_config_file`].
pub struct ResolvedConfig {
    /// The loaded, normalized config.
    pub config: Config,
    /// Which tier supplied the file.
    pub source: ConfigSource,
}

/// Resolves the configured music root against `startup_cwd` and validates it.
///
/// No `Dir::path`/`create_dir_all`: the root must already exist. The returned
/// path is canonicalized (relative components, `..`, and symlinks resolved) so
/// step 5 compares a canonical root against the stored `library.path`.
pub fn resolve_music_root(configured: &str, startup_cwd: &Path) -> Result<PathBuf, PreflightError> {
    if configured.trim().is_empty() {
        return Err(PreflightError::EmptyMusicFolder);
    }
    let candidate = Path::new(configured);
    // Relative roots resolve against the supplied startup cwd (Go's
    // filepath.Abs semantics); absolute roots are used as written.
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        startup_cwd.join(candidate)
    };

    // canonicalize first (Go: filepath.Clean + fs.Stat), which also rejects a
    // missing path and a non-directory target.
    let canonical = std::fs::canonicalize(&joined).map_err(|e| {
        let text = joined.to_string_lossy().into_owned();
        match e.kind() {
            std::io::ErrorKind::NotFound => PreflightError::MusicRootMissing { path: text },
            std::io::ErrorKind::NotADirectory => {
                PreflightError::MusicRootNotDirectory { path: text }
            }
            std::io::ErrorKind::PermissionDenied => PreflightError::MusicRootNotReadable {
                path: text,
                reason: e.to_string(),
            },
            _ => PreflightError::MusicRootNotReadable {
                path: text,
                reason: e.to_string(),
            },
        }
    })?;

    if !canonical.is_dir() {
        return Err(PreflightError::MusicRootNotDirectory {
            path: canonical.to_string_lossy().into_owned(),
        });
    }

    // Readability = the directory can actually be listed, not merely stat'd.
    // Go's `fs.Stat` on the storage FS catches some permission cases; a
    // directory that stats but refuses `read_dir` is reported here instead.
    if let Err(e) = std::fs::read_dir(&canonical) {
        return Err(PreflightError::MusicRootNotReadable {
            path: canonical.to_string_lossy().into_owned(),
            reason: e.to_string(),
        });
    }

    Ok(canonical)
}

/// Validates the required boot values on a committed config.
///
/// `0` is refused unless `allow_zero_port` marks it as a test injection.
/// `Address`/TLS transports the port cannot serve yet are rejected here so a
/// configured value is never silently ignored.
pub fn validate_boot_values(config: &Config, allow_zero_port: bool) -> Result<(), PreflightError> {
    let port = config.port;
    if port == 0 {
        if !allow_zero_port {
            return Err(PreflightError::ZeroPort);
        }
    } else if port < 0 || port > i64::from(u16::MAX) {
        return Err(PreflightError::InvalidPort { value: port });
    }

    if config.auth_request_limit <= 0 {
        return Err(PreflightError::InvalidAuthRequestLimit {
            value: config.auth_request_limit,
        });
    }
    if config.auth_window_length.0 == Duration::ZERO {
        return Err(PreflightError::InvalidAuthWindowLength);
    }

    if !config.tls_cert.is_empty() || !config.tls_key.is_empty() {
        return Err(PreflightError::UnsupportedTls);
    }
    if config.address.starts_with("unix:") {
        return Err(PreflightError::UnsupportedUnixSocket {
            addr: config.address.clone(),
        });
    }

    Ok(())
}

/// Converts a Go-style duration to whole seconds with checked units — never
/// by feeding nanoseconds into a seconds field, and never truncating.
fn checked_secs(name: &'static str, d: Duration) -> Result<i64, PreflightError> {
    let ns = d.as_nanos();
    let secs = ns / 1_000_000_000;
    let frac = ns % 1_000_000_000;
    if frac != 0 || secs > i64::MAX as u128 {
        return Err(PreflightError::DurationConversion {
            name,
            duration: DurationFieldDisplay(d).to_string(),
        });
    }
    Ok(secs as i64)
}

/// Duration text for error messages, without pulling `Display` into
/// `nd-conf`'s public surface from here.
struct DurationFieldDisplay(Duration);

impl std::fmt::Display for DurationFieldDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let secs = self.0.as_secs_f64();
        write!(f, "{secs}s")
    }
}

/// `true` when the effective uid is 0.
///
/// Windows has no uid and Go skips the check there
/// (`conf/configuration.go:875`), so `None` on a non-unix platform means
/// "check not applicable".
fn is_root(injected: Option<u32>) -> bool {
    match injected {
        Some(uid) => uid == 0,
        None => {
            #[cfg(unix)]
            {
                rustix::process::geteuid().is_root()
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
    }
}

/// Reduces a `ConfigError` to a message with no source snippet and no URL
/// userinfo, so a secret on a malformed line cannot be echoed.
fn sanitize_config_error(err: &ConfigError) -> String {
    match err {
        ConfigError::Parse(_) => {
            // `toml::de::Error::Display` renders the offending source line
            // (`N | <source>` plus `|`/`^` caret art), which can contain a
            // password. Keep only the position and the violated rule.
            let rendered = err.to_string();
            let mut parts: Vec<&str> = Vec::new();
            for line in rendered.lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('|') || t.starts_with('^') || is_source_line(t) {
                    continue;
                }
                parts.push(t);
            }
            scrub_userinfo(&parts.join("; "))
        }
        other => scrub_userinfo(&other.to_string()),
    }
}

/// True for toml's rendered source lines: `<number> | <source>`.
fn is_source_line(line: &str) -> bool {
    let prefix: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    !prefix.is_empty() && line[prefix.len()..].trim_start().starts_with('|')
}

/// Replaces `scheme://user:pass@` with `scheme://***@` inside any URL-shaped
/// token (Go's BaseURL validation error echoes the whole URL).
fn scrub_userinfo(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        let after = &rest[i + 3..];
        // The authority ends at the first '/', '?', '#' or whitespace.
        let end = after
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"')
            .unwrap_or(after.len());
        let authority = &after[..end];
        if let Some(at) = authority.rfind('@') {
            out.push_str("***");
            out.push_str(&authority[at..]);
        } else {
            out.push_str(authority);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}
