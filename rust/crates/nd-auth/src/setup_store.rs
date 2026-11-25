//! Executing initial-admin setup against the SQL store.
//!
//! This wires the property/user portion of `server/initial_setup.go:19-73`;
//! the caller remains responsible for storing the music folder. The marker,
//! first-user check and admin insert share a retryable transaction. Unlike
//! Go, the dev password is hashed with argon2id before `UserRepo::put` — and
//! that hashing happens OUTSIDE the transaction, so a busy retry can never
//! re-run Argon2. The in-transaction work is the narrow synchronous
//! [`setup_in_tx`], which takes the caller's borrowed transaction and already
//! prepared dev-admin material.
//!
//! There is deliberately no pre-transaction "already set up?" read: an
//! advisory read would sit outside the store's busy-retry policy. Rust prepares
//! a configured dev password's Argon2id hash once per call, even on restart,
//! to keep hashing outside transaction retries. Go instead checks its marker
//! before creating the dev admin (`server/initial_setup.go:25-33`).

use nd_db::execution::{execute, query_optional};
use nd_db::property_repo::{get_sql, put_sql};
use nd_db::store::{SQLStore, StoreError};
use nd_db::user_repo::{UserError, UserPutValues, UserRepo};

use crate::credentials::password::{hash_password, PasswordError};
use crate::initial_setup::{DEV_INITIAL_NAME, DEV_INITIAL_USER_NAME, INITIAL_SETUP_FLAG_KEY};

/// Dev-admin material, prepared by the caller BEFORE the transaction starts.
///
/// The transaction helper takes a prepared hash, never a cleartext password.
/// Without dev-admin material, it creates no user.
#[derive(Clone)]
pub struct DevAdmin<'a> {
    /// Argon2id PHC string, produced before the transaction begins.
    pub password_hash: &'a str,
}

/// Initial setup storage or password-hashing error (`server/initial_setup.go:19-73`).
#[derive(Debug, thiserror::Error)]
pub enum SetupStoreError {
    /// Store lease, transaction or SQL failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Argon2id hashing failed.
    #[error(transparent)]
    Password(#[from] PasswordError),
}

fn sql_error(error: UserError) -> rusqlite::Error {
    match error {
        UserError::Sqlite(error) => error,
        UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
    }
}

/// The narrow synchronous setup body: one-shot marker check, optional dev-admin
/// insert, marker write — all on the caller's **borrowed** transaction.
///
/// It owns no connection lifecycle: no checkout, no `BEGIN`, no commit, no
/// rollback, and no hashing. The caller's transaction decides whether the
/// writes survive, which is what makes this body usable both inside
/// `with_tx_retry` and inside a caller-owned transaction.
///
/// Semantics preserved from `server/initial_setup.go:19-73`:
/// * an existing marker returns `false` and writes nothing (`:21-25`);
/// * `None` dev-admin inserts no user at all (`:26-27` no-password arm);
/// * the insert is skipped when `admin` already exists (`:28-31`), but the
///   marker is still written;
/// * the marker is an upsert (`:41-42`) written last, so a failure can never
///   mark setup complete without its user row;
/// * the grant leg is `UserRepo::put`'s own admin assignment
///   (`user_repository.go:150-156`), which runs on this same transaction and so
///   rolls back with the user row. When the insert is skipped the grant leg is
///   skipped too, leaving existing grants untouched.
///
/// Unlike Go's `Put` path for `NewPassword`, Rust accepts only the prepared
/// Argon2id hash here; transaction retries do not repeat the password hash.
///
/// # Errors
/// Any SQLite failure from the reads or writes, returned unchanged so the
/// caller's retry policy sees the raw classification. This module adds nothing
/// to those messages: it never embeds the password, the prepared hash or a
/// stored value. What SQLite itself reports (constraint names, SQL text) is
/// SQLite's own diagnostic and is passed through verbatim.
pub fn setup_in_tx(
    tx: &rusqlite::Transaction<'_>,
    dev_admin: Option<&DevAdmin<'_>>,
    completed_at: &str,
) -> Result<bool, rusqlite::Error> {
    if query_optional(tx, &get_sql(INITIAL_SETUP_FLAG_KEY), |row| {
        row.get::<_, String>(0)
    })?
    .is_some()
    {
        return Ok(false);
    }
    let repo = UserRepo::new(tx);
    if let Some(admin) = dev_admin {
        if repo
            .count_by_username(DEV_INITIAL_USER_NAME)
            .map_err(sql_error)?
            == 0
        {
            let values = UserPutValues {
                id: "",
                user_name: DEV_INITIAL_USER_NAME,
                name: DEV_INITIAL_NAME,
                email: "",
                password: Some(admin.password_hash),
                is_admin: true,
                last_login_at: None,
                last_access_at: None,
                created_at: completed_at,
                updated_at: completed_at,
                scrobble_filter: "",
            };
            repo.put(&values, completed_at, false).map_err(sql_error)?;
        }
    }
    let statement = put_sql(INITIAL_SETUP_FLAG_KEY, completed_at);
    if execute(tx, &statement.update)? == 0 {
        execute(tx, &statement.insert)?;
    }
    Ok(true)
}

/// One attempt body, shared by the adapter below and by the module's own
/// tests: it borrows the already-prepared hash, so re-running it for a busy
/// retry performs SQL only.
fn setup_attempt(
    hash: &Option<String>,
    completed_at: &str,
    tx: &rusqlite::Transaction<'_>,
) -> Result<bool, rusqlite::Error> {
    let admin = hash.as_ref().map(|hash| DevAdmin {
        password_hash: hash.as_str(),
    });
    setup_in_tx(tx, admin.as_ref(), completed_at)
}

/// Password-hashing seam. **Private**: production always supplies the real
/// argon2id implementation, so there is no public testing hook, while the
/// module's own tests can inject a counting or failing hasher and still drive
/// the real adapter path. `Arc` because the store runs hashing on a blocking
/// task, so the seam must be `'static` and shareable rather than a borrow.
type Hasher = std::sync::Arc<dyn Fn(&str) -> Result<String, PasswordError> + Send + Sync>;

/// Hashes once, before any transaction. An empty password is the no-user arm
/// and hashes nothing (`server/initial_setup.go:26-27`).
async fn prepare_hash(
    store: &SQLStore,
    dev_password: &str,
    hasher: Hasher,
) -> Result<Option<String>, SetupStoreError> {
    if dev_password.is_empty() {
        return Ok(None);
    }
    let cleartext = dev_password.to_owned();
    // The hashing outcome rides inside the store result, so a hashing failure
    // stays a `Password` error instead of being flattened into a store error.
    Ok(Some(
        store
            .with_connection(move |_| Ok(hasher(&cleartext)))
            .await??,
    ))
}

/// The adapter over [`prepare_hash`] + [`setup_attempt`], taking the hasher as
/// a parameter. Private for the same reason as [`Hasher`]: external callers
/// only ever get [`run_initial_setup`], which supplies real argon2id.
async fn run_initial_setup_hashed(
    store: &SQLStore,
    dev_password: &str,
    completed_at: &str,
    hasher: Hasher,
) -> Result<bool, SetupStoreError> {
    let hash = prepare_hash(store, dev_password, hasher).await?;
    let completed_at = completed_at.to_owned();
    Ok(store
        .with_tx_retry(move |tx| setup_attempt(&hash, &completed_at, tx))
        .await?)
}

/// Runs the property and dev-admin setup in one retryable store transaction
/// (`server/initial_setup.go:19-73`). Returns false if already completed.
/// `completed_at` is supplied by the caller and is also used for timestamps.
///
/// The password is hashed exactly once, before the transaction, and
/// [`setup_in_tx`] takes the finished hash — so busy retries re-run SQL only
/// and never re-hash. A restart of an already-initialized store therefore still
/// pays one argon2id hash before the in-transaction marker check short-circuits;
/// see the module doc for why there is deliberately no cheaper pre-read.
///
/// # Errors
/// Returns a store or argon2id error. A failed transaction commits no marker or
/// admin user, and a hashing failure writes nothing at all. Busy-retry
/// exhaustion stays distinguishable as [`StoreError::BusyRetriesExhausted`]
/// inside [`SetupStoreError::Store`] (`persistence.go:207-228`); every other
/// store failure and any hashing failure is not retryable and surfaces
/// unchanged.
pub async fn run_initial_setup(
    store: &SQLStore,
    dev_password: &str,
    completed_at: &str,
) -> Result<bool, SetupStoreError> {
    run_initial_setup_hashed(
        store,
        dev_password,
        completed_at,
        std::sync::Arc::new(hash_password),
    )
    .await
}

#[cfg(test)]
mod hashing_tests {
    //! Deterministic coverage of the hashing placement, driven through the real
    //! adapter path with a private injected hasher. No global test state and no
    //! public hook: `Hasher`/`run_initial_setup_hashed` are crate-internal.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use nd_db::pool::{Pool, PoolConfig};
    use nd_db::store::{is_busy, with_tx_retry, StoreError};
    use nd_dbschema::schema::apply_fresh_schema;

    use super::*;

    fn setup() -> (SQLStore, std::path::PathBuf) {
        let path =
            std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-setup-", ".db"));
        let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
        let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
        apply_fresh_schema(&first).unwrap();
        pool.put_back(first).expect("return configured handle");
        (SQLStore::new(pool), path)
    }

    fn raw_conn() -> (rusqlite::Connection, std::path::PathBuf) {
        let path =
            std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-setup-raw-", ".db"));
        let conn = nd_dbschema::schema::open(&format!(
            "file:{}?mode=rwc&_foreign_keys=on",
            path.display()
        ))
        .expect("open synthetic db");
        apply_fresh_schema(&conn).expect("fresh schema");
        (conn, path)
    }

    /// `(user count, setup marker)` read straight from the synthetic DB.
    fn state(store: &SQLStore) -> (i64, Option<String>) {
        store
            .with_connection_sync(|conn| {
                Ok((
                    conn.query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))?,
                    query_optional(conn, &get_sql(INITIAL_SETUP_FLAG_KEY), |row| {
                        row.get::<_, String>(0)
                    })?,
                ))
            })
            .expect("read synthetic state")
    }

    /// A counting hasher for the private seam: returns the counter and a
    /// [`Hasher`] that records every actual hash call. It never fails or panics,
    /// so a re-hash inside the retry closure shows up as a count rather than as
    /// a flake.
    fn counting_hasher() -> (std::sync::Arc<AtomicUsize>, Hasher) {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let shared = std::sync::Arc::clone(&calls);
        let hasher: Hasher = std::sync::Arc::new(move |password: &str| {
            shared.fetch_add(1, Ordering::SeqCst);
            Ok(format!("hash-of-{password}"))
        });
        (calls, hasher)
    }

    #[tokio::test]
    async fn hashing_runs_once_per_call_and_never_inside_the_retry_closure() {
        let (store, path) = setup();
        let (counter, hasher) = counting_hasher();

        let wrote = run_initial_setup_hashed(
            &store,
            "dev-password",
            "2024-01-01T00:00:00Z",
            std::sync::Arc::clone(&hasher),
        )
        .await
        .expect("setup succeeds");
        assert!(wrote);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "exactly one hash for a fresh setup"
        );
        let (users, marker) = state(&store);
        assert_eq!(users, 1);
        assert_eq!(marker.as_deref(), Some("2024-01-01T00:00:00Z"));

        // Restart of an initialized store: the marker check inside the
        // transaction short-circuits, so nothing is written. Hashing still ran
        // once — the documented tradeoff of having no advisory pre-read.
        let again = run_initial_setup_hashed(
            &store,
            "dev-password",
            "2024-01-02T00:00:00Z",
            std::sync::Arc::clone(&hasher),
        )
        .await
        .expect("restart does not error");
        assert!(!again, "the in-transaction marker check must short-circuit");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "one hash per call, no more"
        );
        let (users, marker) = state(&store);
        assert_eq!(users, 1, "a restart writes no user");
        assert_eq!(
            marker.as_deref(),
            Some("2024-01-01T00:00:00Z"),
            "marker value is not overwritten"
        );

        // Empty password is the no-user arm: hashing is skipped entirely.
        let (store2, path2) = setup();
        let (counter2, hasher2) = counting_hasher();
        assert!(run_initial_setup_hashed(
            &store2,
            "",
            "2024-01-01T00:00:00Z",
            std::sync::Arc::clone(&hasher2),
        )
        .await
        .expect("marker-only setup succeeds"));
        assert_eq!(
            counter2.load(Ordering::SeqCst),
            0,
            "no password means no hashing"
        );
        assert_eq!(state(&store2).0, 0);
        assert_eq!(state(&store2).1.as_deref(), Some("2024-01-01T00:00:00Z"));

        drop(store);
        drop(store2);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path2);
    }

    #[tokio::test]
    async fn a_failing_hasher_writes_nothing_and_reports_the_password_error() {
        let (store, path) = setup();
        let failing: Hasher = std::sync::Arc::new(|_| Err(PasswordError::HashFailed));
        let error =
            run_initial_setup_hashed(&store, "dev-password", "2024-01-01T00:00:00Z", failing)
                .await
                .expect_err("a hashing failure must not be swallowed");
        assert!(matches!(error, SetupStoreError::Password(_)));
        assert_eq!(state(&store), (0, None), "a hashing failure writes nothing");
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_busy_retry_re_runs_the_sql_body_without_re_hashing() {
        // Deterministic retry: the store's own retry loop with a zero delay, on
        // a raw connection, driving the same `setup_attempt` the adapter uses.
        // Attempt 1 writes then fails with a busy-class error (so it rolls
        // back); attempt 2 succeeds. The hasher count proves re-hashing did not
        // happen, and the attempt counter proves the SQL body did re-run.
        let (mut conn, path) = raw_conn();
        // The seam is called exactly the way `prepare_hash` calls it — once,
        // before the transaction — and its output is what the body must store.
        let (counter, hasher) = counting_hasher();
        let prepared = hasher("dev-password").expect("synthetic hash");
        assert_eq!(counter.load(Ordering::SeqCst), 1, "prepared once, up front");
        let attempts = AtomicUsize::new(0);
        let hash: Option<String> = Some(prepared.clone());
        let stamp = "2024-01-01T00:00:00Z";

        let outcome = with_tx_retry(
            &mut conn,
            &|_| Duration::from_secs(0),
            |tx| -> Result<bool, rusqlite::Error> {
                let attempt = attempts.load(Ordering::SeqCst);
                attempts.store(attempt + 1, Ordering::SeqCst);
                let wrote = setup_attempt(&hash, stamp, tx)?;
                if attempt == 0 {
                    // Roll back attempt 1 with a busy-class failure, exactly the
                    // class the store retries (`persistence.go:218`).
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        Some("synthetic busy failure".to_owned()),
                    ));
                }
                Ok(wrote)
            },
        )
        .expect("a busy failure is retried and the second attempt commits");

        assert!(outcome);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the SQL body ran once per attempt"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "two attempts ran, but the seam was called only for preparation — never in the retry body"
        );
        let users = conn
            .query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(users, 1, "the retried attempt committed its user row");
        assert_eq!(
            conn.query_row(
                "SELECT password FROM user WHERE user_name = ?",
                [DEV_INITIAL_USER_NAME],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            prepared.as_str(),
            "the hash prepared before the transaction is what got stored"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn busy_exhaustion_and_not_busy_failures_stay_distinguishable_through_the_store() {
        // The API relies on the store's classification, so assert that contract
        // directly (extended busy codes included) and that an exhausted retry
        // leaves nothing behind while a not-busy failure surfaces immediately.
        assert!(is_busy(&rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        )));
        assert!(is_busy(&rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
            None,
        )));
        assert!(!is_busy(&rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        )));

        let zero = &|_| Duration::from_secs(0);

        // Busy: every attempt rolls back, retries are exhausted into a
        // distinguishable error, and nothing is committed.
        let (mut busy_conn, busy_path) = raw_conn();
        let attempts = AtomicUsize::new(0);
        let error = with_tx_retry(
            &mut busy_conn,
            zero,
            |tx| -> Result<bool, rusqlite::Error> {
                attempts.fetch_add(1, Ordering::SeqCst);
                setup_attempt(
                    &Some("prepared-hash".to_owned()),
                    "2024-01-01T00:00:00Z",
                    tx,
                )?;
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("synthetic busy".to_owned()),
                ))
            },
        )
        .expect_err("busy failures exhaust the retry budget");
        assert!(matches!(error, StoreError::BusyRetriesExhausted { .. }));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            usize::try_from(nd_db::store::TX_MAX_RETRIES).expect("retry budget fits usize") + 1
        );
        assert_eq!(
            busy_conn
                .query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "an exhausted busy transaction commits no user on any attempt"
        );
        assert_eq!(
            query_optional(&busy_conn, &get_sql(INITIAL_SETUP_FLAG_KEY), |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
            None,
            "an exhausted busy transaction commits no marker"
        );

        // Not busy: fails on the first occurrence, and is NOT the retryable
        // busy-exhaustion outcome.
        let (mut locked_conn, locked_path) = raw_conn();
        let attempts = AtomicUsize::new(0);
        let error = with_tx_retry(
            &mut locked_conn,
            zero,
            |tx| -> Result<bool, rusqlite::Error> {
                attempts.fetch_add(1, Ordering::SeqCst);
                setup_attempt(
                    &Some("prepared-hash".to_owned()),
                    "2024-01-01T00:00:00Z",
                    tx,
                )?;
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
                    Some("synthetic locked".to_owned()),
                ))
            },
        )
        .expect_err("SQLITE_LOCKED is not retried");
        assert!(matches!(error, StoreError::Other(_)));
        assert!(!matches!(error, StoreError::BusyRetriesExhausted { .. }));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a not-busy failure runs the body once"
        );
        assert_eq!(
            locked_conn
                .query_row("SELECT count(*) FROM user", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "a not-busy failure commits nothing"
        );

        let _ = std::fs::remove_file(busy_path);
        let _ = std::fs::remove_file(locked_path);
    }
}
