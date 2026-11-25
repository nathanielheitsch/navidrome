// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! BOOT-01 startup assembly: directories, database, and transactional setup in
//! one callable async step (`docs/rust-port/subplans/boot-api.md:193-197`,
//! steps 2–6).
//!
//! [`initialize`] consumes the increment-A [`crate::preflight::PreflightOutcome`]
//! and returns an [`InitializedState`] carrying an open, migrated, pooled
//! [`nd_db::store::SQLStore`] plus the [`crate::bootstrap::BootstrapOutcome`] that
//! actually committed. It is a **library entry point**, not a binary: there is no
//! `main`, no CLI argument layer, no listener, no `Authenticator`, no cache, no
//! limiter, no ffmpeg wiring, and no scanner here.
//!
//! The whole heavy chain — directory creation, database open/migrate, argon2id
//! hashing, signing-material generation, and the bootstrap transaction — runs
//! inside [`tokio::task::spawn_blocking`], so nothing blocks an async worker
//! thread (`nd_db::store` documents the same boundary: rusqlite stays
//! synchronous in the crate and the async boundary is at the store verb). Before
//! any of that work starts, the blocking closure sends its own
//! [`std::thread::ThreadId`] back through a oneshot; [`InitializedState`] reports
//! it as `worker_thread`, and the tests assert it differs from the thread the
//! `async fn` resumed on. That is a deterministic observation, not a timing
//! heuristic: no sleep, no threshold, no race.
//!
//! Order is the plan's order, and the pre-checks come **before** the first
//! side effect:
//!
//! 1. Data/cache directory paths are anchored to `PreflightOutcome::startup_cwd`
//!    and UTF-8 checked. A relative `Dir` is joined onto the startup cwd — the
//!    same anchor the preflight used for the config file and the music root — and
//!    re-created with [`nd_conf::Dir::with_perm`] carrying the **original**
//!    [`nd_conf::Dir::permissions()`], so a permission a callable preflight set
//!    (`Dir::with_perm`, e.g. Go's `NewDirWithPerm(..., 0700)` for plugins,
//!    `conf/configuration.go:397`) survives the anchoring instead of being
//!    reconstructed with defaults.
//! 2. [`crate::database::resolve_input`] classifies the raw `DbPath` before a
//!    single byte is written, so a malformed DSN or a private-memory target
//!    creates no directory at all. The resolved string is discarded and the input
//!    is handed to [`crate::database::open_database`] **verbatim**, so `D-OPEN`
//!    owns URI resolution, the query is never treated as filename characters, and
//!    the reported [`crate::database::DatabaseTarget`] states whether the path was
//!    anchored to the startup cwd.
//! 3. Only the data and cache directories are created, via the existing
//!    [`nd_conf::Dir::path`] accessor (`create_dir_all` + permission,
//!    `nd-conf/src/dir.rs`). The music root is never created: preflight already
//!    required it to exist, and `Dir::path` on it would boot against an empty
//!    directory (`boot-api.md:187`, `boot01-scout-memo.md` §2b). An empty
//!    configured `Dir` is skipped exactly like Go's `Dir.Path()`
//!    (`conf/dir.go:39-41`).
//! 4. [`crate::database::open_database`] opens + migrates and assembles the pool;
//!    [`crate::bootstrap::bootstrap`] then runs steps 4–6 as one retryable
//!    transaction. A ready state is returned **only** after both succeeded and the
//!    transaction committed.
//!
//! Secrets and paths stay inside: [`StartupError`] carries a static stage, a
//! static class and numeric codes, with no `#[source]` — an [`std::io::Error`]
//! message, a `JoinError`, a DSN, a path, a password or a hash never crosses the
//! boundary. `PasswordEncryptionKey` is deliberately **not** read from `Config`:
//! the fresh TOML contract omits it (`config-contract.md:18`,
//! `credentials.md:66-71`), so the documented public default is used, which is
//! what keeps existing Go-generated properties decryptable. Reporting that as an
//! open launch decision is part of this lane; inventing a config field is not.

use std::path::{Path, PathBuf};
use std::thread::ThreadId;

use chrono::{DateTime, Utc};
use nd_conf::Dir;
use nd_db::store::SQLStore;

use crate::bootstrap::{bootstrap, BootstrapError, BootstrapInput, BootstrapOutcome};
use crate::database::{
    open_database, resolve_input, DatabaseError, DatabaseOptions, DatabaseTarget,
};
use crate::preflight::PreflightOutcome;

/// The stage of the startup assembly that failed. Static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupStage {
    /// A directory path could not be anchored or rendered as UTF-8.
    Paths,
    /// The DB input was rejected, before anything was created.
    DatabaseInput,
    /// A configured directory could not be created.
    Directories,
    /// The database could not be opened or migrated.
    Database,
    /// The bootstrap transaction refused or failed; nothing committed.
    Bootstrap,
    /// The blocking task failed (panic or cancellation) before returning.
    BlockingTask,
}

impl std::fmt::Display for StartupStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            StartupStage::Paths => "resolving directory paths",
            StartupStage::DatabaseInput => "classifying the database input",
            StartupStage::Directories => "creating the data and cache directories",
            StartupStage::Database => "opening and migrating the database",
            StartupStage::Bootstrap => "running the startup transaction",
            StartupStage::BlockingTask => "running the blocking startup task",
        };
        f.write_str(name)
    }
}

/// Why startup failed, as a static class plus optional numeric codes.
///
/// Deliberately not `#[source]`-chained: an [`std::io::Error`] names the path it
/// failed on and a [`tokio::task::JoinError`] prints the panic payload, so
/// forwarding either would put filesystem detail — and potentially a value from a
/// config path — into a log. The raw OS `errno` is kept as a number only, in the
/// same spirit as bootstrap's SQLite result codes.
#[derive(Debug, thiserror::Error)]
pub struct StartupError {
    /// Which step failed.
    pub stage: StartupStage,
    /// Static reason.
    pub reason: &'static str,
    /// Raw OS error code (`errno`), when the failure came from the filesystem.
    pub errno: Option<i32>,
    /// Numeric SQLite codes, when the failure came from SQLite.
    pub sqlite: Option<crate::database::SqliteCode>,
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stage = self.stage;
        let reason = self.reason;
        match (self.errno, self.sqlite) {
            (Some(errno), _) => {
                write!(f, "startup failed during {stage}: {reason} (errno {errno})")
            }
            (None, Some(code)) => write!(
                f,
                "startup failed during {stage}: {reason} (sqlite code {}, extended {})",
                code.primary, code.extended
            ),
            (None, None) => write!(f, "startup failed during {stage}: {reason}"),
        }
    }
}

impl StartupError {
    fn path(stage: StartupStage, reason: &'static str) -> Self {
        StartupError {
            stage,
            reason,
            errno: None,
            sqlite: None,
        }
    }

    /// An I/O failure: the class is static and only the numeric `errno` survives.
    fn io(stage: StartupStage, reason: &'static str, err: &std::io::Error) -> Self {
        StartupError {
            stage,
            reason,
            errno: err.raw_os_error(),
            sqlite: None,
        }
    }
}

/// What startup resolved and produced.
///
/// The store is reachable but not printable: [`SQLStore`]'s derived `Debug` prints
/// its pool config, i.e. the DSN with its query, so it is withheld. The bootstrap
/// outcome holds [`crate::signing_keys::SigningSecret`] values whose own `Debug`
/// is already redacted, and no password, hash or ciphertext is stored here at all.
pub struct InitializedState {
    /// Open, migrated, fully pooled store, ready for later service wiring.
    pub store: SQLStore,
    /// The transactional setup that COMMITTED. Not a plan: every field was read
    /// back from the committing transaction.
    pub bootstrap: BootstrapOutcome,
    /// Database resolution facts (never the DSN itself).
    pub database: DatabaseTarget,
    /// Data directory actually created (or verified) by this call.
    pub data_dir: PathBuf,
    /// Cache directory actually created (or verified) by this call.
    pub cache_dir: PathBuf,
    /// The blocking worker thread the heavy chain ran on, captured by the
    /// heartbeat the closure sends before doing any work. `Some` whenever the task
    /// started; a `BlockingTask` failure therefore never surfaces one.
    pub worker_thread: ThreadId,
}

impl std::fmt::Debug for InitializedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitializedState")
            .field("bootstrap", &self.bootstrap)
            .field("database", &self.database)
            .field("data_dir", &self.data_dir)
            .field("cache_dir", &self.cache_dir)
            .field("worker_thread", &self.worker_thread)
            // Deliberately withheld: the store's Debug prints the pool DSN.
            .field("store", &"<pooled store>")
            .finish()
    }
}

/// Join a configured [`Dir`] onto the startup cwd and check its UTF-8 text.
///
/// Absolute paths are used as written; `to_str` is used rather than
/// `to_string_lossy`, because a lossy conversion could name a *different*
/// directory. The returned [`Dir`] preserves the original permissions.
///
/// # Errors
/// [`StartupStage::Paths`] when the anchored path cannot be rendered as UTF-8.
fn anchored_dir(configured: &Dir, startup_cwd: &Path) -> Result<Dir, StartupError> {
    let raw = Path::new(configured.as_str());
    let anchored = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        startup_cwd.join(raw)
    };
    let text = anchored.to_str().ok_or_else(|| {
        StartupError::path(StartupStage::Paths, "anchored directory path is not UTF-8")
    })?;
    Ok(Dir::with_perm(text.to_string(), configured.permissions()))
}

/// The owned work a blocking startup task performs. Built on the calling thread
/// after the path/D-OPEN pre-checks, then moved into [`spawn_blocking`] so the
/// closure borrows nothing.
struct BlockingJob {
    data: Dir,
    cache: Dir,
    /// Already classified by [`resolve_input`], handed to `open_database` verbatim.
    /// The raw `Config::db_path`. `open_database` re-runs the same classification
    /// and owns the anchoring decision, so the target facts report it honestly.
    db_path: String,
    startup_cwd: PathBuf,
    music_root: PathBuf,
    dev_password: Option<String>,
    pool_size: usize,
    now: DateTime<Utc>,
}

/// What the blocking chain produced, before it is folded into an
/// [`InitializedState`].
struct BlockingOutcome {
    store: SQLStore,
    bootstrap: BootstrapOutcome,
    database: DatabaseTarget,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

/// The blocking half of startup: directory creation, open/migrate, hashing and
/// the bootstrap transaction. Runs only on a `spawn_blocking` worker.
fn startup_blocking(job: BlockingJob) -> Result<BlockingOutcome, StartupError> {
    // Step 3: create only data and cache. `Dir::path` is `create_dir_all` plus the
    // permission, applied only when the directory is created, so an existing one
    // is left exactly as it is (Go `conf/dir.go:44`).
    let data_dir = job.data.path().map_err(|err| {
        StartupError::io(
            StartupStage::Directories,
            "data directory could not be created",
            &err,
        )
    })?;
    let cache_dir = job.cache.path().map_err(|err| {
        StartupError::io(
            StartupStage::Directories,
            "cache directory could not be created",
            &err,
        )
    })?;

    // Step 4a: open + migrate + pool. The raw DbPath is passed through, so its URI
    // query is resolved by D-OPEN and never treated as filename characters.
    let opened = open_database(DatabaseOptions {
        db_path: job.db_path.as_str(),
        startup_cwd: job.startup_cwd.as_path(),
        pool_size: job.pool_size,
    })
    .map_err(database_failure)?;

    // Step 4b: steps 4-6 as one retryable transaction. `PasswordEncryptionKey` is
    // not a field of the fresh TOML config (config-contract.md:18), so the
    // documented public default keys the envelope.
    let input = BootstrapInput {
        music_root: job.music_root.as_path(),
        password_encryption_key: "",
        dev_password: job.dev_password.as_deref(),
        now: job.now,
    };
    let outcome = bootstrap(&opened.store, &input).map_err(|err| bootstrap_failure(&err))?;

    Ok(BlockingOutcome {
        store: opened.store,
        bootstrap: outcome,
        database: opened.target,
        data_dir,
        cache_dir,
    })
}

/// Map a database-assembly failure, keeping its stage and numeric codes.
///
/// `DatabaseError`'s own reason text is replaced by a static string here: its
/// classes are already static, but re-stating them keeps this boundary's reason
/// type `&'static str` and guarantees no upstream text can ever appear.
fn database_failure(err: DatabaseError) -> StartupError {
    let (stage, reason) = match err.stage {
        crate::database::DbStage::Input => (
            StartupStage::DatabaseInput,
            "database input is not supported",
        ),
        crate::database::DbStage::MigrationOpen => {
            (StartupStage::Database, "the database could not be opened")
        }
        crate::database::DbStage::Migration => {
            (StartupStage::Database, "the fresh migration failed")
        }
        crate::database::DbStage::Pool => {
            (StartupStage::Database, "pool handles could not be opened")
        }
        crate::database::DbStage::Admission => (
            StartupStage::Database,
            "migration handle was refused admission",
        ),
    };
    StartupError {
        stage,
        reason,
        errno: None,
        sqlite: err.sqlite,
    }
}

/// Map a bootstrap failure. Every `BootstrapError` variant is already static — a
/// domain refusal, a key/password class, or a stage plus SQLite result codes — so
/// this reduces it to a stage and one static reason rather than chaining it, and
/// keeps only the numeric codes.
fn bootstrap_failure(err: &BootstrapError) -> StartupError {
    let mut sqlite = None;
    let reason = match err {
        BootstrapError::Domain(_) => "startup refused by the committed database state",
        BootstrapError::RelativeMusicRoot => "the music root is not absolute",
        BootstrapError::NonUtf8MusicRoot => "the music root is not valid UTF-8",
        BootstrapError::Password(_) => "the dev password could not be hashed",
        BootstrapError::Entropy(_) => "signing material could not be generated",
        BootstrapError::Store(failure) => match failure.kind {
            crate::bootstrap::FailureKind::BusyExhausted { .. } => {
                "the startup transaction remained busy"
            }
            crate::bootstrap::FailureKind::Sqlite { primary, extended } => {
                sqlite = Some(crate::database::SqliteCode { primary, extended });
                "the startup transaction failed"
            }
            crate::bootstrap::FailureKind::MissingRow => {
                "a row the startup transaction requires is absent"
            }
            crate::bootstrap::FailureKind::Pool => "no database connection was available",
        },
    };
    StartupError {
        stage: StartupStage::Bootstrap,
        reason,
        errno: None,
        sqlite,
    }
}

/// Run BOOT-01 steps 2–6 against a preflight outcome.
///
/// The DB input is classified and both directory paths are anchored and UTF-8
/// checked **before** anything is created, so a bad DSN or a non-UTF-8 path leaves
/// the filesystem untouched. Everything after that point — directory creation,
/// open/migrate, hashing, secret generation, and the bootstrap transaction — runs
/// on a blocking worker thread.
///
/// # Errors
/// [`StartupError`] with a static stage and class. No ready state is produced for
/// any failure: a bootstrap refusal or a schema failure never yields a store, and
/// a failed transaction commits nothing.
pub async fn initialize(
    preflight: &PreflightOutcome,
    now: DateTime<Utc>,
) -> Result<InitializedState, StartupError> {
    // Pre-checks on the calling thread: cheap, side-effect free, and they must
    // happen before a directory exists.
    let data = anchored_dir(&preflight.config.data_folder, &preflight.startup_cwd)?;
    let cache = anchored_dir(&preflight.config.cache_folder, &preflight.startup_cwd)?;
    // Classification only: the resolved string is discarded so `open_database`
    // receives the raw input and reports its own anchoring decision. Doing this
    // here is what guarantees a refused input never reaches directory creation.
    resolve_input(preflight.db_path.as_str(), &preflight.startup_cwd).map_err(database_failure)?;

    // Everything the blocking task needs is owned, so the closure borrows nothing.
    let job = BlockingJob {
        data,
        cache,
        db_path: preflight.db_path.clone(),
        startup_cwd: preflight.startup_cwd.clone(),
        music_root: preflight.music_root.clone(),
        dev_password: if preflight.config.dev_auto_create_admin_password.is_empty() {
            None
        } else {
            Some(preflight.config.dev_auto_create_admin_password.clone())
        },
        pool_size: nd_db::pool::pool_size(),
        now,
    };

    // Deterministic evidence that the heavy chain is off the caller's runtime:
    // the closure reports its own thread id before doing any work.
    let (probe, observed) = tokio::sync::oneshot::channel::<ThreadId>();
    let worker = tokio::task::spawn_blocking(move || {
        if probe.send(std::thread::current().id()).is_err() {
            // Receiver dropped: the caller went away before any work happened.
            return Err(StartupError::path(
                StartupStage::BlockingTask,
                "startup was cancelled before it started",
            ));
        }
        startup_blocking(job)
    });

    // `observed` resolves as soon as the closure starts, i.e. before the heavy
    // work; `await`ing it here does not block the runtime thread.
    let worker_thread = observed.await.map_err(|_| {
        StartupError::path(
            StartupStage::BlockingTask,
            "startup blocking task ended without starting",
        )
    })?;

    // Two layers: the JoinError (panic/cancel) and the task's own error. Neither is
    // forwarded as text — a JoinError prints the panic payload.
    let done = worker.await.map_err(|_| {
        StartupError::path(
            StartupStage::BlockingTask,
            "startup blocking task failed before completing",
        )
    })??;

    Ok(InitializedState {
        store: done.store,
        bootstrap: done.bootstrap,
        database: done.database,
        data_dir: done.data_dir,
        cache_dir: done.cache_dir,
        worker_thread,
    })
}
