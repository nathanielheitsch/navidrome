// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Startup preflight: the callable, side-effect-free checks BOOT-01 steps 1–2
//! freeze (`docs/rust-port/subplans/boot-api.md:192-193`).
//!
//! This module is a **library entry point for later bootstrap work**, not a
//! binary: [`preflight`] takes the startup cwd, an explicit config-file flag
//! and an injected environment, and returns a
//! [`PreflightOutcome`] carrying the committed `Config` plus the canonical
//! music root and data directory that later increments (DB open/migrate,
//! secrets, library seeding) consume.
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
//!   Relative config paths are anchored to the **startup cwd**, which is
//!   resolved once at the top of [`preflight`] and reused for the config file,
//!   the music root, and the data folder — Go likewise resolves every relative
//!   value against the working directory it starts in. A flag-named path that
//!   cannot be read is fatal (Go fatals at `viper.ReadInConfig`,
//!   `conf/configuration.go:1188-1191`); only the env tier has the
//!   skip-if-missing guard.
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
//! * **Secrets stay out of errors and `Debug`.** Config-load failures are
//!   reported structurally through [`ConfigErrorClass`]: an allowlisted class
//!   plus, for a TOML syntax error, the byte offset taken from the parse span.
//!   The document is never re-read to derive a position and no upstream text is
//!   forwarded, because `nd-conf`'s `ConfigError::Invalid` embeds the raw
//!   invalid value and `toml`'s `Display` quotes the offending source line —
//!   exactly how a `Password`/`ApiKey`/`Secret` could reach a log
//!   (`nd-conf/src/config.rs:30-48`, `:157`, `:288-290`, `:583`; there is no
//!   Rust redactor). The error type deliberately carries no `#[source]`:
//!   nesting `ConfigError` would re-expose the raw text through the source
//!   chain. Values this module could otherwise print are **omitted** rather
//!   than partially redacted: the outcome's `Debug` prints neither `Config`,
//!   `Address`, nor `db_path`, and the unsupported-socket error carries no
//!   address at all. The verbatim URI string is still handed to later
//!   bootstrap, where **D-OPEN owns its resolution**.

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

/// The allowlisted class of a configuration-load failure.
///
/// `nd-conf` reports load failures as four variants, two of which carry
/// arbitrary upstream text (`ConfigError::Invalid(String)` embeds the raw
/// invalid value, and `toml`'s error quotes the offending source line). Only
/// the *class* crosses this boundary, as a static description, so no config
/// value can be echoed. The actionable detail that survives is the config file
/// path plus a byte offset from the original parse span, when available.
///
/// Surfacing *which* key or field failed is safe once `nd-conf` carries it as a
/// typed field rather than free text; that is an `nd-conf` (#02/#03) change and
/// is reported rather than guessed here, because recovering it from
/// `Invalid(String)` would mean parsing upstream text — precisely what is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigErrorClass {
    /// The document is not valid TOML.
    #[error("the configuration file is not valid TOML")]
    TomlSyntax,
    /// A key is outside the accepted (fresh) key set.
    #[error(
        "the configuration file contains a key outside the accepted key set \
         (see docs/rust-port/config-contract.md)"
    )]
    UnknownKey,
    /// A key appears in both canonical and `ND_`-prefixed spelling.
    #[error("the configuration file contains a key in both canonical and ND_-prefixed spelling")]
    DuplicateNdKey,
    /// A value failed semantic validation (type, duration, byte size, URL,
    /// schedule, …).
    #[error("a configuration value failed validation")]
    InvalidValue,
}

/// Why a startup preflight refused to continue.
///
/// Variants carry no config values, no secret material, and no raw address;
/// [`PreflightError::ConfigLoad`] carries only an allowlisted class and a
/// numeric byte offset (see the module docs), and no variant has a `source`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    /// `EnforceNonRootUser` is set and the effective uid is 0 (posix only).
    #[error("EnforceNonRootUser is enabled but Navidrome is running as root")]
    RunningAsRoot,
    /// `ND_CONFIGFILE` names an existing path that is not a regular file.
    ///
    /// A *non-existent* env-named target is **not** an error: Go's `os.Stat`
    /// guard skips it and the loader falls back to its normal default tier
    /// (`conf/configuration.go:1201-1205`). A directory is fatal because Go
    /// also fatals there (`viper.ReadInConfig` fails on a non-file config
    /// target, `conf/configuration.go:1188-1191`).
    #[error("config file {path:?} named by {env:?} is not a regular file")]
    ConfigFileNotAFile {
        /// The path as resolved from the startup cwd.
        path: String,
        /// The environment variable that carried it.
        env: &'static str,
    },
    /// The configuration path selected by a tier is not valid UTF-8.
    ///
    /// `Config::load_with_env` takes `&str`, and the loader must never receive
    /// a **lossily** converted path: `to_string_lossy` rewrites invalid bytes to
    /// U+FFFD, which can silently name a *different existing* file — a sibling
    /// directory literally named with that replacement character. The path text
    /// is omitted here because those bytes cannot be rendered without loss.
    #[error("the configuration path selected by {tier} is not valid UTF-8")]
    ConfigPathNotUtf8 {
        /// Which tier selected the path (`"the --config flag"` or
        /// `"ND_CONFIGFILE"`).
        tier: &'static str,
    },
    /// An explicitly requested (flag-named) config file cannot be read.
    ///
    /// Go fatals when `cfgFile` names something it cannot read
    /// (`conf/configuration.go:1188-1191`), so a missing or non-file flag path
    /// is fatal here too — unlike `ND_CONFIGFILE`, which only carries the
    /// existing-file guard.
    #[error("config file {path:?} cannot be read: {reason}")]
    ConfigFileUnreadable {
        /// The path after anchoring to the startup cwd.
        path: String,
        /// OS error text, or `"not a regular file"`. Contains an errno
        /// description and the path, never config values.
        reason: String,
    },
    /// The config file could not be read, parsed, or validated.
    ///
    /// Only an allowlisted class and a numeric byte offset are reported, so
    /// neither document content nor an invalid value can reach the message (see
    /// the module docs). There is deliberately no `source()`: nesting
    /// `ConfigError` would re-expose the raw upstream text.
    #[error("invalid configuration in {path}: {class}{location}")]
    ConfigLoad {
        /// Config file that was read, or `"(defaults)"` when the default tier
        /// ran and no file was read.
        path: String,
        /// Allowlisted failure class.
        class: ConfigErrorClass,
        /// Allowlisted location suffix: `" at byte offset N"` for a TOML syntax
        /// error, empty otherwise. Digits only, taken from the original parse
        /// span — never document text.
        location: String,
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
    #[error("Server.Address uses a unix socket, which the Rust port cannot serve yet")]
    UnsupportedUnixSocket,
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
    /// The startup cwd could not be resolved.
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
    /// Startup cwd, canonicalized. Relative config, music-root and data-folder
    /// paths were all resolved against it.
    pub startup_cwd: PathBuf,
    /// Canonicalized configured music root. Never created by this code.
    pub music_root: PathBuf,
    /// Resolved data folder.
    ///
    /// Reported **without** creating it: `Dir::path` would `create_dir_all`,
    /// which step 2 permits, but a preflight must be repeatable and read-only.
    /// Later bootstrap calls `config.data_folder.path()` when it actually opens
    /// the database.
    pub data_folder: PathBuf,
    /// Resolved SQLite path/URI from the loader (`<DataFolder>/<DefaultDbPath>`
    /// plus the default URI parameters when `DbPath` is empty).
    ///
    /// Handed on **verbatim**: the URI query must not be treated as path
    /// characters, and **D-OPEN owns the resolution** of this string into an
    /// actual connection (path normalization, `?_pragma=` parameters, `file:`
    /// scheme handling). Nothing in this increment opens or creates a database.
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
/// `PreflightOutcome` carries a `Config` that derives `Debug` **including**
/// secret fields (`auth.password`, `last_fm.api_key`/`secret`,
/// `dev_auto_create_admin_password`). A manual impl therefore prints only the
/// non-secret resolution facts and never delegates to `Config`.
///
/// `address` and `db_path` are omitted entirely rather than partially
/// redacted: both are arbitrary caller-supplied strings that can carry
/// credentials (`Address` userinfo, a `DbPath` URI query), and a partial
/// redaction is still an disclosure surface. The values remain available
/// through the public fields for later bootstrap.
impl std::fmt::Debug for PreflightOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreflightOutcome")
            .field("config_source", &self.config_source)
            .field("config_file", &self.config.config_file)
            .field("port", &self.config.port)
            .field("startup_cwd", &self.startup_cwd)
            .field("music_root", &self.music_root)
            .field("data_folder", &self.data_folder)
            .field("session_timeout_secs", &self.session_timeout_secs)
            .field("auth_window_secs", &self.auth_window_secs)
            // Deliberately withheld: Config holds passwords and API
            // credentials, and `address`/`db_path` can carry caller-supplied
            // credentials of their own.
            .finish_non_exhaustive()
    }
}

/// Inputs to [`preflight`]. Every field is injectable so tests need no real
/// process cwd, no privileges, and no process environment.
#[derive(Default)]
pub struct PreflightOptions<'a> {
    /// Explicit config file (the `--config` flag equivalent). Wins over
    /// [`CONFIGFILE_ENV`]; an empty string counts as "not supplied" (Go's
    /// `cfgFile != ""` test). A relative value is anchored to `startup_cwd`.
    pub config_file: Option<&'a str>,
    /// Environment lookup, same signature as
    /// [`nd_conf::Config::load_with_env`].
    pub env: Option<Box<EnvLookup<'a>>>,
    /// The single working-directory anchor: relative config paths, the music
    /// root, and the data folder all resolve against it. `None` captures the
    /// real `std::env::current_dir()` once. Tests inject a temp root so the
    /// process cwd is never consulted.
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
/// Order is the pinned one: startup-cwd capture → config-path resolution →
/// root-user restriction → full config load/validation → boot-value validation
/// → music root resolution. Nothing is created and nothing is written; the only
/// filesystem access is read-only stat/canonicalize/listing of the config
/// path, the startup cwd, and the music root.
pub fn preflight(options: PreflightOptions<'_>) -> Result<PreflightOutcome, PreflightError> {
    // 0. One working-directory anchor, captured once and reused: it resolves
    // relative config paths below, and the music root and data folder after.
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

    // 1a. Config path: flag, then ND_CONFIGFILE (existing-path rule), then the
    // loader's normal default tier. A relative path is anchored to the startup
    // cwd captured above.
    let resolved = resolve_config_file(options.config_file, options.env.as_deref(), &startup_cwd)?;

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

    // 2a. Music root and data folder resolve against the same anchor the
    // config file was resolved against (captured in step 0).

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
        anchor_path(resolved.config.data_folder.as_str(), &startup_cwd)
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

/// Joins a relative path onto `cwd`; absolute paths are used as written.
///
/// Go's equivalent is `filepath.Abs`/`filepath.IsAbs` at the point each value
/// is consumed, so which cwd anchors a value is decided by the consumer.
/// Checked conversion of a selected config path to the `&str` the loader takes.
///
/// `to_string_lossy` is never used here: invalid bytes would be rewritten to
/// U+FFFD and could then name a different, existing file. The path is rejected
/// instead, and its bytes are not reported.
fn checked_path_text(path: &Path, tier: &'static str) -> Result<String, PreflightError> {
    match path.to_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(PreflightError::ConfigPathNotUtf8 { tier }),
    }
}

fn anchor_path(value: &str, cwd: &Path) -> PathBuf {
    let raw = Path::new(value);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    }
}

/// The config-path resolution rule, exposed for reuse by later bootstrap
/// (BOOT-03/04 own the CLI argument layer, which will call this).
///
/// Returns the loaded `Config` plus the source. Mirrors Go `getConfigFile`
/// (`conf/configuration.go:1195-1206`): the flag wins outright; an env-named
/// path is used only when it exists (the `os.Stat` guard) and a missing one
/// falls back to the normal loader defaults. A relative path is anchored to
/// `startup_cwd`; absolute paths are used as written.
pub fn resolve_config_file(
    flag: Option<&str>,
    env: Option<&EnvLookup<'_>>,
    startup_cwd: &Path,
) -> Result<ResolvedConfig, PreflightError> {
    let lookup = |k: &str| env.and_then(|f| f(k));
    let anchor = |value: &str| anchor_path(value, startup_cwd);

    // The selected path stays a `PathBuf` until the loader boundary: a lossy
    // conversion could silently name a *different existing* file (see
    // [`PreflightError::ConfigPathNotUtf8`]), so the text handed to the loader
    // comes from a checked conversion only.
    let (path, source) = match flag.filter(|p| !p.is_empty()) {
        Some(p) => {
            // No skip-if-missing guard on the flag tier: an explicitly
            // requested config file that cannot be read is fatal in Go, so it
            // is fatal here — with the class named structurally rather than
            // forwarded from the loader.
            let joined = anchor(p);
            match std::fs::metadata(&joined) {
                Err(e) => {
                    return Err(PreflightError::ConfigFileUnreadable {
                        path: joined.to_string_lossy().into_owned(),
                        reason: e.to_string(),
                    })
                }
                Ok(md) if !md.is_file() => {
                    return Err(PreflightError::ConfigFileUnreadable {
                        path: joined.to_string_lossy().into_owned(),
                        reason: "not a regular file".to_string(),
                    })
                }
                Ok(_) => (
                    checked_path_text(&joined, "--config flag")?,
                    ConfigSource::Flag,
                ),
            }
        }
        None => {
            let env_path = lookup(CONFIGFILE_ENV).filter(|v| !v.is_empty());
            match env_path {
                Some(p) => {
                    let joined = anchor(&p);
                    // Go's os.Stat guard: a non-existent target is skipped.
                    match std::fs::metadata(&joined) {
                        Err(_) => (String::new(), ConfigSource::Defaults),
                        Ok(md) => {
                            if md.is_file() {
                                (
                                    checked_path_text(&joined, CONFIGFILE_ENV)?,
                                    ConfigSource::Env,
                                )
                            } else {
                                // Go would SetConfigFile(dir) and fatal on the
                                // read error; report it as the same class.
                                return Err(PreflightError::ConfigFileNotAFile {
                                    path: joined.to_string_lossy().into_owned(),
                                    env: CONFIGFILE_ENV,
                                });
                            }
                        }
                    }
                }
                None => (String::new(), ConfigSource::Defaults),
            }
        }
    };

    // When a tier ran, the loader receives an absolute path, so its own
    // `read_to_string` cannot land on a different cwd than the guard above.
    let used = path.clone();
    let mut config = Config::load_with_env(
        if used.is_empty() {
            None
        } else {
            Some(used.as_str())
        },
        |k| lookup(k),
    )
    .map_err(|e| PreflightError::ConfigLoad {
        path: if used.is_empty() {
            "(defaults)".to_string()
        } else {
            used.clone()
        },
        class: config_error_class(&e),
        location: config_error_location(&e),
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

/// Maps an `nd-conf` load failure onto an allowlisted class, discarding the
/// variant payloads, which carry raw config text.
fn config_error_class(err: &ConfigError) -> ConfigErrorClass {
    match err {
        ConfigError::Parse(_) => ConfigErrorClass::TomlSyntax,
        ConfigError::UnknownKey(_) => ConfigErrorClass::UnknownKey,
        ConfigError::DuplicateNdKey { .. } => ConfigErrorClass::DuplicateNdKey,
        ConfigError::Invalid(_) => ConfigErrorClass::InvalidValue,
    }
}

/// Location suffix for a config-load failure: digits only.
///
/// A TOML syntax error carries a byte span, which is reported as an offset with
/// no further computation: the document is never re-read to derive a line or
/// column, so there is no window in which a replaced config file could be
/// measured, and no document text can be touched. Every other class reports no
/// location, because the loader's text for those embeds the raw invalid value
/// and there is no typed position to fall back on.
fn config_error_location(err: &ConfigError) -> String {
    let ConfigError::Parse(toml_err) = err else {
        return String::new();
    };
    match toml_err.span() {
        Some(span) => format!(" at byte offset {}", span.start),
        None => String::new(),
    }
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
    // Relative roots resolve against the supplied startup cwd, not the process
    // cwd: Go computes the root from the *startup* working directory while the
    // config file was already read from wherever the flag/env named it.
    let joined = anchor_path(configured, startup_cwd);

    // canonicalize first (Go: filepath.Clean + fs.Stat), which also rejects a
    // missing path and a non-directory target.
    let canonical = std::fs::canonicalize(&joined).map_err(|e| {
        let text = joined.to_string_lossy().into_owned();
        match e.kind() {
            std::io::ErrorKind::NotFound => PreflightError::MusicRootMissing { path: text },
            std::io::ErrorKind::NotADirectory => {
                PreflightError::MusicRootNotDirectory { path: text }
            }
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
        // The address is deliberately not carried: a URL-shaped value is
        // caller-supplied and may contain userinfo.
        return Err(PreflightError::UnsupportedUnixSocket);
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
