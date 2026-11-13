//! Executed regression tests for statement binding and the store boundary.
//!
//! Port targets: `sql_base_repository.go:358-372` (`toSQL` output actually run
//! against SQLite) and `persistence.go:165-228` (`WithTx`/`WithTxRetry` reached
//! from an async task without blocking the executor).

use nd_db::execution::{execute, query_all, query_optional};
use nd_db::library_repo;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::{Repository, Value};
use nd_db::store::SQLStore;
use nd_db::user_repo::{
    find_by_username_sql, get_all_users_sql, get_user_sql, select_user_with_libraries,
};
use nd_dbschema::schema::apply_fresh_schema;

fn store() -> SQLStore {
    // Named memory DBs are isolated per invocation and disappear with the pool.
    let path = format!(
        "file:runtime-{}?mode=memory&cache=shared",
        nd_id::new_random()
    );
    let pool = Pool::new(&PoolConfig::with_size(path, 1)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    SQLStore::new(pool)
}

#[test]
fn user_where_before_group_by_executes_with_bound_adversarial_username() {
    let store = store();
    store
        .with_connection_sync(|conn| {
            // user.created_at/updated_at are NOT NULL (nd-dbschema DDL), so the
            // fixture row carries stamps like Go's Put always does.
            conn.execute(
                "INSERT INTO user(id,user_name,created_at,updated_at) VALUES('u', ?, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
                ["x' OR 1=1 --"],
            )?;
            let id = query_optional(conn, &get_user_sql("u"), |r| r.get::<_, String>("id"))?;
            assert_eq!(id.as_deref(), Some("u"));
            let libs = query_optional(conn, &get_user_sql("u"), |r| {
                r.get::<_, String>("libraries_json")
            })?;
            assert_eq!(libs.as_deref(), Some("[]"));
            assert!(query_optional(conn, &get_user_sql("' OR 1=1 --"), |_| Ok(()))?.is_none());
            assert!(
                query_optional(conn, &find_by_username_sql("X' OR 1=1 --"), |_| Ok(()))?.is_some()
            );
            let page = select_user_with_libraries(None, vec![], &["user.id"], Some(1), Some(1));
            assert!(query_all(conn, &page, |_| Ok(()))?.is_empty());
            // Offset-only page: squirrel's bare `OFFSET n` is a SQLite parse
            // error; the port's `LIMIT -1 OFFSET n` renders and executes (the
            // cited deviation on `select_user_with_libraries`). One row exists,
            // so offset 0 keeps it and offset 1 pages past it.
            let first_page = get_all_users_sql(&["user.id"], None, Some(0));
            assert_eq!(query_all(conn, &first_page, |_| Ok(()))?.len(), 1);
            let past_last = get_all_users_sql(&["user.id"], None, Some(1));
            assert_eq!(query_all(conn, &past_last, |_| Ok(()))?.len(), 0);
            let values = (
                "SELECT ?, ?, ?, ?, ?".into(),
                vec![
                    Value::from("text"),
                    Value::Integer(7),
                    Value::Real(1.5),
                    Value::Bool(true),
                    Value::Null,
                ],
            );
            assert_eq!(
                query_optional(conn, &values, |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, Option<String>>(4)?
                )))?,
                Some(("text".into(), 7, 1.5, true, None))
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn literal_pagination_bounds_execute_against_sqlite() {
    // Milestone-3 golden: squirrel renders LIMIT/OFFSET as decimal literals
    // (`select.go:139-147`), and SQLite rejects a bare `OFFSET n`. Every page
    // shape the port now emits must actually run, not just render.
    let store = store();
    store
        .with_connection_sync(|conn| {
            for id in 1..=5 {
                conn.execute(
                    "INSERT INTO library(id, name, path) VALUES(?, ?, ?)",
                    rusqlite::params![id, format!("lib-{id}"), format!("/r/{id}")],
                )?;
            }
            // Builder path: LIMIT + OFFSET as literals, no bound args.
            let (sql, args) = Repository::new("library")
                .select(["id"])
                .order_by(["id"])
                .limit(2)
                .offset(2)
                .to_sql()
                .expect("pagination renders");
            assert_eq!(sql, "SELECT id FROM library ORDER BY id LIMIT 2 OFFSET 2");
            assert!(args.is_empty());
            assert_eq!(
                query_all(conn, &(sql, args), |r| r.get::<_, i64>(0))?,
                vec![3, 4]
            );

            // Offset-only page through the builder: the port's `LIMIT -1`
            // deviation is what makes this execute at all.
            let (sql, args) = Repository::new("library")
                .select(["id"])
                .order_by(["id"])
                .offset(3)
                .to_sql()
                .expect("offset renders");
            assert_eq!(sql, "SELECT id FROM library ORDER BY id LIMIT -1 OFFSET 3");
            assert_eq!(
                query_all(conn, &(sql, args), |r| r.get::<_, i64>(0))?,
                vec![4, 5]
            );

            // squirrel's own shape, for contrast: a bare OFFSET is a parse error.
            let bare = "SELECT id FROM library ORDER BY id OFFSET 3";
            assert!(conn.prepare(bare).is_err(), "bare OFFSET must not prepare");

            // Repository-level pages reached through the real SQL builders.
            let (sql, args) = library_repo::get_all_sql(&["name asc"], 2, 1);
            assert_eq!(
                sql,
                "SELECT * FROM library ORDER BY name asc LIMIT 2 OFFSET 1"
            );
            assert!(args.is_empty());
            assert_eq!(query_all(conn, &(sql, args), |_| Ok(()))?.len(), 2);

            // Offset without a limit at a hand-rolled site: executes via `LIMIT -1`.
            let (sql, args) = library_repo::get_all_sql(&["name asc"], 0, 2);
            assert_eq!(
                sql,
                "SELECT * FROM library ORDER BY name asc LIMIT -1 OFFSET 2"
            );
            assert_eq!(query_all(conn, &(sql, args), |_| Ok(()))?.len(), 3);

            // A page beyond the last row is an empty result, not an error.
            let (sql, args) = library_repo::get_all_sql(&["name asc"], 10, 50);
            assert!(query_all(conn, &(sql, args), |_| Ok(()))?.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn store_transaction_commits_and_rolls_back_without_executor_blocking() {
    let store = store();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let busy_store = store.clone();
    let busy = tokio::spawn(async move {
        busy_store
            .with_connection(move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    // With the only lease held, a second async checkout must not freeze this
    // current-thread runtime. The timer can release the first connection.
    let waiting_store = store.clone();
    let waiting = tokio::spawn(async move { waiting_store.with_connection(|_| Ok(())).await });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_tx.send(()).unwrap();
    busy.await.unwrap().unwrap();
    waiting.await.unwrap().unwrap();
    store
        .with_tx(|tx| {
            execute(
                tx,
                &(
                    "INSERT INTO property(id,value) VALUES(?,?)".into(),
                    vec![Value::from("a"), Value::from("yes")],
                ),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let failed = store
        .with_tx(|tx| {
            tx.execute("INSERT INTO property(id,value) VALUES('b','no')", [])?;
            tx.execute("INSERT INTO property(id,value) VALUES('a','duplicate')", [])
        })
        .await;
    assert!(failed.is_err());
    store
        .with_connection(|conn| {
            assert_eq!(
                conn.query_row("SELECT value FROM property WHERE id='a'", [], |r| r
                    .get::<_, String>(0))?,
                "yes"
            );
            assert_eq!(
                conn.query_row("SELECT count(*) FROM property WHERE id='b'", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
}
