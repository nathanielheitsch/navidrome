//! DB-04 — the EXECUTED property writes (`persistence/property_repository.go:23-35,
//! 60-62`) through the DB-00 typed transaction seam.
//!
//! Go truth under test: `propertyRepository.Put` (`:23-35`) runs the UPDATE first
//! and the INSERT only when `count == 0` (`:29-31` returns as soon as `count > 0`);
//! `propertyRepository.Delete` (`:60-62`) goes through `sqlRepository.delete`
//! (`sql_base_repository.go:644-648`), which discards the affected-row count, so a
//! missing id is a silent no-op — `deleteByID` (`:650-658`) is the arm that reports
//! `model.ErrNotFound` and this repository does not use it.
//!
//! Go semantics here are SOURCE-VERIFIED (the pinned file plus `sql_base_repository.go`
//! cited above); every behavioral claim below is then EXECUTED against a real SQLite
//! DB built from the fresh DDL (`nd_dbschema::schema::apply_fresh_schema`, one
//! isolated temp file per invocation, `_foreign_keys=on` — Go's `db/db.go:55` DSN
//! parameter). The write is issued through the production
//! [`nd_db::property_repo::put_in_tx`]/[`nd_db::property_repo::delete_in_tx`] on the
//! caller's transaction and read back through the production
//! [`nd_db::property_repo::PropertyRepo`]. No durable Go oracle directory exists for
//! this package (`docs/rust-port/subplans/db-runtime.md` DB-04 point 8 records the
//! oracle as the public Go Property methods), so no oracle fixture is invented here.
//! There is deliberately no duplicate write helper in this file — the tests call the
//! production API, which is what makes the ordering, rollback and retry claims
//! executed rather than restated.

use std::convert::Infallible;
use std::error::Error;
use std::time::Duration;

use nd_db::pool::{Pool, PoolConfig};
use nd_db::property_repo::{delete_in_tx, put_in_tx, PropertyError, PropertyRepo};
use nd_db::store::{is_busy, try_with_tx_retry, SQLStore, TX_MAX_RETRIES};
use nd_db::{DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::{apply_fresh_schema, open};
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Error as SqliteError, TransactionBehavior};

fn fresh_path() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-property-writes-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    path
}

/// Go's `db/db.go:55` DSN parameter: `foreign_keys` ON for every handle.
fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = fresh_path();
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// The same fixture with a single-slot pool.
fn fresh_single_store() -> (SQLStore, std::path::PathBuf) {
    let path = fresh_path();
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 1)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Re-open a file that a raw connection already configured (a hand-made schema the
/// fresh DDL must not overwrite) through a size-1 pool. `Pool` deliberately refuses
/// raw handles, so the path is re-opened rather than admitted.
fn pool_over(path: &std::path::Path) -> SQLStore {
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(path), 1)).unwrap();
    pool.put_back(first).unwrap();
    SQLStore::new(pool)
}

/// Read one key through the production repository on a leased connection.
fn get(store: &SQLStore, id: &str) -> Result<String, PropertyError> {
    store
        .with_connection_sync(|conn| Ok(PropertyRepo::new(conn).get(id)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// `DefaultGet` through the production repository, with the store error unwrapped so
/// the repository's own error value is what a test asserts on.
fn default_get(store: &SQLStore, id: &str, fallback: &str) -> Result<String, PropertyError> {
    store
        .with_connection_sync(|conn| Ok(PropertyRepo::new(conn).default_get(id, fallback)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// Commit one `Put` through the typed seam, exactly as a caller would.
fn put_committed(store: &SQLStore, id: &str, value: &str) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| put_in_tx(tx, id, value))
        .unwrap_or_else(|e| panic!("put_in_tx({id:?}, {value:?}) failed: {e}"));
}

/// Commit one `Delete` through the typed seam.
fn delete_committed(store: &SQLStore, id: &str) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| delete_in_tx(tx, id))
        .unwrap_or_else(|e| panic!("delete_in_tx({id:?}) failed: {e}"));
}

/// `INSERT INTO property` outside the ported API, for fixture cells the tests must
/// control exactly (an INTEGER storage class, a NULL, an original value).
fn raw_insert(store: &SQLStore, id: &str, value: SqlValue) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "INSERT INTO property (id, value) VALUES (?, ?)",
                rusqlite::params_from_iter([SqlValue::Text(id.to_string()), value].iter()),
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("fixture insert for {id:?} failed: {e}"));
}

fn row_count(store: &SQLStore) -> i64 {
    store
        .with_connection_sync(|conn| {
            conn.query_row("SELECT count(*) FROM property", [], |r| r.get::<_, i64>(0))
        })
        .unwrap_or_else(|e| panic!("count failed: {e}"))
}

/// Property tables present in the main schema (the injection control).
fn property_tables(store: &SQLStore) -> i64 {
    store
        .with_connection_sync(|conn| {
            conn.query_row(
                "SELECT count(*) FROM main.sqlite_master \
                 WHERE type = 'table' AND name = 'property'",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .unwrap_or_else(|e| panic!("sqlite_master read failed: {e}"))
}

/// Walk the `Error::source` chain for the concrete SQLite failure — the E1
/// requirement that a typed error keeps `rusqlite::Error` reachable rather than
/// stringified.
fn sqlite_source<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a SqliteError> {
    let mut current = Some(error);
    while let Some(node) = current {
        if let Some(found) = node.downcast_ref::<SqliteError>() {
            return Some(found);
        }
        current = node.source();
    }
    None
}

fn failure_code(error: &(dyn Error + 'static)) -> Option<i32> {
    sqlite_source(error).and_then(|source| match source {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    })
}

#[test]
fn fixture_handles_run_the_real_schema_with_foreign_keys_on() {
    let (store, path) = fresh_store();
    let (fk, tables) = store
        .with_connection_sync(|conn| {
            Ok((
                conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
                property_tables_of(conn),
            ))
        })
        .unwrap();
    assert_eq!(fk, 1, "the DSN's `_foreign_keys=on` holds per handle");
    assert_eq!(
        tables, 1,
        "the fresh DDL declares exactly one property table"
    );
    std::fs::remove_file(&path).ok();
}

fn property_tables_of(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM main.sqlite_master WHERE type = 'table' AND name = 'property'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
}

/// Go `Put` (`property_repository.go:23-35`): INSERT when the UPDATE matched no row,
/// UPDATE when it matched one, and never both. Zero, one and many keys; empty and
/// adversarial bound strings; unrelated keys untouched.
///
/// Non-vacuity: a second `Put` of the SAME key (including the byte-identical value)
/// proves the UPDATE arm short-circuits before the INSERT (`:29-31`). If the INSERT
/// ran anyway, the `id` primary key (`schema_ddl.rs:53`) would fail with
/// SQLITE_CONSTRAINT, so this test fails loudly instead of silently storing a
/// duplicate — a renderer-only assertion cannot observe that.
#[test]
fn property_put_insert_update_empty_and_adversarial_key() {
    let (store, path) = fresh_store();

    // Zero keys: a transaction whose only write deletes an absent row leaves the
    // table empty.
    store
        .try_with_tx_sync::<(), Infallible>(|tx| {
            delete_in_tx(tx, "absent")?;
            delete_in_tx(tx, "")
        })
        .unwrap_or_else(|e| panic!("delete of an absent key must be a no-op: {e}"));
    assert_eq!(row_count(&store), 0, "Go's silent delete stores nothing");
    assert!(matches!(
        get(&store, "absent"),
        Err(PropertyError::NotFound)
    ));

    // One key: the UPDATE matches 0 rows, so the INSERT runs (`:32-33`).
    put_committed(&store, "last_scan.md5", "abc123");
    assert_eq!(get(&store, "last_scan.md5").unwrap(), "abc123");
    assert_eq!(row_count(&store), 1, "the first Put inserted one row");

    // Same key, different value: the UPDATE matches (`count > 0`) and Go returns
    // before the INSERT, so the row count stays 1.
    put_committed(&store, "last_scan.md5", "def456");
    assert_eq!(get(&store, "last_scan.md5").unwrap(), "def456");
    assert_eq!(
        row_count(&store),
        1,
        "the update leg did not fall through to insert"
    );

    // Byte-identical value on an existing row: still the UPDATE arm.
    put_committed(&store, "last_scan.md5", "def456");
    assert_eq!(get(&store, "last_scan.md5").unwrap(), "def456");
    assert_eq!(
        row_count(&store),
        1,
        "an identical re-Put is an update, not a second row"
    );

    // Empty key and empty value are ordinary bound TEXT: Go validates nothing
    // (`:23-35`), and the column default `''` is a stored value, not a sentinel.
    put_committed(&store, "", "");
    assert_eq!(get(&store, "").unwrap(), "", "the empty key is a real row");
    assert_eq!(row_count(&store), 2);

    // Adversarial strings stay bound values, never SQL. `varchar(255)` is not a length
    // constraint in SQLite, so the long value is stored in full — as in Go, which also
    // binds it whole.
    let long = "z".repeat(1_000);
    let adversarial: &[(&str, &str)] = &[
        ("Robert'; DROP TABLE property;--", "injected'),(1,'x"),
        ("quote\"key", "tab\nnewline"),
        ("percent%_like", "%_%"),
        ("unicode-ключ", "値🎵"),
        ("long", &long),
    ];
    for (id, value) in adversarial {
        put_committed(&store, id, value);
        assert_eq!(
            get(&store, id).unwrap(),
            *value,
            "adversarial key {id:?} round-trips as bound text"
        );
    }
    assert_eq!(
        property_tables(&store),
        1,
        "the injected DROP TABLE stayed a bound value"
    );

    // Many keys in ONE caller transaction: all commit together.
    store
        .try_with_tx_sync::<(), Infallible>(|tx| {
            for n in 0..5 {
                put_in_tx(tx, &format!("batch.{n}"), &format!("v{n}"))?;
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("batch put failed: {e}"));
    for n in 0..5 {
        assert_eq!(
            get(&store, &format!("batch.{n}")).unwrap(),
            format!("v{n}"),
            "key {n} from the same transaction"
        );
    }

    // An unrelated key with an INTEGER storage class (Go reads it as decimal text)
    // survives writes to other keys.
    raw_insert(&store, "num", SqlValue::Integer(7));
    put_committed(&store, "last_scan.md5", "final");
    assert_eq!(get(&store, "num").unwrap(), "7", "unrelated key preserved");
    assert_eq!(get(&store, "last_scan.md5").unwrap(), "final");

    // A write issued on a transaction that is dropped without committing rolls back
    // (rusqlite's default DropBehavior), and the committed keys survive. The
    // transaction is opened by the caller on its own handle — `put_in_tx` never
    // begins or commits one.
    {
        let mut conn = open(&dsn(&path)).unwrap();
        conn.busy_timeout(Duration::ZERO).unwrap();
        let tx = conn.transaction().unwrap();
        put_in_tx(&tx, "never_committed", "ghost").unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT value FROM property WHERE id = 'never_committed'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "ghost",
            "visible inside the uncommitted transaction"
        );
        drop(tx);
        assert!(matches!(
            get(&store, "never_committed"),
            Err(PropertyError::NotFound)
        ));
        assert_eq!(get(&store, "num").unwrap(), "7");
    }

    std::fs::remove_file(&path).ok();
}

/// Go `Delete` (`:60-62` via `sql_base_repository.go:644-648`): a missing id is a
/// successful no-op; an existing id is removed and the subsequent `Get` reports
/// `model.ErrNotFound` (`sql_base_repository.go:375-388`). Asserted after the
/// transaction commits, so it is committed state, not tx-local visibility.
#[test]
fn delete_missing_is_noop_and_existing_reads_back_not_found_after_commit() {
    let (store, path) = fresh_store();
    raw_insert(&store, "keep", SqlValue::Text("keep-value".to_string()));
    raw_insert(&store, "drop", SqlValue::Text("drop-value".to_string()));

    // Absent keys (including the empty key): Ok, and nothing else changed.
    store
        .try_with_tx_sync::<(), Infallible>(|tx| {
            delete_in_tx(tx, "never-existed")?;
            delete_in_tx(tx, "")
        })
        .unwrap_or_else(|e| panic!("a missing delete must not error: {e}"));
    assert_eq!(get(&store, "keep").unwrap(), "keep-value");
    assert_eq!(get(&store, "drop").unwrap(), "drop-value");
    assert_eq!(row_count(&store), 2);

    // Existing key: removed on commit, and the read reports NotFound — not the empty
    // string Go returns beside its error (`property_repository.go:42-45`).
    delete_committed(&store, "drop");
    assert!(matches!(get(&store, "drop"), Err(PropertyError::NotFound)));
    assert_eq!(row_count(&store), 1);

    // `DefaultGet` folds that missing key into its default with no error, while the
    // surviving key still returns its stored value, and the read writes nothing.
    assert_eq!(
        default_get(&store, "drop", "fallback").unwrap(),
        "fallback",
        "NotFound is the only arm DefaultGet folds"
    );
    assert_eq!(
        default_get(&store, "keep", "fallback").unwrap(),
        "keep-value",
        "an existing key is never replaced by the default"
    );
    assert_eq!(row_count(&store), 1, "DefaultGet reads, it does not write");

    // Deleting the same key twice is still a no-op the second time.
    delete_committed(&store, "drop");
    assert_eq!(get(&store, "keep").unwrap(), "keep-value");
    assert_eq!(row_count(&store), 1);
    std::fs::remove_file(&path).ok();
}

/// Forced failure AFTER successful writes: the whole caller transaction rolls back and
/// the original committed values read back unchanged (DB-04 point 7).
///
/// The failure is forced inside the `property` table itself by a `RAISE(ABORT)`
/// trigger, so nothing about the transaction semantics is hand-made.
#[test]
fn property_bundle_failure_rolls_back_and_preserves_the_original_value() {
    let (store, path) = fresh_store();
    store
        .with_connection_sync(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER property_poison_insert BEFORE INSERT ON property \
                 WHEN NEW.id = 'poison' \
                 BEGIN SELECT RAISE(ABORT, 'forced property failure'); END;\
                 CREATE TRIGGER property_poison_update BEFORE UPDATE ON property \
                 WHEN NEW.id = 'poisoned' \
                 BEGIN SELECT RAISE(ABORT, 'forced property update failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
    put_committed(&store, "last_scan.md5", "original");
    put_committed(&store, "other", "other-original");

    // A bundle: a successful UPDATE, a successful INSERT, a successful DELETE, then a
    // statement that aborts. All of it must be undone.
    let failure = store.try_with_tx_sync::<(), Infallible>(|tx| {
        put_in_tx(tx, "last_scan.md5", "rewritten")?;
        put_in_tx(tx, "added", "added-value")?;
        delete_in_tx(tx, "other")?;
        put_in_tx(tx, "poison", "never")
    });
    match failure {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ref source,
        }) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
            assert!(
                sqlite_source(source).is_some(),
                "the concrete rusqlite error stays reachable through source()"
            );
        }
        other => panic!("expected a typed Body-stage SQLite failure, got {other:?}"),
    }

    // Nothing in the bundle survived: the UPDATE reverted, the INSERT and DELETE were
    // undone, and no partial write leaked.
    assert_eq!(get(&store, "last_scan.md5").unwrap(), "original");
    assert_eq!(get(&store, "other").unwrap(), "other-original");
    assert!(matches!(get(&store, "added"), Err(PropertyError::NotFound)));
    assert!(matches!(
        get(&store, "poison"),
        Err(PropertyError::NotFound)
    ));
    assert_eq!(row_count(&store), 2, "the failed bundle stored nothing");

    // The failed transaction did not poison the pool: both handles are idle, in
    // autocommit, and reusable for a successful write.
    assert_eq!(store.idle_count().unwrap(), 2, "the lease was returned");
    store
        .try_with_tx_sync::<(), Infallible>(|tx| put_in_tx(tx, "after-failure", "works"))
        .unwrap_or_else(|e| panic!("the pooled handle stayed usable: {e}"));
    store
        .with_connection_sync(|conn| {
            assert!(
                conn.is_autocommit(),
                "no transaction left open on the handle"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(get(&store, "after-failure").unwrap(), "works");

    // Second bundle, poisoned on the UPDATE leg: the abort fires AFTER the UPDATE
    // statement has already matched a row, so the rollback must undo an UPDATE (not
    // just an INSERT), and Go's `count > 0` short-circuit must not hide the failure.
    put_committed(&store, "poisoned", "keep-me");
    let update_failure = store.try_with_tx_sync::<(), Infallible>(|tx| {
        put_in_tx(tx, "last_scan.md5", "rewritten-again")?;
        put_in_tx(tx, "poisoned", "never-either")
    });
    match update_failure {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ref source,
        }) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
        }
        other => panic!("expected a typed Body-stage UPDATE failure, got {other:?}"),
    }
    assert_eq!(
        get(&store, "poisoned").unwrap(),
        "keep-me",
        "the aborted UPDATE left the stored value untouched"
    );
    assert_eq!(
        get(&store, "last_scan.md5").unwrap(),
        "original",
        "the earlier UPDATE in the same bundle reverted too"
    );
    assert_eq!(
        row_count(&store),
        4,
        "rows = 2 originals + after-failure + poisoned; neither failed bundle added one"
    );
    std::fs::remove_file(&path).ok();
}

/// The retry path (`store::try_with_tx_retry`, the port of `persistence.go:207-228`)
/// retries an UPDATE blocked before mutation, then updates the existing row after
/// the holder rolls back. It does not exercise INSERT or commit-stage replay.
/// The busy holder is released inside the injected delay closure, so the
/// retry succeeds without real sleeps and without thread timing races.
#[test]
fn retry_put_is_idempotent_with_injected_delay_and_no_real_sleeps() {
    let path = fresh_path();
    let uri = dsn(&path);
    let mut holder = open(&uri).unwrap();
    holder.busy_timeout(Duration::ZERO).unwrap();
    apply_fresh_schema(&holder).unwrap();
    assert_eq!(
        holder
            .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "the raw fixture handle is FK-on too"
    );

    // Committed original state, then a lock-holding transaction whose own write never
    // commits.
    holder
        .execute(
            "INSERT INTO property (id, value) VALUES ('k', 'original')",
            [],
        )
        .unwrap();
    let held = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    held.execute("UPDATE property SET value = 'holder' WHERE id = 'k'", [])
        .unwrap();

    let mut contender = open(&uri).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();

    let attempts = std::cell::Cell::new(0u32);
    let delays = std::cell::RefCell::new(Vec::new());
    let held = std::cell::RefCell::new(Some(held));
    // Releasing the holder in the delay slot makes attempt 2 succeed deterministically.
    let zero = |attempt: u32| {
        delays.borrow_mut().push(attempt);
        drop(held.borrow_mut().take());
        Duration::ZERO
    };
    let result: Result<(), DbRuntimeError<Infallible>> =
        try_with_tx_retry(&mut contender, &zero, |tx| {
            attempts.set(attempts.get() + 1);
            put_in_tx(tx, "k", "final")
        });
    result.unwrap_or_else(|e| panic!("the replayed Put must succeed: {e}"));

    assert_eq!(
        attempts.get(),
        2,
        "one busy attempt, then one committed replay"
    );
    assert_eq!(*delays.borrow(), vec![0], "exactly one injected zero delay");
    let stored = contender
        .query_row("SELECT value FROM property WHERE id = 'k'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(
        stored, "final",
        "the replay converged, no leftover holder value"
    );
    let rows = contender
        .query_row("SELECT count(*) FROM property", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert_eq!(
        rows, 1,
        "the retried UPDATE preserves the existing row count"
    );
    assert!(
        contender.is_autocommit(),
        "the successful attempt committed and left no open transaction"
    );

    std::fs::remove_file(&path).ok();
}

/// The same loop when the lock is never released: Go's `WithTxRetry` gives up after
/// `txMaxRetries` (`persistence.go:205,218-227`) and the port reports the typed
/// [`DbRuntimeError::BusyExhausted`] with the real busy code and stage — no swallowed
/// error, no leaked mutation, and no real sleeping.
#[test]
fn retry_put_reports_typed_busy_exhaustion_and_leaks_no_mutation() {
    let path = fresh_path();
    let uri = dsn(&path);
    let mut holder = open(&uri).unwrap();
    holder.busy_timeout(Duration::ZERO).unwrap();
    apply_fresh_schema(&holder).unwrap();
    holder
        .execute(
            "INSERT INTO property (id, value) VALUES ('k', 'original')",
            [],
        )
        .unwrap();
    let held = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    held.execute("UPDATE property SET value = 'holder' WHERE id = 'k'", [])
        .unwrap();

    let mut contender = open(&uri).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();

    let attempts = std::cell::Cell::new(0u32);
    let delays = std::cell::RefCell::new(Vec::new());
    let zero = |attempt: u32| {
        delays.borrow_mut().push(attempt);
        Duration::ZERO
    };
    let result: Result<(), DbRuntimeError<Infallible>> =
        try_with_tx_retry(&mut contender, &zero, |tx| {
            attempts.set(attempts.get() + 1);
            put_in_tx(tx, "k", "final")
        });

    match result {
        Err(DbRuntimeError::BusyExhausted {
            attempts: reported,
            stage,
            ref source,
        }) => {
            assert_eq!(reported, TX_MAX_RETRIES + 1);
            assert_eq!(stage, TransactionStage::Body);
            assert!(
                is_busy(source),
                "the real busy code survives classification"
            );
            assert!(
                sqlite_source(source).is_some(),
                "no stringification: rusqlite::Error stays in the chain"
            );
        }
        other => panic!("expected typed busy exhaustion, got {other:?}"),
    }
    assert_eq!(attempts.get(), TX_MAX_RETRIES + 1);
    assert_eq!(
        *delays.borrow(),
        vec![0, 1, 2],
        "Go's three-slot schedule, injected as zero"
    );
    let stored = contender
        .query_row("SELECT value FROM property WHERE id = 'k'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(stored, "original", "every aborted attempt rolled back");
    assert!(contender.is_autocommit());
    drop(held);
    std::fs::remove_file(&path).ok();
}

/// A non-busy SQLite failure fails immediately on the first attempt
/// (`persistence.go:218` `!db.IsBusy(err)`) and is reported as
/// `Sqlite { stage: Body }` — retry is a busy-only behavior, not a general "try again".
#[test]
fn non_busy_sqlite_failure_is_not_retried() {
    let path = fresh_path();
    let uri = dsn(&path);
    let mut conn = open(&uri).unwrap();
    apply_fresh_schema(&conn).unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    conn.execute(
        "CREATE TRIGGER property_poison BEFORE INSERT ON property \
         WHEN NEW.id = 'poison' BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        [],
    )
    .unwrap();

    let attempts = std::cell::Cell::new(0u32);
    let delays = std::cell::Cell::new(0u32);
    let zero = |_| {
        delays.set(delays.get() + 1);
        Duration::ZERO
    };
    let result: Result<(), DbRuntimeError<Infallible>> =
        try_with_tx_retry(&mut conn, &zero, |tx| {
            attempts.set(attempts.get() + 1);
            put_in_tx(tx, "poison", "x")
        });
    match result {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ref source,
        }) => {
            assert!(!is_busy(source));
            assert_eq!(failure_code(source), Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
        }
        other => panic!("expected one non-retryable typed failure, got {other:?}"),
    }
    assert_eq!(
        attempts.get(),
        1,
        "a constraint failure is not a busy failure"
    );
    assert_eq!(delays.get(), 0, "no backoff was scheduled");
    assert_eq!(
        conn.query_row("SELECT count(*) FROM property", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "the aborted INSERT left nothing"
    );
    std::fs::remove_file(&path).ok();
}

/// A missing `property` table is a HARD typed SQLite error on both write legs — not a
/// silent success, and not a domain error (`Infallible` makes `Domain` uninhabited, so
/// nothing can be fabricated there).
#[test]
fn missing_property_table_is_a_typed_sqlite_error_not_a_silent_write() {
    let path = fresh_path();
    let mut conn = open(&dsn(&path)).unwrap();
    conn.execute_batch("CREATE TABLE unrelated(id TEXT PRIMARY KEY);")
        .unwrap();

    let put = conn
        .transaction()
        .map(|tx| put_in_tx(&tx, "k", "v"))
        .unwrap();
    match put {
        Err(DbOperationError::Sqlite(ref source)) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_ERROR));
            assert!(
                source.to_string().contains("property"),
                "SQLite names the missing table: {source}"
            );
        }
        other => panic!("expected a typed SQLite failure, got {other:?}"),
    }

    let delete = conn.transaction().map(|tx| delete_in_tx(&tx, "k")).unwrap();
    match delete {
        Err(DbOperationError::Sqlite(ref source)) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_ERROR));
        }
        other => panic!("expected a typed SQLite failure, got {other:?}"),
    }

    assert_eq!(
        conn.query_row("SELECT count(*) FROM unrelated", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "the failed writes touched nothing else"
    );
    std::fs::remove_file(&path).ok();
}

/// A malformed `property` table (declared without the `value` column) fails `Put` as a
/// typed SQLite error on both legs, while `Delete` — which references only `id`, like
/// Go's `Delete(property).Where(Eq{"id": id})` (`sql_base_repository.go:644-648`) —
/// still executes. The read side reports `PropertyError::Sqlite`, and `DefaultGet`
/// does NOT fold that into its default: Go folds only `model.ErrNotFound`
/// (`property_repository.go:49-58`), so a default beside a hidden failure would be an
/// invention.
#[test]
fn malformed_property_schema_fails_writes_and_default_get_does_not_fold_it() {
    let path = fresh_path();
    let conn = open(&dsn(&path)).unwrap();
    conn.execute_batch("CREATE TABLE property ( id varchar(255) not null primary key );")
        .unwrap();
    drop(conn);
    let store = pool_over(&path);

    let failure = store.try_with_tx_sync::<(), Infallible>(|tx| put_in_tx(tx, "k", "v"));
    match failure {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ref source,
        }) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_ERROR));
            assert!(
                source.to_string().contains("value"),
                "SQLite names the missing column: {source}"
            );
            assert!(sqlite_source(source).is_some());
        }
        other => panic!("expected a typed Body-stage SQLite failure, got {other:?}"),
    }

    // Delete touches only `id`, so it is not a schema failure — and it stores nothing.
    delete_committed(&store, "k");
    assert_eq!(row_count(&store), 0);

    let read = get(&store, "k");
    assert!(
        matches!(read, Err(PropertyError::Sqlite(_))),
        "a missing column is a statement failure, not a missing row: {read:?}"
    );
    let folded = default_get(&store, "k", "fallback");
    assert!(
        matches!(folded, Err(PropertyError::Sqlite(_))),
        "DefaultGet must not hand back a default beside a hidden error: {folded:?}"
    );
    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "the malformed schema did not poison the pool"
    );

    std::fs::remove_file(&path).ok();
}

/// NULL is the only realistic scan failure and the fresh schema forbids it
/// (`schema_ddl.rs:53` declares `value ... not null`), so this control hand-builds the
/// same table without that constraint. `Get` fails on the NULL cell, and `DefaultGet`
/// propagates the failure rather than folding it: Go's `errors.Is(err, model.ErrNotFound)`
/// guard (`property_repository.go:51`) folds only the missing row.
#[test]
fn default_get_folds_only_the_missing_key_and_propagates_a_scan_failure() {
    let path = fresh_path();
    let conn = open(&dsn(&path)).unwrap();
    conn.execute_batch(
        "CREATE TABLE property ( id varchar(255) not null primary key, value varchar(255) default '' );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO property (id, value) VALUES ('nulls', NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO property (id, value) VALUES ('set', 'stored')",
        [],
    )
    .unwrap();
    drop(conn);
    let store = pool_over(&path);

    assert_eq!(get(&store, "set").unwrap(), "stored");
    assert!(matches!(
        get(&store, "missing"),
        Err(PropertyError::NotFound)
    ));
    assert_eq!(
        default_get(&store, "missing", "fallback").unwrap(),
        "fallback",
        "the missing key folds into the default"
    );

    let scan = get(&store, "nulls");
    assert!(
        matches!(scan, Err(PropertyError::Sqlite(_))),
        "Go's non-pointer string dest fails on NULL: {scan:?}"
    );
    let folded = default_get(&store, "nulls", "fallback");
    assert!(
        matches!(folded, Err(PropertyError::Sqlite(_))),
        "a scan failure is not folded into the default: {folded:?}"
    );

    // The write legs still work on this schema, so the failure is a read-side scan and
    // not a broken fixture.
    put_committed(&store, "nulls", "now-set");
    assert_eq!(get(&store, "nulls").unwrap(), "now-set");

    std::fs::remove_file(&path).ok();
}

/// The single-slot pool case: put/update/delete run on one lease, commit together, and
/// the lease is returned after both success and failure.
#[test]
fn writes_and_deletes_commit_through_a_size_one_pool_lease() {
    let (store, path) = fresh_single_store();
    store
        .try_with_tx_sync::<(), Infallible>(|tx| {
            put_in_tx(tx, "auth.secret", "enc1")?;
            delete_in_tx(tx, "auth.secret")?;
            put_in_tx(tx, "auth.secret", "enc2")
        })
        .unwrap_or_else(|e| panic!("bundle on one lease failed: {e}"));
    assert_eq!(get(&store, "auth.secret").unwrap(), "enc2");
    assert_eq!(row_count(&store), 1);
    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "lease returned after commit"
    );

    // A failed bundle on the same single-slot pool rolls back and still returns the
    // only handle.
    store
        .with_connection_sync(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER property_poison_insert BEFORE INSERT ON property \
                 WHEN NEW.id = 'poison' BEGIN SELECT RAISE(ABORT, 'forced'); END;",
            )?;
            Ok(())
        })
        .unwrap();
    let failure = store.try_with_tx_sync::<(), Infallible>(|tx| {
        put_in_tx(tx, "auth.secret", "enc3")?;
        put_in_tx(tx, "poison", "never")
    });
    assert!(matches!(
        failure,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ..
        })
    ));
    assert_eq!(
        get(&store, "auth.secret").unwrap(),
        "enc2",
        "the aborted update reverted"
    );
    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "the lease came back after the failure"
    );

    std::fs::remove_file(&path).ok();
}

/// A caller-owned domain error survives composition with a property write, and
/// that successful property write rolls back when the caller returns its error.
/// The separate non-busy SQLite control below tests the executor's SQLite arm;
/// it does not exercise a failing property operation through the composition.
#[test]
fn caller_domain_errors_compose_without_hiding_sqlite_arms() {
    #[derive(Debug, thiserror::Error)]
    #[error("caller domain failure")]
    struct Domain;

    let (store, path) = fresh_store();
    let failure = store.try_with_tx_sync::<(), Domain>(|tx| {
        put_in_tx(tx, "k", "v").map_err(|err| match err {
            DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
            // Uninhabited: there is no value that could reach this arm.
            DbOperationError::Domain(infallible) => match infallible {},
        })?;
        Err(DbOperationError::Domain(Domain))
    });
    assert!(matches!(failure, Err(DbRuntimeError::Domain(Domain))));
    assert!(matches!(get(&store, "k"), Err(PropertyError::NotFound)));
    assert_eq!(store.idle_count().unwrap(), 2, "both leases returned");

    // A generic executor control: this direct SQL failure is not a property failure
    // and must remain a typed, non-busy SQLite error rather than a domain error.
    let sqlite_failure = store.try_with_tx_sync::<(), Domain>(|tx| {
        tx.execute("DROP TABLE nonexistent_table", [])
            .map_err(DbOperationError::<Domain>::sqlite)?;
        Ok(())
    });
    match sqlite_failure {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ref source,
        }) => {
            assert_eq!(failure_code(source), Some(ffi::SQLITE_ERROR));
            assert!(!is_busy(source));
        }
        other => panic!("expected a typed Body-stage SQLite failure, got {other:?}"),
    }

    std::fs::remove_file(&path).ok();
}
