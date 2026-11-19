//! Integration coverage for the typed transaction runtime and retry contract.

use std::error::Error;
use std::fs;
use std::time::Duration;

use nd_db::marshal::MarshalError;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::{
    production_delay, try_with_tx, try_with_tx_retry, with_tx, SQLStore, StoreError, TX_MAX_RETRIES,
};
use nd_db::{DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::open;
use rusqlite::{Connection, Error as SqliteError, Transaction, TransactionBehavior};

#[derive(Debug, thiserror::Error)]
#[error("operation looks busy but is a domain failure")]
struct BusyLookingDomain;

#[derive(Debug, thiserror::Error)]
#[error("domain wrapper around a SQLite failure: {0}")]
struct NestedBusyDomain(#[source] SqliteError);

fn db_path(label: &str) -> String {
    format!("/tmp/nd-db-typed-{label}-{}.sqlite", nd_id::new_random())
}

fn source_has_rusqlite(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(node) = current {
        if node.downcast_ref::<SqliteError>().is_some() {
            return true;
        }
        current = node.source();
    }
    false
}

fn open_pair(label: &str) -> (String, Connection, Connection) {
    let path = db_path(label);
    let a = Connection::open(&path).unwrap();
    let b = Connection::open(&path).unwrap();
    a.busy_timeout(Duration::ZERO).unwrap();
    b.busy_timeout(Duration::ZERO).unwrap();
    a.execute_batch("CREATE TABLE item(id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();
    (path, a, b)
}

fn write_item(tx: &Transaction<'_>, value: &str) -> Result<(), SqliteError> {
    tx.execute("INSERT INTO item(value) VALUES(?)", [value])?;
    Ok(())
}

fn insert_or_rollback_duplicate(tx: &Transaction<'_>) -> Result<(), SqliteError> {
    tx.execute("INSERT INTO item(value) VALUES('duplicate')", [])?;
    tx.execute(
        "INSERT OR ROLLBACK INTO item(value) VALUES('duplicate')",
        [],
    )?;
    Ok(())
}

#[test]
fn domain_failure_after_write_rolls_back_and_preserves_existing_marshal_error() {
    let (path, mut conn, _) = open_pair("domain");
    let result: Result<(), DbRuntimeError<MarshalError>> = try_with_tx(&mut conn, |tx| {
        write_item(tx, "rolled back").map_err(DbOperationError::sqlite)?;
        Err(DbOperationError::Domain(MarshalError::IdInMap))
    });

    assert!(matches!(
        result,
        Err(DbRuntimeError::Domain(MarshalError::IdInMap))
    ));
    assert_eq!(
        conn.query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn sqlite_or_rollback_is_body_failure_not_rollback_failure_for_typed_and_legacy_paths() {
    let path = db_path("or-rollback-typed");
    let (pool, initializer) = Pool::new(&PoolConfig::with_size(path.clone(), 1)).unwrap();
    initializer
        .execute_batch("CREATE TABLE item(value TEXT UNIQUE NOT NULL);")
        .unwrap();
    pool.put_back(initializer).unwrap();
    let store = SQLStore::new(pool);

    let typed = store.try_with_tx_sync::<_, MarshalError>(|tx| {
        insert_or_rollback_duplicate(tx).map_err(DbOperationError::sqlite)
    });
    assert!(matches!(
        typed,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            source: SqliteError::SqliteFailure(ref code, _)
        }) if code.extended_code & 0xff == rusqlite::ffi::SQLITE_CONSTRAINT
    ));
    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "autocommit lease is reusable"
    );
    store
        .with_connection_sync(|conn| {
            assert!(conn.is_autocommit());
            assert_eq!(
                conn.query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    store
        .try_with_tx_sync::<_, MarshalError>(|tx| {
            tx.execute("INSERT INTO item(value) VALUES('after typed')", [])
                .map_err(DbOperationError::sqlite)?;
            Ok(())
        })
        .unwrap();
    fs::remove_file(path).unwrap();

    let legacy_path = db_path("or-rollback-legacy");
    let mut legacy = Connection::open(&legacy_path).unwrap();
    legacy
        .execute_batch("CREATE TABLE item(value TEXT UNIQUE NOT NULL);")
        .unwrap();
    let legacy_result = with_tx(&mut legacy, insert_or_rollback_duplicate);
    assert!(matches!(
        legacy_result,
        Err(StoreError::Other(SqliteError::SqliteFailure(ref code, _)))
            if code.extended_code & 0xff == rusqlite::ffi::SQLITE_CONSTRAINT
    ));
    assert!(legacy.is_autocommit());
    assert_eq!(
        legacy
            .query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    with_tx(&mut legacy, |tx| {
        tx.execute("INSERT INTO item(value) VALUES('after legacy')", [])?;
        Ok(())
    })
    .unwrap();
    fs::remove_file(legacy_path).unwrap();
}

#[test]
fn successful_commit_and_size_one_pool_lease_release() {
    let path = db_path("pool");
    let (pool, first) = Pool::new(&PoolConfig::with_size(path.clone(), 1)).unwrap();
    first
        .execute_batch("CREATE TABLE item(id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();
    pool.put_back(first).unwrap();
    let store = SQLStore::new(pool);

    store
        .try_with_tx_sync::<_, MarshalError>(|tx| {
            write_item(tx, "committed").map_err(DbOperationError::sqlite)?;
            Ok(())
        })
        .unwrap();
    store
        .try_with_tx_retry_sync::<_, MarshalError>(|tx| {
            write_item(tx, "retry committed").map_err(DbOperationError::sqlite)?;
            Ok(())
        })
        .unwrap();
    let failure =
        store.try_with_tx_sync::<(), _>(|_| Err(DbOperationError::Domain(MarshalError::IdInMap)));
    assert!(matches!(
        failure,
        Err(DbRuntimeError::Domain(MarshalError::IdInMap))
    ));
    assert_eq!(store.idle_count().unwrap(), 1, "lease returned after error");
    store
        .with_connection_sync(|conn| {
            assert_eq!(
                conn.query_row("SELECT value FROM item", [], |r| r.get::<_, String>(0))?,
                "committed"
            );
            Ok(())
        })
        .unwrap();
    fs::remove_file(path).unwrap();
}

#[test]
fn real_busy_retries_four_times_and_preserves_code_stage_and_delay_schedule() {
    let (path, mut holder, mut contender) = open_pair("busy");
    let held = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    write_item(&held, "holder").unwrap();
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::RefCell::new(Vec::new());
    let zero = |attempt| {
        delays.borrow_mut().push(attempt);
        Duration::ZERO
    };
    let result: Result<(), DbRuntimeError<MarshalError>> =
        try_with_tx_retry(&mut contender, &zero, |tx| {
            attempts.set(attempts.get() + 1);
            write_item(tx, "contender").map_err(DbOperationError::sqlite)
        });

    assert_eq!(attempts.get(), TX_MAX_RETRIES as usize + 1);
    assert_eq!(*delays.borrow(), vec![0, 1, 2]);
    assert_eq!(
        [
            production_delay(0),
            production_delay(1),
            production_delay(2)
        ],
        [
            Duration::from_secs(5),
            Duration::from_secs(10),
            Duration::from_secs(15)
        ]
    );
    match result {
        Err(DbRuntimeError::BusyExhausted {
            attempts,
            stage,
            source,
        }) => {
            assert_eq!(attempts, TX_MAX_RETRIES + 1);
            assert_eq!(stage, TransactionStage::Body);
            assert!(nd_db::store::is_busy(&source));
            assert!(matches!(
                source,
                SqliteError::SqliteFailure(ref code, _)
                    if code.extended_code & 0xff == rusqlite::ffi::SQLITE_BUSY
            ));
            assert!(source_has_rusqlite(
                &DbRuntimeError::<MarshalError>::BusyExhausted {
                    attempts,
                    stage,
                    source,
                }
            ));
        }
        other => panic!("expected typed BUSY exhaustion, got {other:?}"),
    }
    drop(held);
    fs::remove_file(path).unwrap();
}

#[test]
fn real_shared_cache_locked_is_not_retried() {
    let uri = format!(
        "file:nd-db-locked-{}?mode=memory&cache=shared",
        nd_id::new_random()
    );
    let mut holder = open(&uri).unwrap();
    let mut contender = open(&uri).unwrap();
    holder.busy_timeout(Duration::ZERO).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();
    holder
        .execute_batch("CREATE TABLE item(id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();
    let held = holder.transaction().unwrap();
    write_item(&held, "holder").unwrap();
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::Cell::new(0);
    let zero = |_| {
        delays.set(delays.get() + 1);
        Duration::ZERO
    };
    let result: Result<(), DbRuntimeError<MarshalError>> =
        try_with_tx_retry(&mut contender, &zero, |tx| {
            attempts.set(attempts.get() + 1);
            write_item(tx, "contender").map_err(DbOperationError::sqlite)
        });

    assert_eq!(attempts.get(), 1);
    assert_eq!(delays.get(), 0);
    match result {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            source,
        }) => {
            assert!(!nd_db::store::is_busy(&source));
            assert!(matches!(
                source,
                SqliteError::SqliteFailure(ref code, _)
                    if code.extended_code == rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE
            ));
            assert!(source_has_rusqlite(
                &DbRuntimeError::<MarshalError>::Sqlite {
                    stage: TransactionStage::Body,
                    source,
                }
            ));
        }
        other => panic!("expected one nonretryable SQLITE_LOCKED, got {other:?}"),
    }
    drop(held);
}

#[test]
fn domain_even_with_nested_busy_sqlite_source_is_not_retried() {
    let (path, mut conn, _) = open_pair("nested-domain");
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::Cell::new(0);
    let zero = |_| {
        delays.set(delays.get() + 1);
        Duration::ZERO
    };
    let result = try_with_tx_retry::<(), NestedBusyDomain>(&mut conn, &zero, |_| {
        attempts.set(attempts.get() + 1);
        let busy = SqliteError::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_string()),
        );
        Err(DbOperationError::Domain(NestedBusyDomain(busy)))
    });
    assert!(matches!(result, Err(DbRuntimeError::Domain(_))));
    assert_eq!(attempts.get(), 1);
    assert_eq!(delays.get(), 0);
    fs::remove_file(path).unwrap();
}

#[test]
fn domain_display_that_looks_busy_is_not_retried() {
    let (path, mut conn, _) = open_pair("busy-looking-domain");
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::Cell::new(0);
    let zero = |_| {
        delays.set(delays.get() + 1);
        Duration::ZERO
    };
    let result = try_with_tx_retry::<(), BusyLookingDomain>(&mut conn, &zero, |_| {
        attempts.set(attempts.get() + 1);
        Err(DbOperationError::Domain(BusyLookingDomain))
    });
    assert!(matches!(result, Err(DbRuntimeError::Domain(_))));
    assert_eq!(attempts.get(), 1);
    assert_eq!(delays.get(), 0);
    fs::remove_file(path).unwrap();
}

#[test]
fn callback_sqlite_stage_is_always_body() {
    let (path, mut conn, _) = open_pair("body-stage");
    let result: Result<(), DbRuntimeError<MarshalError>> = try_with_tx(&mut conn, |_| {
        Err(DbOperationError::sqlite(SqliteError::InvalidQuery))
    });
    assert!(matches!(
        result,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            source: SqliteError::InvalidQuery
        })
    ));
    fs::remove_file(path).unwrap();
}

#[test]
fn begin_busy_is_staged_without_running_callback() {
    let (path, mut holder, mut contender) = open_pair("begin");
    let held = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    write_item(&held, "holder").unwrap();
    contender.set_transaction_behavior(TransactionBehavior::Immediate);
    let called = std::cell::Cell::new(false);
    let result: Result<(), DbRuntimeError<MarshalError>> = try_with_tx(&mut contender, |_| {
        called.set(true);
        Ok(())
    });
    assert!(!called.get());
    match result {
        Err(DbRuntimeError::Sqlite { stage, source }) => {
            assert_eq!(stage, TransactionStage::Begin);
            assert!(matches!(
                source,
                SqliteError::SqliteFailure(ref code, _)
                    if code.extended_code & 0xff == rusqlite::ffi::SQLITE_BUSY
            ));
        }
        other => panic!("expected typed BEGIN BUSY, got {other:?}"),
    }
    drop(held);
    fs::remove_file(path).unwrap();
}

#[test]
fn commit_busy_reader_preserves_commit_stage_and_cleans_transaction() {
    let (path, mut writer, mut reader) = open_pair("commit");
    let reader_tx = reader.transaction().unwrap();
    reader_tx
        .query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
        .unwrap();
    let result: Result<(), DbRuntimeError<MarshalError>> = try_with_tx(&mut writer, |tx| {
        write_item(tx, "must not commit").map_err(DbOperationError::sqlite)
    });
    match result {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Commit,
            source,
        }) => {
            assert!(nd_db::store::is_busy(&source));
            assert!(
                writer.is_autocommit(),
                "failed commit was explicitly rolled back"
            );
            assert!(source_has_rusqlite(
                &DbRuntimeError::<MarshalError>::Sqlite {
                    stage: TransactionStage::Commit,
                    source,
                }
            ));
        }
        other => panic!("expected typed commit BUSY, got {other:?}"),
    }
    drop(reader_tx);
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    fs::remove_file(path).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn async_entrypoint_commits() {
    let path = db_path("async");
    let (pool, first) = Pool::new(&PoolConfig::with_size(path.clone(), 1)).unwrap();
    first
        .execute_batch("CREATE TABLE item(id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();
    pool.put_back(first).unwrap();
    let store = SQLStore::new(pool);
    store
        .try_with_tx::<_, MarshalError>(|tx| {
            write_item(tx, "async committed").map_err(DbOperationError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .try_with_tx_retry::<_, MarshalError>(|tx| {
            write_item(tx, "async retry committed").map_err(DbOperationError::sqlite)?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .with_connection(|conn| {
            assert_eq!(
                conn.query_row("SELECT value FROM item ORDER BY id DESC LIMIT 1", [], |r| r
                    .get::<_, String>(0),)?,
                "async retry committed"
            );
            Ok(())
        })
        .await
        .unwrap();
    fs::remove_file(path).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_async_waiter_does_not_cancel_started_work_or_lease_release() {
    let path = db_path("dropped-waiter");
    let (pool, first) = Pool::new(&PoolConfig::with_size(path.clone(), 1)).unwrap();
    first
        .execute_batch("CREATE TABLE item(id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();
    pool.put_back(first).unwrap();
    let store = SQLStore::new(pool);
    let worker_store = store.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let mut waiter = tokio::spawn(async move {
        worker_store
            .try_with_tx::<_, MarshalError>(move |tx| {
                entered_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
                write_item(tx, "completed after waiter dropped")
                    .map_err(DbOperationError::sqlite)?;
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    waiter.abort();
    let cancelled = tokio::time::timeout(Duration::from_secs(1), &mut waiter).await;
    let was_cancelled = matches!(cancelled, Ok(Err(error)) if error.is_cancelled());
    if !was_cancelled {
        continue_tx.send(()).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(1), &mut waiter).await;
        panic!("the outer waiter task must be cancelled before releasing the blocking callback");
    }
    continue_tx.send(()).unwrap();

    store
        .with_connection(|conn| {
            assert_eq!(
                conn.query_row("SELECT value FROM item", [], |r| r.get::<_, String>(0))?,
                "completed after waiter dropped"
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.idle_count().unwrap(), 1);
    fs::remove_file(path).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_task_join_error_remains_typed() {
    let path = db_path("join-error");
    let (pool, first) = Pool::new(&PoolConfig::with_size(path.clone(), 1)).unwrap();
    pool.put_back(first).unwrap();
    let store = SQLStore::new(pool);
    let result = store
        .try_with_tx::<(), MarshalError>(|_| panic!("intentional callback panic"))
        .await;
    match result {
        Err(DbRuntimeError::Pool(StoreError::Task(error))) => assert!(error.is_panic()),
        other => panic!("expected typed JoinError, got {other:?}"),
    }
    assert_eq!(store.idle_count().unwrap(), 1);
    fs::remove_file(path).unwrap();
}
