// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! BOOT-01 composition: assemble the ready service state from one startup run.
//!
//! [`prepare`] is the single callable composition. It performs **exactly one**
//! [`crate::startup::initialize`], then builds the shared [`StreamState`] and
//! [`AuthState`] from its result. It re-runs no bootstrap step, opens no second
//! pool, and stores no extra copy of the signing secrets: the committed winners
//! move into the `Authenticator` inside [`AuthState`] and nowhere else. The ready
//! value carries the [`AuthState`] and [`StreamState`] plus non-secret launch
//! metadata; the store is reached through `ready.auth.store`.
//!
//! ## One pre-write validation policy, shared with the stream slice
//!
//! Before anything writes, `prepare` runs the *same*
//! [`crate::stream_state::validate_config`] that [`StreamState::new`] uses, plus a
//! positivity re-check of the three preflight-checked auth numbers. Because those
//! are validated **before** [`initialize`], an invalid stream budget or a bad cap
//! or a non-positive auth number produces no data directory, no cache directory,
//! and no database file. There is no duplicated rule and no public testing-only
//! hook: both consumers call the one helper. `PreflightOutcome` fields are public
//! and externally mutable, so re-checking here (rather than trusting the
//! preflight result passed in) is deliberate, not redundant.
//!
//! ## Ordering and what is preserved
//!
//! `initialize` returns an [`crate::startup::InitializedState`] which
//! [`AuthState::new`] consumes by value. The non-secret facts a later service
//! wiring needs — the resolved directories, the [`crate::database::DatabaseTarget`]
//! facts, and the blocking worker-thread id — are captured from it *before* that
//! consumption. The [`crate::bootstrap::BootstrapOutcome`] is intentionally **not**
//! retained: it owns the decrypted [`crate::signing_keys::SigningSecret`] values,
//! and their only destination is the authenticator. No `Config` is stored either,
//! so `ServiceReady` can be `Debug`-printed without exposing passwords or DSNs.
//!
//! ## Failure semantics (documented, not overclaimed)
//!
//! Startup itself commits database initialization (library, signing secrets,
//! setup marker, optional dev admin) in one transaction. The stream-state cache
//! construction is a **filesystem** step that runs *after* that commit. A cache
//! directory failure therefore can leave a committed, migrated database with no
//! transcoding cache — this composition is **not** all-or-nothing across the whole
//! startup and provides no rollback of the committed transaction if a later step
//! fails. `prepare` returns no ready state on any failure and does not undo a
//! commit; a retry sees the previously committed database and proceeds
//! idempotently, which is the restartable behavior BOOT-01 specifies. Cancellation
//! likewise does not roll back a committed transaction.
//!
//! ## Disclosure
//!
//! [`ServiceReadyError`] carries a static stage and the offending component's own
//! error, all of which were verified disclosure-safe: [`crate::startup::StartupError`]
//! holds a static reason plus numeric `errno`/SQLite codes with no source chain;
//! [`crate::stream_state::StreamStateError`] holds static classes plus a numeric
//! `errno`; [`crate::state::AuthStateError`] holds static text plus numeric
//! values. None carries a config value, DSN, credential, or path. [`ServiceReady`]
//! has a manual `Debug` that prints only non-secret launch facts, delegating to the
//! already-redacted `AuthState`/`StreamState` debuggers.

use std::path::PathBuf;
use std::thread::ThreadId;

use chrono::{DateTime, Utc};

use crate::database::DatabaseTarget;
use crate::preflight::PreflightOutcome;
use crate::startup::{initialize, StartupError};
use crate::state::{AuthState, AuthStateError};
use crate::stream_state::{validate_config, StreamState, StreamStateError};

/// Which composition step failed. Static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyStage {
    /// Stream budget/caps or the auth numbers were invalid, before any write.
    Validation,
    /// Startup (directories, open/migrate, bootstrap) failed.
    Startup,
    /// The transcoding cache could not be constructed (after the DB commit).
    Stream,
    /// The auth state could not be constructed.
    Auth,
}

impl std::fmt::Display for ReadyStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ReadyStage::Validation => "validating launch values",
            ReadyStage::Startup => "starting up the service",
            ReadyStage::Stream => "constructing the transcoding cache",
            ReadyStage::Auth => "constructing the auth state",
        };
        f.write_str(name)
    }
}

/// Why `prepare` failed: a static stage plus the safe component error.
///
/// Each variant wraps a component error whose own `Display`/`Debug` is
/// disclosure-safe (see the module doc), so this wrapper never carries a config
/// value, DSN, credential, or path. It deliberately does not invent a new reason
/// string — the component's static class is the diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum ServiceReadyError {
    /// A launch value was invalid before any write. [`ValidationCause`] names the
    /// class statically; the stage is always [`ReadyStage::Validation`].
    #[error("invalid launch value during {}", ReadyStage::Validation)]
    Validation(#[source] ValidationCause),
    /// Startup failed. The wrapped [`StartupError`] is disclosure-safe (static
    /// reason plus numeric `errno`/SQLite codes, no source chain).
    #[error("startup failed during {}", ReadyStage::Startup)]
    Startup(#[source] StartupError),
    /// The transcoding cache failed to construct. This happens **after** the
    /// database transaction committed; see the module doc on non-atomicity. The
    /// wrapped [`StreamStateError`] is disclosure-safe (static class plus a numeric
    /// `errno`).
    #[error("transcoding cache failed during {}", ReadyStage::Stream)]
    Stream(#[source] StreamStateError),
    /// The auth state failed to construct. The wrapped [`AuthStateError`] is
    /// disclosure-safe (static text plus numeric values).
    #[error("auth state failed during {}", ReadyStage::Auth)]
    Auth(#[source] AuthStateError),
}

impl ServiceReadyError {
    /// The static stage of the failure.
    #[must_use]
    pub fn stage(&self) -> ReadyStage {
        match self {
            ServiceReadyError::Validation(_) => ReadyStage::Validation,
            ServiceReadyError::Startup(_) => ReadyStage::Startup,
            ServiceReadyError::Stream(_) => ReadyStage::Stream,
            ServiceReadyError::Auth(_) => ReadyStage::Auth,
        }
    }
}

/// The validation-stage cause, reduced to static text.
///
/// The pre-write validation can fail for either the stream config or an auth
/// number. Both underlying errors are disclosure-safe, but this variant exposes a
/// single static class so `stage()`/`Display` stay uniform. The numeric detail the
/// underlying errors carry (e.g. the offending budget class) is preserved via
/// `Display`.
/// The pre-write validation cause. Each arm carries the offending component
/// error, which is disclosure-safe: [`AuthStateError`] is static text plus numeric
/// values, [`StreamStateError`] is a static class plus a numeric `errno`. Neither
/// can carry a config value, credential, DSN, or path.
#[derive(Debug, thiserror::Error)]
pub enum ValidationCause {
    /// One of the three auth numbers is not positive.
    #[error("an auth timeout, window, or limit is not positive")]
    AuthNumber(#[source] AuthStateError),
    /// The stream budget or a concurrency cap is invalid.
    #[error("the transcoding budget or a concurrency cap is invalid")]
    StreamConfig(#[source] StreamStateError),
}

/// The ready service state, produced by one startup run.
///
/// `Clone` is cheap and shares everything: `auth` clones share the pool and the
/// auth limiter, `stream` clones share the cache and the transcode limiter. The
/// store is reached through `auth.store`; there is no second store and no
/// retained signing secret here.
#[derive(Clone)]
pub struct ServiceReady {
    /// Auth state: pool-shared store, committed-key authenticator, shared auth
    /// limiter, and the checked auth numbers.
    pub auth: AuthState,
    /// Shared transcoding cache and concurrency limiter.
    pub stream: StreamState,
    /// Database resolution facts (never the DSN).
    pub database: DatabaseTarget,
    /// Data directory created/verified by startup. Non-secret.
    pub data_dir: PathBuf,
    /// Cache directory created/verified by startup. Non-secret.
    pub cache_dir: PathBuf,
    /// Resolved, canonical music root. Non-secret.
    pub music_root: PathBuf,
    /// The blocking worker thread the heavy startup chain ran on.
    pub worker_thread: ThreadId,
}

impl ServiceReady {
    /// Compose the ready service state from a completed preflight (BOOT-01).
    ///
    /// Runs the shared stream-config validation and an auth-number positivity
    /// check **before** the single `initialize`, so invalid launch values leave no
    /// data directory, cache directory, or database file. After startup it builds
    /// the cache from the resolved cache directory and the auth state from the
    /// committed secrets, capturing the non-secret metadata before `AuthState::new`
    /// consumes the initialized state.
    ///
    /// # Errors
    /// [`ServiceReadyError`] naming the failed stage. No ready state is produced on
    /// failure; a failure after the database commit does not roll it back (see the
    /// module doc).
    pub async fn prepare(
        preflight: &PreflightOutcome,
        now: DateTime<Utc>,
    ) -> Result<Self, ServiceReadyError> {
        // --- Pre-write validation: no side effect happens before these pass. ---
        // The three auth numbers are pub-mutable on the outcome; re-check the exact
        // positive contract AuthState::new requires, before initialize writes.
        let session_timeout_secs = preflight.session_timeout_secs;
        let auth_window_secs = preflight.auth_window_secs;
        let auth_request_limit = preflight.config.auth_request_limit;
        if session_timeout_secs <= 0 {
            return Err(ServiceReadyError::Validation(ValidationCause::AuthNumber(
                AuthStateError::NonPositiveSessionTimeout {
                    value: session_timeout_secs,
                },
            )));
        }
        if auth_window_secs <= 0 {
            return Err(ServiceReadyError::Validation(ValidationCause::AuthNumber(
                AuthStateError::NonPositiveAuthWindow {
                    value: auth_window_secs,
                },
            )));
        }
        if auth_request_limit <= 0 {
            return Err(ServiceReadyError::Validation(ValidationCause::AuthNumber(
                AuthStateError::NonPositiveAuthRequestLimit {
                    value: auth_request_limit,
                },
            )));
        }
        // The one shared stream policy, applied before initialize so a bad budget
        // or cap creates no directory and no database.
        validate_config(
            &preflight.config.transcoding_cache_size,
            preflight.config.transcoding.max_concurrent,
            preflight.config.transcoding.max_concurrent_per_user,
        )
        .map_err(|stream_err| {
            ServiceReadyError::Validation(ValidationCause::StreamConfig(stream_err))
        })?;

        // --- Exactly one startup run. ---
        let initialized = initialize(preflight, now)
            .await
            .map_err(ServiceReadyError::Startup)?;

        // Capture non-secret metadata before AuthState::new consumes `initialized`.
        let data_dir = initialized.data_dir.clone();
        let cache_dir = initialized.cache_dir.clone();
        let music_root = preflight.music_root.clone();
        let worker_thread = initialized.worker_thread;
        let database = initialized.database.clone();

        // The transcoding cache from the resolved cache directory. StreamState::new
        // re-runs the same validate_config (same helper, not duplicate policy) and
        // creates cache_dir/transcoding.
        let stream = StreamState::new(
            &cache_dir,
            &preflight.config.transcoding_cache_size,
            preflight.config.transcoding.max_concurrent,
            preflight.config.transcoding.max_concurrent_per_user,
        )
        .await
        .map_err(ServiceReadyError::Stream)?;

        // The auth state, keyed by the committed winners (previous = None). This
        // consumes `initialized`; the signing secrets live only in its
        // authenticator from here on.
        let auth = AuthState::new(
            initialized,
            session_timeout_secs,
            auth_window_secs,
            auth_request_limit,
        )
        .map_err(ServiceReadyError::Auth)?;

        Ok(ServiceReady {
            auth,
            stream,
            database,
            data_dir,
            cache_dir,
            music_root,
            worker_thread,
        })
    }
}

/// Manual `Debug`: non-secret launch facts only.
///
/// `auth` and `stream` delegate to their own redacted debuggers (which withhold
/// signing keys, DSNs, and the cache path). `database` is disclosure-safe (it
/// never prints the DSN). The directory fields are printed because they are the
/// service's own resolved paths; `music_root` is included as a non-secret
/// operational fact. If a caller must avoid path disclosure entirely, use the
/// component debuggers directly.
impl std::fmt::Debug for ServiceReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceReady")
            .field("database", &self.database)
            .field("data_dir", &self.data_dir)
            .field("cache_dir", &self.cache_dir)
            .field("music_root", &self.music_root)
            .field("worker_thread", &self.worker_thread)
            // Redacted: signing keys, DSN, cache path.
            .field("auth", &self.auth)
            .field("stream", &self.stream)
            .finish()
    }
}
