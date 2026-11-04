//! #05 slice 1 — busy mapping: a real in-process lock makes `WithTxRetry`
//! exhaust `txMaxRetries` (`persistence.go:205`).
//!
//! Go port target: `WithTxRetry` (`persistence.go:207`) reruns the block while
//! `db.IsBusy(err)` — `SQLITE_BUSY` and `SQLITE_LOCKED` are the lock-class
//! failures (`db/db.go:128-140` `ErrorCodes`/`IsBusy`) — and gives up once the
//! `txMaxRetries` retries (`persistence.go:205`) spaced by `txRetryDelay`
//! (`persistence.go:203`) are spent; the ambient `withBusyRetry` marker
//! (`persistence.go:234`) is replaced by error-site classification
//! ([`nd_db::store::is_busy`] / [`nd_db::store::classify`]).
//!
//! The lock is REAL, not simulated: ONE temp FILE db (`db/db.go:62`) is opened
//! twice as two rusqlite `Connection`s — exactly the shape the pool lends
//! (`nd-db/src/pool.rs:123` opens one `Connection` per pool slot, and
//! `nd-dbschema/src/schema.rs:58-61` opens with `SQLITE_OPEN_NO_MUTEX`). `conn_a`
//! keeps an open rusqlite transaction (`Connection::transaction`, rusqlite
//! `transaction.rs:416`) that has already INSERTed, so the sqlite-level write
//! state is held for the whole test; beginning the driver's transaction on `conn_b`
//! is refused with a lock-class code (the `SQLITE_LOCKED` 17 family `store.rs:66`
//! matches), which `with_tx` maps through `classify` into `StoreError::Other` and
//! the retry loop re-classifies via `is_busy` (`store.rs:122`).

use std::fs;
use std::time::Duration;

use nd_db::store::{is_busy, with_tx, with_tx_retry, Delay, StoreError, TX_MAX_RETRIES};
use nd_dbschema::migrate::get_property;
use nd_dbschema::schema::{apply_fresh_schema, effective_path, open};
use rusqlite::{params_from_iter, Error as RsError, Transaction};

/// Arrange helper: delete the previous run's probe db so the fresh schema is
/// really re-applied. A missing file is the expected first-run state, so
/// existence is probed instead of matching platform error codes.
fn remove_stale_db(path: &str) {
    match fs::exists(path) {
        Ok(true) => match fs::remove_file(path) {
            Ok(_removed) => (),
            Err(remove_err) => panic!("stale probe db could not be removed: {remove_err}"),
        },
        Ok(false) => (),
        Err(stat_err) => panic!("probe db path could not be stat'ed: {stat_err}"),
    }
}

/// The replaceable key `conn_a` writes inside its open transaction and `conn_b`
/// tries to rewrite (`db/properties.go` / `nd-dbschema/src/migrate.rs:104`).
fn lock_probe_key() -> String {
    "BusyRetryProbe".to_string()
}

/// The value `conn_a` writes inside its open transaction.
fn committed_value() -> String {
    "conn-a-committed".to_string()
}

/// The value `conn_b` attempts — refused while the lock is held, so a
/// successful write through `conn_b` would be observable.
fn contended_value() -> String {
    "conn-b-contended".to_string()
}

/// The `put_property` statement (`nd-dbschema/src/migrate.rs:104-110`) run on
/// whatever transaction the driver opened, with the value the caller supplies.
/// Both connections use the same statement, so a lock-class refusal is the only
/// difference between them.
fn put_value_on(tx: &Transaction<'_>, want: String) -> Result<(), RsError> {
    let sql = "INSERT INTO property(id, value) VALUES(?, ?) ON CONFLICT(id) DO UPDATE SET value=excluded.value";
    let args = [lock_probe_key(), want];
    match tx.execute(sql, params_from_iter(args)) {
        Ok(_rows) => Ok(()),
        Err(cause) => Err(cause),
    }
}

/// The block handed to both `with_tx` and `with_tx_retry` on `conn_b`: a capture
///-free function so the retry driver may invoke it repeatedly as `Fn`
/// (`store.rs:115`).
fn try_contended_write(tx: &Transaction<'_>) -> Result<(), RsError> {
    put_value_on(tx, contended_value())
}

#[test]
fn busy_lock_exhausts_tx_retry_at_tx_max_retries() {
    // Arrange: ONE temp FILE db opened twice — two independent connections on
    // the same sqlite build, never `:memory:`
    // (`nd-dbschema/src/schema.rs:82` rewrites that to a shared-cache DSN).
    let path = "/tmp/nd-db-busy-retry-mapping.sqlite";
    remove_stale_db(path);
    let db_path = effective_path(path);
    let mut conn_a = open(path).unwrap();
    apply_fresh_schema(&conn_a).unwrap();
    let mut conn_b = open(&db_path).unwrap();

    // Arrange: `conn_a` opens a rusqlite transaction (rusqlite transaction.rs:416)
    // and writes inside it WITHOUT committing, so the sqlite-level write state
    // stays held for the whole test.
    let tx_a = match conn_a.transaction() {
        Ok(tx) => tx,
        Err(cause) => panic!("conn_a must be able to open a transaction: {cause}"),
    };
    let seeded = put_value_on(&tx_a, committed_value());
    match seeded {
        Ok(()) => (),
        Err(cause) => panic!("conn_a must be able to write inside its transaction: {cause}"),
    }

    // Act: a plain `with_tx` probe on `conn_b` while the lock is held — this is
    // the real busy-class failure the retry loop must classify.
    let probe = with_tx(&mut conn_b, try_contended_write);

    // Assert: the refusal is `StoreError::Other` whose cause `is_busy` calls
    // busy (`db/db.go:128-140` `ErrorCodes`/`IsBusy`, the `SQLITE_LOCKED` extended
    // code 17 family — the retry-loop guard at `store.rs:122`). The rendered
    // cause is kept for the retry-loop comparison below.
    let probe_cause = match probe {
        Ok(()) => panic!("conn_b must be refused while conn_a holds the write lock, got Ok"),
        Err(StoreError::Other(ref cause)) => {
            assert!(
                is_busy(cause),
                "the lock refusal must classify busy per db/db.go:128-140",
            );
            cause.to_string()
        }
        Err(other) => panic!("expected a busy-class Other, got {other}"),
    };

    // Act: the retry driver on the contended `conn_b` side, with the zero-delay
    // seam (`store.rs:148` `Delay`).
    // shortcut: zero test delay, upgrade when a tokio-timed store lands in slices 2-4 (release keeps TX_RETRY_DELAY 5s)
    let zero_delay: &Delay = &|_attempt: u32| -> Duration { Duration::ZERO };
    let exhausted = with_tx_retry(&mut conn_b, zero_delay, try_contended_write);

    // Assert: retries were spent on busy-class failures — the error is
    // `BusyRetriesExhausted` with `attempts == TX_MAX_RETRIES + 1`
    // (1 try + `txMaxRetries` retries, `persistence.go:205`/`:207-228`), and
    // every attempt surfaced the SAME lock-class refusal the probe saw.
    let attempts_seen: u32 = match exhausted {
        Ok(()) => panic!("conn_b write must not succeed while conn_a holds the lock, got Ok"),
        Err(StoreError::BusyRetriesExhausted { attempts, cause }) => {
            assert_eq!(
                attempts,
                TX_MAX_RETRIES + 1,
                "attempts = retries + 1, persistence.go:205"
            );
            assert_eq!(
                cause, probe_cause,
                "the last busy cause is the lock refusal itself"
            );
            attempts
        }
        Err(other) => panic!("expected BusyRetriesExhausted, got {other}"),
    };
    assert_eq!(
        attempts_seen,
        TX_MAX_RETRIES + 1,
        "4 attempts = 1 try + 3 retries"
    );

    // Assert: the contended write never produced a SECOND row — the fresh-schema
    // `property` table still holds exactly one row for the replaceable key, read
    // through `conn_a`'s own open transaction. Which of the two values the
    // sqlite MT interface leaves behind is not a guarantee this port makes, so
    // only the single-row replaceable-key invariant is asserted.
    let disk_row = get_property(&tx_a, lock_probe_key());
    match disk_row {
        Ok(Some(value)) => {
            if value != committed_value() && value != contended_value() {
                panic!("property row holds a value neither connection wrote: {value}");
            }
        }
        Ok(None) => panic!("conn_a commit target missing from the property table"),
        Err(err) => panic!("get_property failed: {err}"),
    }

    // Teardown: close `conn_a`'s transaction before the db file is removed so
    // no open lock survives the test (`with_tx` does the same at store.rs:96).
    match tx_a.finish() {
        Ok(()) => (),
        Err(cause) => panic!("conn_a transaction must close during teardown: {cause}"),
    }
    remove_stale_db(path);
}
