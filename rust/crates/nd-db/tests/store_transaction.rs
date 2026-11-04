//! #05 slice 1 — the transaction core of `persistence/persistence.go`.
//!
//! Go port target: `WithTx` (`persistence.go:165-171`) — BEGIN the dbx
//! transaction, run the block, commit on success and roll back on failure;
//! `txRetryDelay` (`persistence.go:203`); `txMaxRetries` (`persistence.go:205`);
//! `WithTxRetry` (`persistence.go:207`), which reruns the block only while
//! `db.IsBusy(err)` and fails immediately on the `!db.IsBusy(err)` branch
//! (`persistence.go:218`); and the ambient busy marker `withBusyRetry`
//! (`persistence.go:234`), replaced here by error-site classification
//! ([`nd_db::store::is_busy`] / [`nd_db::store::classify`]).
//!
//! Both tests open a temp FILE db (`db/db.go:62` real users are file dbss):
//! `:memory:` is never used, because `nd_dbschema/src/schema.rs:82` rewrites it
//! to the shared-cache DSN of `db/db.go:54-59`, which aliases instances across
//! concurrently running tests.

use std::cell::Cell;
use std::fs;
use std::time::Duration;

use nd_db::store::{is_busy, production_delay, with_tx, with_tx_retry, Delay, StoreError};
use nd_dbschema::migrate::get_property;
use nd_dbschema::schema::{apply_fresh_schema, open};
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
        Err(_stat_err) => panic!("probe db path could not be stat'ed: {_stat_err}"),
    }
}

/// The replaceable key written into the fresh-schema `property` table
/// (`db/properties.go`, `db/optimize.go:261-265`).
fn probe_key() -> String {
    "StoreTxProbe".to_string()
}

/// The value written and re-read by the success test.
fn probe_value() -> String {
    "landed".to_string()
}

/// Block seam for the success test: the same `put_property` statement
/// (`nd-dbschema/src/migrate.rs:105`) run inside the rusqlite transaction
/// (`Transaction` derefs to `Connection`, rusqlite transaction.rs:232).
fn put_probe_property(tx: &Transaction<'_>) -> Result<(), RsError> {
    let sql = "INSERT INTO property(id, value) VALUES(?, ?) ON CONFLICT(id) DO UPDATE SET value=excluded.value";
    let args = [probe_key(), probe_value()];
    match tx.execute(sql, params_from_iter(args)) {
        Ok(_rows) => Ok(()),
        Err(cause) => Err(cause),
    }
}

/// Block seam for the non-retry test: a statement sqlite rejects at prepare
/// time (bogus token plus a table that does not exist) — a plain SQLite error,
/// never a busy-class failure (`persistence.go:218` `!db.IsBusy(err)`).
fn bogus_sql_block(tx: &Transaction<'_>) -> Result<(), RsError> {
    match tx.execute("INSERTED INTO no_such_table VALUES(1)", []) {
        Ok(_rows) => Ok(()),
        Err(cause) => Err(cause),
    }
}

#[test]
fn with_tx_commit_is_read_back_by_get_property_on_same_conn() {
    // Arrange: a temp FILE db opened through `schema::open` and given the
    // fresh DDL (`apply_fresh_schema`), never `:memory:`.
    let path = "/tmp/nd-db-store-transaction.sqlite";
    remove_stale_db(path);
    let mut conn = open(path).unwrap();
    apply_fresh_schema(&conn).unwrap();

    // Act: the `property` write runs inside `with_tx` — `persistence.go:165-171`
    // (BEGIN, block, commit; rollback on block error).
    let written = with_tx(&mut conn, put_probe_property);

    // Assert: the transaction committed. Any other outcome is a port bug and
    // its classified reason is surfaced.
    match written {
        Ok(()) => (),
        Err(err) => panic!("WithTx must commit the property write: {err}"),
    }

    // Assert: the write landed — `get_property` (`db/optimize.go:267-274`)
    // reads the value back on the SAME conn.
    let read_back = get_property(&conn, probe_key());
    match read_back {
        Ok(Some(value)) => assert_eq!(
            value,
            probe_value(),
            "row round-trips through the committed tx"
        ),
        Ok(None) => panic!("property row missing after WithTx commit"),
        Err(err) => panic!("get_property failed: {err}"),
    }

    // Assert: the same block through `with_tx_retry` on the production
    // schedule (`persistence.go:207` `WithTxRetry`, `:203` `txRetryDelay` via
    // [`nd_db::store::production_delay`], `:205` `txMaxRetries`) succeeds on
    // the first attempt: no busy-class error => no rerun and no sleep.
    let prod: &Delay = &production_delay;
    let retried = with_tx_retry(&mut conn, prod, put_probe_property);
    match retried {
        Ok(()) => (),
        Err(err) => panic!("WithTxRetry must accept a not-busy success: {err}"),
    }

    // Assert: the ON CONFLICT rewrite (`db/properties.go`) left the same value
    // and the fresh-schema `property` table is still the only write target.
    let again = get_property(&conn, probe_key());
    match again {
        Ok(Some(value)) => assert_eq!(
            value,
            probe_value(),
            "replaceable key rewritten idempotently"
        ),
        Ok(None) => panic!("property row missing after WithTxRetry"),
        Err(err) => panic!("get_property failed: {err}"),
    }

    remove_stale_db(path);
}

#[test]
fn with_tx_retry_runs_block_once_on_not_busy_failure() {
    // Arrange: the same temp FILE db + fresh schema, plus a caller-owned
    // attempt counter the block increments — the observable standing in for
    // Go's ambient `withBusyRetry` attempt marker (`persistence.go:234`).
    let path = "/tmp/nd-db-store-transaction-once.sqlite";
    remove_stale_db(path);
    let mut conn = open(path).unwrap();
    apply_fresh_schema(&conn).unwrap();

    let attempts: Cell<usize> = Cell::new(0);

    // Act: the closure is a caller-owned `Cell` counter (shared borrow only),
    // so `with_tx_retry` may invoke it as `Fn`.
    let counting_block = |tx: &Transaction<'_>| -> Result<(), RsError> {
        attempts.set(attempts.get() + 1);
        bogus_sql_block(tx)
    };
    // shortcut: zero test delay, upgrade when a tokio-timed store lands in slices 2-4 (release keeps TX_RETRY_DELAY 5s)
    let zero_delay: &Delay = &|_attempt: u32| -> Duration { Duration::ZERO };
    let result = with_tx_retry(&mut conn, zero_delay, counting_block);

    // Assert: the block ran EXACTLY ONCE — `persistence.go:218` `!db.IsBusy(err)`
    // fails immediately, no rerun. No wall-time assertion: the count is the
    // observable for `txMaxRetries` (`persistence.go:205`).
    assert_eq!(
        attempts.get(),
        1,
        "not-busy failure => single attempt, persistence.go:218"
    );

    // Assert: the failure surfaced as `StoreError::Other` and its cause is
    // classified NOT busy by `is_busy` — the retry-loop guard that did not fire.
    match result {
        Ok(()) => panic!("a bogus SQL statement must fail"),
        Err(StoreError::Other(ref cause)) => assert!(
            !is_busy(cause),
            "a prepare-time SQL error is not a lock-class failure"
        ),
        Err(StoreError::BusyRetriesExhausted {
            attempts: used,
            cause,
        }) => panic!("not-busy failure must not exhaust retries ({used} attempts): {cause}"),
        Err(other) => panic!("unexpected store failure class: {other}"),
    }

    // Assert: the transaction rolled back — rusqlite `DropBehavior::Rollback`
    // (transaction.rs:62) plus the explicit `tx.finish()` in `with_tx` — so
    // nothing from the bogus statement is visible.
    let absent = get_property(&conn, probe_key());
    match absent {
        Ok(None) => (),
        Ok(Some(value)) => panic!("rollback lost: {value}"),
        Err(err) => panic!("get_property failed: {err}"),
    }

    remove_stale_db(path);
}
