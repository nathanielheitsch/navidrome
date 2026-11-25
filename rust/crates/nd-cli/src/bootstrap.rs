//! Executing BOOT-01 steps 4-6 (`docs/rust-port/subplans/boot-api.md` §BOOT-01)
//! as ONE caller-owned transaction on an already-open, already-migrated
//! [`SQLStore`](nd_db::store::SQLStore).
//!
//! Nothing here boots a process: no config load, no directory creation, no
//! database open or migration, no listener, no scanner. The caller supplies the
//! canonical music root produced by [`crate::preflight`], the prepared inputs,
//! and an initialized store.
//!
//! The order inside the transaction is the pinned one — library, then signing
//! properties, then the setup marker/dev admin — so the dev admin's
//! `UserRepo::put` admin-library assignment (`user_repository.go:150-156`) can
//! only ever see a library row that already exists in the same transaction.
//! Signing candidates and the optional dev-password hash are prepared BEFORE the
//! retry loop, so a busy retry re-runs SQL only: it never re-derives a secret and
//! never re-hashes.
//!
//! Configured-library policy is BOOT-01's own and is deliberately stricter than
//! Go's `StoreMusicFolder` (`library_repository.go:138-150`, which re-points
//! library 1): a changed or duplicated configured root is a **fatal conflict**,
//! never a silent repoint, and an initialized store missing its library row is
//! reported rather than silently repaired. An existing row with the same root is
//! verified and left byte-identical — no stats, PID, scanned-PID, epoch or
//! timestamp is rewritten.
//!
//! ## Diagnostic discipline
//!
//! Raw database errors are consumed INSIDE this module, after the store's retry
//! classification has run, and never leave it. The public boundary exposes only
//! [`BootstrapDomain`] refusals and [`StoreFailure`]: a static stage plus a
//! numeric SQLite result code. No SQLite message text, SQL text, bound parameter,
//! file path or error source chain crosses the boundary, so nothing a trigger, a
//! schema object or a driver can write ever reaches a log line.

use std::path::Path;

use chrono::{DateTime, Utc};
use nd_auth::credentials::password::{hash_password, PasswordError};
use nd_auth::initial_setup::{DEV_INITIAL_USER_NAME, INITIAL_SETUP_FLAG_KEY};
use nd_auth::setup_store::{setup_in_tx, DevAdmin};
use nd_db::execution::{execute, query_optional};
use nd_db::hydrate::row_to_library;
use nd_db::library_repo::{self, LibraryRecord};
use nd_db::property_repo::{get_sql, put_sql};
use nd_db::runtime_error::{DbOperationError, DbRuntimeError};
use nd_db::store::SQLStore;
use nd_db::user_repo::{UserError, UserRepo};
use rusqlite::Transaction;

use crate::signing_keys::{
    decrypt_signing_secret, encrypt_signing_secret, generate_signing_secret, EncryptionKey,
    SigningKeyError, SigningSecret, JWT_PUBLIC_SECRET_PROPERTY, JWT_SECRET_PROPERTY,
};

/// The configured library name (BOOT-01 step 5 recommendation).
pub const DEFAULT_LIBRARY_NAME: &str = "Music Library";

/// Grant rows are read directly: `user_library` has no repository API in nd-db.
const GRANT_QUERY: &str =
    "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id";

/// Why a bootstrap attempt refused to run or failed inside its transaction.
///
/// Every variant is a static classification plus, where useful, the property key
/// name. Nothing here carries stored ciphertext, a decrypted secret, a bound SQL
/// parameter, a file path or SQL text, so a refusal can be logged verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapDomain {
    /// Library 1 exists and points at a different root than the configured one.
    /// Go would re-point it; this port refuses, because repointing silently
    /// strands every already-scanned row under a different root.
    #[error("library 1 is already bound to a different root: refusing to repoint it")]
    LibraryPathConflict,

    /// Another library row already owns the configured root. Creating a second
    /// row for the same path is refused rather than resolved by
    /// `INSERT OR REPLACE`, which would cascade-delete the existing row's albums,
    /// media files and grants.
    #[error("another library already owns the configured root: refusing to duplicate it")]
    DuplicateLibraryPath,

    /// The setup marker is present (or users exist) while library 1 is missing.
    /// Inserting a fresh library row here would silently renumber an initialized
    /// store, so it is reported as an inconsistent store instead.
    #[error("initialized store has no library 1 row: refusing to initialize silently")]
    InitializedStoreWithoutLibrary,

    /// A stored signing property exists but cannot be used. `reason` is a static
    /// class name, never the offending value.
    #[error("stored signing property {key} is unusable: {reason}")]
    SecretCorrupt {
        /// Which property (`JWTSecret` / `JWTPublicSecret`).
        key: &'static str,
        /// Static failure class.
        reason: &'static str,
    },

    /// The property row is absent immediately after we wrote it, so the committed
    /// winner cannot be identified.
    #[error("signing property {key} is missing after this transaction wrote it")]
    SecretMissingAfterWrite {
        /// Which property.
        key: &'static str,
    },
}

/// The phase of the bootstrap sequence that failed. Static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureStage {
    /// SQLite could not start the transaction.
    Begin,
    /// A statement inside the transaction failed.
    Body,
    /// SQLite could not commit the transaction.
    Commit,
    /// The transaction's cleanup (rollback) failed on top of the original failure.
    Rollback,
    /// The connection could not be checked out of the pool.
    Pool,
}

impl std::fmt::Display for FailureStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Begin => "beginning the transaction",
            Self::Body => "running the transaction",
            Self::Commit => "committing the transaction",
            Self::Rollback => "cleaning up a failed transaction",
            Self::Pool => "acquiring a database connection",
        };
        f.write_str(name)
    }
}

/// What failed, reduced to a stable machine-readable classification. SQLite's own
/// result codes are the only detail carried, as numbers: a message string from
/// SQLite, a trigger, or a schema object is never retained or rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The transaction stayed busy and the store's retries ran out. `attempts` is
    /// the total number of transactions attempted.
    BusyExhausted {
        /// Total transactions attempted.
        attempts: u32,
    },
    /// A SQLite failure identified by its result codes, comparable against the
    /// `rusqlite::ffi::SQLITE_*` constants.
    Sqlite {
        /// Primary result code (`extended & 0xff`, per SQLite's own
        /// decomposition). Never `SQLITE_OK`.
        primary: i32,
        /// Extended result code exactly as SQLite reported it.
        extended: i32,
    },
    /// A read found no row where one was required (a store-level inconsistency,
    /// not a configuration refusal).
    MissingRow,
    /// The pool refused or lost the connection.
    Pool,
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BusyExhausted { attempts } => {
                write!(f, "database remained busy after {attempts} attempts")
            }
            Self::Sqlite { primary, extended } => {
                write!(f, "SQLite failure (code {primary}, extended {extended})")
            }
            Self::MissingRow => write!(f, "a required row was absent"),
            Self::Pool => write!(f, "database connection unavailable"),
        }
    }
}

/// A database failure at the public boundary: stage and kind only.
///
/// Deliberately NOT `#[source]`-chained to the underlying error. The raw failure
/// is consumed inside this module — after the store's retry classification has
/// inspected it — precisely so SQLite's message text cannot travel into a log,
/// an error chain or a `Debug` print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bootstrap failed during {stage}: {kind}")]
pub struct StoreFailure {
    /// Phase that failed.
    pub stage: FailureStage,
    /// Classification, without any upstream text.
    pub kind: FailureKind,
}

/// Any bootstrap failure.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The configured music root is a refusal of the caller's own state, not a
    /// database failure.
    #[error(transparent)]
    Domain(#[from] BootstrapDomain),

    /// The music root is relative. [`crate::preflight`] canonicalizes it, so a
    /// relative value is a caller bug. The path is not echoed: it is unvalidated
    /// caller data.
    #[error("music root is relative: an absolute canonical path is required")]
    RelativeMusicRoot,

    /// The music root cannot be rendered as UTF-8, so it cannot be stored.
    /// Rejected before any preparation or SQL, and the bytes are not echoed.
    #[error("music root is not valid UTF-8 and cannot be stored")]
    NonUtf8MusicRoot,

    /// Argon2id hashing of the optional dev password failed. Static message.
    #[error(transparent)]
    Password(#[from] PasswordError),

    /// Fresh signing material could not be generated. Static message.
    #[error(transparent)]
    Entropy(#[from] SigningKeyError),

    /// The transaction refused or failed; everything it wrote rolled back.
    #[error(transparent)]
    Store(#[from] StoreFailure),
}

impl BootstrapError {
    /// The domain refusal, if this error is one. Lets a caller tell "conflicting
    /// configuration" from "database error" without string matching.
    #[must_use]
    pub fn domain(&self) -> Option<&BootstrapDomain> {
        match self {
            Self::Domain(domain) => Some(domain),
            _ => None,
        }
    }
}

/// Prepared inputs to one bootstrap attempt.
#[derive(Clone)]
pub struct BootstrapInput<'a> {
    /// Canonical configured music root, from
    /// [`crate::preflight::PreflightOutcome::music_root`]. Never created here, and
    /// stored only after a checked UTF-8 conversion.
    pub music_root: &'a Path,
    /// `PasswordEncryptionKey` for the signing-property envelope
    /// ([`crate::signing_keys`]). Empty selects the documented public default,
    /// which is what makes existing Go-generated properties decryptable.
    pub password_encryption_key: &'a str,
    /// Optional dev-admin password. `None` or empty sets the marker and creates no
    /// user, matching `server/initial_setup.go:26-27`.
    pub dev_password: Option<&'a str>,
    /// Caller-owned clock. Library `created_at`/`updated_at` and the setup marker
    /// share this single stamp.
    pub now: DateTime<Utc>,
}

impl std::fmt::Debug for BootstrapInput<'_> {
    /// Presence-only for the password and no `password_encryption_key` field at
    /// all: both are credentials/key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapInput")
            .field("music_root", &self.music_root)
            .field("dev_password", &self.dev_password.is_some())
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

/// Committed state produced by one bootstrap.
///
/// Every field was read from the borrowed transaction that wrote it, and the
/// value is only handed out once that transaction has COMMITTED: an aborted
/// attempt publishes nothing. Signing secrets are held as [`SigningSecret`],
/// whose `Debug` never leaks the value; the caller recovers them with `expose()`.
pub struct BootstrapOutcome {
    /// The library row as committed, read inside the transaction.
    pub library: LibraryRecord,
    /// `true` when this attempt inserted the library row rather than verifying
    /// an existing one.
    pub library_created: bool,
    /// Winning value for `JWTSecret`, decrypted from the row this transaction
    /// committed.
    pub session_secret: SigningSecret,
    /// Winning value for `JWTPublicSecret`, likewise.
    pub public_secret: SigningSecret,
    /// `true` when this attempt inserted the `JWTSecret` row.
    pub session_secret_created: bool,
    /// `true` when this attempt inserted the `JWTPublicSecret` row.
    pub public_secret_created: bool,
    /// `true` when this attempt inserted the setup marker (rather than finding it
    /// already present).
    pub setup_marker_written: bool,
    /// `true` only when this attempt INSERTED the dev admin user. Finding an
    /// existing admin — including with the marker missing — reports `false`.
    pub dev_admin_created: bool,
    /// `(user_id, library_id)` grant rows as committed, read inside the
    /// transaction.
    pub grants: Vec<(String, i64)>,
}

impl std::fmt::Debug for BootstrapOutcome {
    /// Deliberately minimal: neither signing secret is printed, and this type
    /// holds no SQL text or bound parameter.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapOutcome")
            .field("library_id", &self.library.id)
            .field("library_created", &self.library_created)
            .field("session_secret_created", &self.session_secret_created)
            .field("public_secret_created", &self.public_secret_created)
            .field("setup_marker_written", &self.setup_marker_written)
            .field("dev_admin_created", &self.dev_admin_created)
            .field("grants", &self.grants.len())
            .finish_non_exhaustive()
    }
}

/// Runs BOOT-01 steps 4-6 against an already-open, already-migrated store.
///
/// The whole sequence is ONE retryable transaction on a single connection lease
/// (`persistence.go:207-228` `WithTxRetry`), so the library row, both signing
/// properties, the setup marker, the dev admin and the admin's library grants
/// commit or roll back together. Only SQLite busy failures are retried; every
/// other failure — a domain refusal, a corrupt stored secret, any SQLite error —
/// is fatal and commits nothing.
///
/// Order inside the transaction:
/// 1. [`initialize_library`] — initialize or verify
///    [`DEFAULT_LIBRARY_ID`](library_repo::DEFAULT_LIBRARY_ID);
/// 2. [`secure_property`] for each key — read, or missing-only insert, then
///    decrypt the value this transaction holds;
/// 3. [`setup_in_tx`] — marker and dev admin, after the library exists;
/// 4. final reads of the library row, both ciphertexts and the grant rows ON THE
///    SAME borrowed transaction, from which the outcome is built.
///
/// The outcome leaves the retry loop only through the store's own commit path,
/// so nothing is published for a rolled-back attempt and no second checkout,
/// decrypt or other fallible work happens after the commit.
///
/// # Errors
/// [`BootstrapError::RelativeMusicRoot`] / [`BootstrapError::NonUtf8MusicRoot`]
/// for a root that cannot be stored (rejected before any preparation or SQL);
/// [`BootstrapError::Entropy`] or [`BootstrapError::Password`] when preparation
/// fails; [`BootstrapError::Domain`] for a refusal; [`BootstrapError::Store`]
/// for a database failure, carrying only stage and result code. A malformed or
/// empty stored secret is fatal by design: it is corruption, not a missing
/// value, and is never rotated or rewritten.
pub fn bootstrap(
    store: &SQLStore,
    input: &BootstrapInput<'_>,
) -> Result<BootstrapOutcome, BootstrapError> {
    // Rejected BEFORE preparation and before any SQL: the root is only ever
    // stored as checked UTF-8. Neither error echoes the path.
    if !input.music_root.is_absolute() {
        return Err(BootstrapError::RelativeMusicRoot);
    }
    let root = input
        .music_root
        .to_str()
        .ok_or(BootstrapError::NonUtf8MusicRoot)?;

    let key = EncryptionKey::from_password_encryption_key(input.password_encryption_key);

    // Step 4, OUTSIDE the retry loop: two independent fresh candidates, each
    // 256 random bits, encrypted to the Go wire format. Neither is published
    // unless it wins a missing-only insert, and a busy retry never re-draws.
    let session_candidate_secret = generate_signing_secret()?;
    let session_candidate = encrypt_signing_secret(&key, session_candidate_secret.expose())?;
    let public_candidate_secret = generate_signing_secret()?;
    let public_candidate = encrypt_signing_secret(&key, public_candidate_secret.expose())?;

    // Step 4, OUTSIDE the retry loop: the optional dev-password hash, once.
    let dev_hash = match input.dev_password {
        Some(password) if !password.is_empty() => Some(hash_password(password)?),
        _ => None,
    };
    let stamp = nd_db::time::to_text(&input.now);

    let outcome = store
        .try_with_tx_retry_sync(
            |tx| -> Result<BootstrapOutcome, DbOperationError<BootstrapDomain>> {
                let library_created = initialize_library(tx, root, &input.now)?;
                let (session_secret, session_created) =
                    secure_property(tx, JWT_SECRET_PROPERTY, &session_candidate, &key)?;
                let (public_secret, public_created) =
                    secure_property(tx, JWT_PUBLIC_SECRET_PROPERTY, &public_candidate, &key)?;
                // Marker/dev admin last, so the admin's library grant only ever sees
                // a library row that already exists in this transaction.
                let (setup_marker_written, dev_admin_created) =
                    run_setup(tx, dev_hash.as_deref(), &stamp)?;

                // Final reads on the SAME borrowed transaction: whatever is returned
                // here is exactly what this transaction commits, and it is dropped
                // unchanged if the commit does not happen.
                let library = committed_library(tx)?;
                let grants = grant_rows(tx).map_err(DbOperationError::sqlite)?;

                Ok(BootstrapOutcome {
                    library,
                    library_created,
                    session_secret,
                    public_secret,
                    session_secret_created: session_created,
                    public_secret_created: public_created,
                    setup_marker_written,
                    dev_admin_created,
                    grants,
                })
            },
        )
        .map_err(summarize)?;

    // Reached only after the store committed the transaction.
    Ok(outcome)
}

/// Initialize or verify the configured library row, then associate admins with
/// the library rows that exist.
///
/// * Missing: insert exactly one row — id 1, configured root,
///   `default_new_users = true`, empty PID overrides and scanned-PIDs, zero
///   stats, `full_scan_in_progress = false`, both stamps = `now`, and the
///   never-scanned stamps for `last_scan*`.
/// * Present with the same root: verify only. Nothing is written, so no
///   timestamp, stat or PID is rewritten.
/// * Present with a different root, or the root owned by another id: refusal.
/// * Setup marker present or users exist with no library row: refusal.
///
/// The admin association is the repository's idempotent
/// `ON CONFLICT DO NOTHING` leg (`library_repository.go:117-128`); with no
/// library row it grants nothing, so an absent library cannot produce a grant.
///
/// Returns whether this attempt inserted the row.
fn initialize_library(
    tx: &Transaction<'_>,
    root: &str,
    now: &DateTime<Utc>,
) -> Result<bool, DbOperationError<BootstrapDomain>> {
    let existing = query_optional(
        tx,
        &library_repo::get_sql(library_repo::DEFAULT_LIBRARY_ID),
        row_to_library,
    )
    .map_err(DbOperationError::sqlite)?;

    // Duplicate-path check BEFORE anything is written, so an
    // `INSERT OR REPLACE` cascade is structurally impossible.
    if library_rows(tx)
        .map_err(DbOperationError::sqlite)?
        .iter()
        .any(|record| record.path == root && record.id != library_repo::DEFAULT_LIBRARY_ID)
    {
        return Err(DbOperationError::Domain(
            BootstrapDomain::DuplicateLibraryPath,
        ));
    }

    match existing {
        None => {
            // An initialized store with no library row is not a fresh store.
            let marker = query_optional(tx, &get_sql(INITIAL_SETUP_FLAG_KEY), |row| {
                row.get::<_, String>(0)
            })
            .map_err(DbOperationError::sqlite)?;
            let users = UserRepo::new(tx)
                .count_all()
                .map_err(|error| DbOperationError::sqlite(user_sqlite_error(error)))?;
            if marker.is_some() || users > 0 {
                return Err(DbOperationError::Domain(
                    BootstrapDomain::InitializedStoreWithoutLibrary,
                ));
            }
            let record = fresh_library(root, now);
            // `put_in_tx` on an absent id-1 row runs its INSERT leg and then the
            // admin assignment; its path guard is a no-op when nothing is stored.
            let result =
                library_repo::put_in_tx(tx, &record, now, &[]).map_err(|error| match error {
                    DbOperationError::Domain(domain) => {
                        DbOperationError::Domain(library_domain(domain))
                    }
                    DbOperationError::Sqlite(source) => DbOperationError::sqlite(source),
                })?;
            Ok(result.inserted)
        }
        Some(current) => {
            if current.path != root {
                return Err(DbOperationError::Domain(
                    BootstrapDomain::LibraryPathConflict,
                ));
            }
            // Idempotent restart: verify, write nothing.
            Ok(false)
        }
    }
}

/// Read or missing-only-insert one signing property and return the winning
/// secret this transaction holds for it, plus whether this attempt inserted the
/// row.
///
/// An existing value is preserved byte-for-byte. A missing one receives the
/// candidate through the plain `INSERT` arm of [`put_sql`] — never the update
/// arm, so a racing committed writer is never overwritten. A unique/primary-key
/// constraint violation on that insert means another writer committed first: the
/// candidate is discarded and the committed row becomes the winner. An empty or
/// malformed stored value is fatal corruption, never a rotation.
fn secure_property(
    tx: &Transaction<'_>,
    key_name: &'static str,
    candidate: &str,
    key: &EncryptionKey,
) -> Result<(SigningSecret, bool), DbOperationError<BootstrapDomain>> {
    let corrupt = |error: SigningKeyError| {
        DbOperationError::Domain(BootstrapDomain::SecretCorrupt {
            key: key_name,
            reason: corruption_class(error),
        })
    };

    if let Some(stored) = read_optional(tx, key_name).map_err(DbOperationError::sqlite)? {
        // Preserve: decrypt the stored value — it is the winner.
        let secret = decrypt_signing_secret(key, &stored).map_err(corrupt)?;
        return Ok((secret, false));
    }

    let statement = put_sql(key_name, candidate);
    let created = match execute(tx, &statement.insert) {
        Ok(_) => true,
        Err(source) => {
            // Only a UNIQUE or PRIMARY-KEY violation means "committed first".
            // Everything else — a trigger-raised constraint, NOT NULL, CHECK,
            // busy, locked — is fatal and keeps its result code.
            if !is_conflict_ownership(&source) {
                return Err(DbOperationError::sqlite(source));
            }
            false
        }
    };
    let stored = read_optional(tx, key_name)
        .map_err(DbOperationError::sqlite)?
        .ok_or(DbOperationError::Domain(
            BootstrapDomain::SecretMissingAfterWrite { key: key_name },
        ))?;
    let secret = decrypt_signing_secret(key, &stored).map_err(corrupt)?;
    Ok((secret, created))
}

/// Marker / dev-admin step, always run after the library step.
///
/// `dev_admin_created` reports an actual INSERT only: `setup_in_tx` skips the
/// user when the marker is already set or when `admin` already exists, and both
/// cases are detected here rather than inferred from the supplied password.
fn run_setup(
    tx: &Transaction<'_>,
    dev_hash: Option<&str>,
    stamp: &str,
) -> Result<(bool, bool), DbOperationError<BootstrapDomain>> {
    let admin_exists = UserRepo::new(tx)
        .count_by_username(DEV_INITIAL_USER_NAME)
        .map_err(|error| DbOperationError::sqlite(user_sqlite_error(error)))?
        > 0;
    let dev_admin = dev_hash.map(|password_hash| DevAdmin { password_hash });
    let marker_written =
        setup_in_tx(tx, dev_admin.as_ref(), stamp).map_err(DbOperationError::sqlite)?;
    Ok((
        marker_written,
        dev_admin.is_some() && !admin_exists && marker_written,
    ))
}

/// The library row as committed inside this transaction.
///
/// [`initialize_library`] already guarantees the row in this same transaction, so
/// a missing row here is a store-level inconsistency, not a configuration
/// refusal: it surfaces as [`FailureKind::MissingRow`].
fn committed_library(
    tx: &Transaction<'_>,
) -> Result<LibraryRecord, DbOperationError<BootstrapDomain>> {
    query_optional(
        tx,
        &library_repo::get_sql(library_repo::DEFAULT_LIBRARY_ID),
        row_to_library,
    )
    .map_err(DbOperationError::sqlite)?
    .ok_or(DbOperationError::sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

/// The library row a fresh store gets: explicit BOOT-01 policy values, not the
/// DDL defaults (`default_new_users` defaults false, so it is stated here).
fn fresh_library(root: &str, now: &DateTime<Utc>) -> LibraryRecord {
    LibraryRecord {
        id: library_repo::DEFAULT_LIBRARY_ID,
        name: DEFAULT_LIBRARY_NAME.to_owned(),
        path: root.to_owned(),
        remote_path: String::new(),
        // Never scanned. `library_repo::insert_pairs` binds these through
        // `nd_db::time::value`, so the persisted TEXT is Go's zero time in the
        // port's canonical SQLite form — `0001-01-01 00:00:00+00:00` — which is
        // what `library_repo::zero_time_value` and `ScanEnd` also write, and what
        // `hydrate::row_to_library` reports as "no scan yet". The DDL column
        // DEFAULT `'0000-00-00 00:00:00'` is NOT used: Go's `Put` always binds
        // the model's zero `time.Time`, and this row is always inserted with an
        // explicit value. See the plan discrepancy noted in the module report.
        last_scan_at: nd_db::time::zero(),
        last_scan_started_at: nd_db::time::zero(),
        full_scan_in_progress: false,
        updated_at: *now,
        created_at: *now,
        total_songs: 0,
        total_albums: 0,
        total_artists: 0,
        total_folders: 0,
        total_files: 0,
        total_missing_files: 0,
        total_size: 0,
        total_duration: 0.0,
        default_new_users: true,
        pid_album: String::new(),
        pid_track: String::new(),
        scanned_pid_album: String::new(),
        scanned_pid_track: String::new(),
    }
}

fn library_rows(tx: &Transaction<'_>) -> Result<Vec<LibraryRecord>, rusqlite::Error> {
    let statement = library_repo::get_all_sql(&[], 0, 0);
    let mut prepared = tx.prepare(&statement.0)?;
    let rows = prepared
        .query_map([], row_to_library)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn read_optional(tx: &Transaction<'_>, key_name: &str) -> Result<Option<String>, rusqlite::Error> {
    query_optional(tx, &get_sql(key_name), |row| row.get::<_, String>(0))
}

fn grant_rows(tx: &Transaction<'_>) -> Result<Vec<(String, i64)>, rusqlite::Error> {
    let mut prepared = tx.prepare(GRANT_QUERY)?;
    let rows = prepared
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Map a `SigningKeyError` to a static corruption class. The error is not
/// carried: a refusal naming the key plus a class is all a log line needs.
fn corruption_class(error: SigningKeyError) -> &'static str {
    match error {
        SigningKeyError::EmptyStoredValue => "empty",
        SigningKeyError::MalformedCiphertext => "not decodable ciphertext",
        SigningKeyError::WrongKeyOrTag => "authentication tag mismatch",
        SigningKeyError::EmptyPlaintext => "empty plaintext",
        SigningKeyError::TooShort { .. } => "shorter than a Go-generated id",
        SigningKeyError::InvalidCharacters => "invalid characters",
        SigningKeyError::SealingFailed => "could not be sealed",
        SigningKeyError::EntropyUnavailable => "entropy source unavailable",
    }
}

/// Reduce a store/transaction failure for the caller.
///
/// A `Domain` refusal is handed back intact — it is a typed, self-authored
/// classification. Every other arm is reduced to [`StoreFailure`]: the raw
/// errors are matched and dropped here, so SQLite's message text never reaches
/// a rendered message, a `Debug` print or an error source chain.
fn summarize(failure: DbRuntimeError<BootstrapDomain>) -> BootstrapError {
    match failure {
        DbRuntimeError::Domain(domain) => BootstrapError::Domain(domain),
        DbRuntimeError::Sqlite { stage, source } => BootstrapError::Store(StoreFailure {
            stage: map_stage(stage),
            kind: sqlite_kind(&source),
        }),
        DbRuntimeError::BusyExhausted {
            attempts, stage, ..
        } => BootstrapError::Store(StoreFailure {
            stage: map_stage(stage),
            kind: FailureKind::BusyExhausted { attempts },
        }),
        // Rollback failed on top of an original failure: the caller learns the
        // cleanup failed (stage) and what the original was (kind), with no text.
        DbRuntimeError::RollbackFailed { original, .. } => BootstrapError::Store(StoreFailure {
            stage: FailureStage::Rollback,
            kind: match *original {
                DbRuntimeError::Sqlite { source, .. } => sqlite_kind(&source),
                DbRuntimeError::BusyExhausted { attempts, .. } => {
                    FailureKind::BusyExhausted { attempts }
                }
                DbRuntimeError::Pool(_) | DbRuntimeError::RollbackFailed { .. } => {
                    FailureKind::Pool
                }
                DbRuntimeError::Domain(_) => FailureKind::MissingRow,
            },
        }),
        DbRuntimeError::Pool(_) => BootstrapError::Store(StoreFailure {
            stage: FailureStage::Pool,
            kind: FailureKind::Pool,
        }),
    }
}

fn map_stage(stage: nd_db::runtime_error::TransactionStage) -> FailureStage {
    use nd_db::runtime_error::TransactionStage;
    match stage {
        TransactionStage::Begin => FailureStage::Begin,
        TransactionStage::Body => FailureStage::Body,
        TransactionStage::Commit => FailureStage::Commit,
    }
}

/// Result codes only. `ErrorCodes`/`IsBusy` (`db.go:128-140`) key on the primary
/// code, and the extended code is retained for callers that need the subtype
/// (e.g. `SQLITE_CONSTRAINT_UNIQUE` vs `SQLITE_CONSTRAINT_TRIGGER`).
fn sqlite_kind(source: &rusqlite::Error) -> FailureKind {
    if matches!(source, rusqlite::Error::QueryReturnedNoRows) {
        return FailureKind::MissingRow;
    }
    let Some(failure) = source.sqlite_error() else {
        return FailureKind::MissingRow;
    };
    // `extended_code` is the C result code SQLite reported. SQLite defines the
    // primary code as its low byte, so both numbers are derived from that one
    // authoritative value rather than from rusqlite's `ErrorCode` enum, whose
    // discriminants are NOT the C constants.
    let extended = failure.extended_code;
    FailureKind::Sqlite {
        primary: extended & 0xff,
        extended,
    }
}

/// Only a UNIQUE or PRIMARY-KEY constraint violation means "another writer
/// committed this row first". Extended result codes are compared in full, so a
/// trigger-raised `SQLITE_CONSTRAINT_TRIGGER` or a `NOT NULL`/`CHECK` violation
/// is never mistaken for a race.
fn is_conflict_ownership(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    )
}

/// The user-count read's only non-SQLite arm is `NotFound`, which cannot happen
/// for an aggregate; map it to the neutral no-rows error rather than inventing a
/// domain variant.
fn user_sqlite_error(error: UserError) -> rusqlite::Error {
    match error {
        UserError::Sqlite(source) => source,
        UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
    }
}

/// Map a `LibraryWriteError` refusal into this module's domain. The stored and
/// requested paths are deliberately not carried: naming them in a refusal
/// invites log disclosure of configured paths.
fn library_domain(error: library_repo::LibraryWriteError) -> BootstrapDomain {
    match error {
        library_repo::LibraryWriteError::PathImmutable { .. } => {
            BootstrapDomain::LibraryPathConflict
        }
    }
}
