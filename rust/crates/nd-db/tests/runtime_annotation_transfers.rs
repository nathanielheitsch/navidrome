//! DB-15 runtime half: `ReassignAnnotation` EXECUTED through the DB-00 typed
//! transaction seam, plus the landed (`ab466928`) average-rating repair executed on
//! seeded tables.
//!
//! Go truth under test: `persistence/sql_annotations.go:185-197`
//! (`ReassignAnnotation`) — the empty/equal-id guard (`:186-188`), the
//! `update or ignore annotation set item_id = ? where item_type = ? and item_id = ?`
//! move (`:190-191`, whose comment at `:189` states that OR IGNORE keeps the target's
//! own row instead of aborting the statement), then `updateAvgRating` on the TARGET in
//! the same tx (`:195-196`); and `persistence/sql_annotations.go:151-160`
//! (`updateAvgRating`), whose SET carries the embedded
//! `coalesce((select round(avg(rating), 2) …), 0)` expression with three ordered binds
//! `[item_id, table, item_id]`.
//!
//! Every statement here comes from the production renderers and runs through
//! [`nd_db::sql_annotations::reassign_annotation_in_tx`] /
//! [`nd_db::sql_annotations::update_avg_rating_in_tx`] inside
//! [`nd_db::store::SQLStore::try_with_tx_sync`] (and commits through
//! `try_with_tx_retry_sync`), so parameter mismatches, rollback and retry are executed
//! facts rather than rendered ones. Fixtures are manual `INSERT`s only — no handwritten
//! replacement UPDATE stands in for the production write, except the two
//! explicitly-labelled non-vacuity controls.
//!
//! Go-side expectations were reproduced against the pinned statement text with the
//! repository's own SQLite driver (`github.com/mattn/go-sqlite3`, go.mod:40) in a
//! `/tmp` scratch program (local evidence, also EXECUTED here — not a
//! clone-reproducible oracle directory):
//!   plain move rows=2 / avg rows=1, target average 3.5, source average untouched;
//!   conflict (u1 annotated both ids): move rows=1, target keeps its own rating 2, the
//!   conflicting source row STAYS on prev_id, target average 3;
//!   `OR REPLACE` control: move rows=2, target average 4.5 and its own rating lost;
//!   absent source rows: move rows=0 with no error, target average reset 7.7 → 0;
//!   missing target row: avg rows=0 with no error;
//!   NULL+3+0 population → 3, NULL-only → 0, all-zero → 0, 1/2/4 → 2.33.
//!
//! No production caller is wired here (no scanner/BOOT/API consumer), and no other
//! crate or file is touched.

use std::convert::Infallible;
use std::error::Error;

use nd_db::execution::{execute, query_all, query_optional};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_annotations::{
    average_rating_expr_sql, reassign_annotation_in_tx, reassign_annotation_sql,
    update_avg_rating_in_tx, update_avg_rating_sql, AnnotationEntityKind, AnnotationWriteError,
};
use nd_db::sqlrepo::Value;
use nd_db::store::{try_with_tx, SQLStore};
use nd_db::{DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::params;
use rusqlite::Error as SqliteError;

const STAMP: &str = "2024-01-02 03:04:05+00:00";
const PREV: &str = "prev";
const NEXT: &str = "next";
const ALBUM: AnnotationEntityKind = AnnotationEntityKind::Album;
const MEDIA_FILE: AnnotationEntityKind = AnnotationEntityKind::MediaFile;

/// One isolated on-disk DB per invocation: unique temp path, freshly migrated, pooled,
/// `_foreign_keys=on` (Go's `db/db.go:55` DSN parameter) so `annotation`'s FK onto
/// `user` is really enforced — the rollback fixtures rely on that. Harness shape copied
/// from `tests/runtime_annotation_rating.rs:42-56`.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-annotation-transfers-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Run a block on one pooled lease. A SQL error surfaces as a panic with the raw
/// rusqlite cause — nothing here silently ignores a statement failure.
fn run<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> T {
    store
        .with_connection_sync(call)
        .unwrap_or_else(|e| panic!("store lease or statement failed: {e}"))
}

/// Same, but handing back the error for the negative controls instead of panicking.
fn try_run<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> Result<T, nd_db::store::StoreError> {
    store.with_connection_sync(call)
}

/// A real mid-transaction SQLite failure for rollback injection: `annotation.user_id`
/// references `user(id)` (`nd-dbschema/src/schema_ddl.rs:59`) and every store here is
/// opened with `_foreign_keys=on`, so a row for a non-existent user aborts the tx with
/// `SQLITE_CONSTRAINT_FOREIGNKEY`. This replaces a fabricated error value, so the
/// rollback under test is triggered by SQLite itself.
fn inject_fk_failure(tx: &rusqlite::Transaction<'_>) -> AnnotationWriteError {
    tx.execute(
        "INSERT INTO annotation(user_id,item_id,item_type) VALUES('ghost-user','poison','album')",
        [],
    )
    .err()
    .map(AnnotationWriteError::sqlite)
    .expect("FK violation on a missing user must fail")
}

/// Commit one production `reassign_annotation_in_tx` through the typed seam, exactly as
/// a caller would (Go wraps it in `WithTx`/`WithTxRetry`).
fn reassign_committed(store: &SQLStore, entity: AnnotationEntityKind, prev_id: &str, new_id: &str) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| {
            reassign_annotation_in_tx(tx, entity, prev_id, new_id)
        })
        .unwrap_or_else(|e| {
            panic!("reassign_annotation_in_tx({prev_id:?} → {new_id:?}) failed: {e}")
        });
}

/// Commit one production `update_avg_rating_in_tx` through the typed seam.
fn avg_committed(store: &SQLStore, entity: AnnotationEntityKind, id: &str) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| update_avg_rating_in_tx(tx, entity, id))
        .unwrap_or_else(|e| panic!("update_avg_rating_in_tx({entity:?}, {id:?}) failed: {e}"));
}

/// Affected-row counts of the two production legs, captured inside a transaction that
/// is then aborted by [`inject_fk_failure`] so pinning Go's counts costs no committed
/// state. Go reads `RowsAffected` (`sql_base_repository.go:341-344`) and then discards
/// the count, so these numbers are observable behavior of the statements, not part of
/// the ported API.
fn count_legs_rolled_back(
    store: &SQLStore,
    entity: AnnotationEntityKind,
    prev_id: &str,
    new_id: &str,
) -> (usize, usize) {
    let mut counts = (0usize, 0usize);
    let failure = store.try_with_tx_sync::<(), Infallible>(|tx| {
        counts.0 = execute(
            tx,
            &reassign_annotation_sql(entity.table(), prev_id, new_id),
        )
        .map_err(AnnotationWriteError::sqlite)?;
        counts.1 = execute(tx, &update_avg_rating_sql(entity.table(), new_id))
            .map_err(AnnotationWriteError::sqlite)?;
        Err(inject_fk_failure(tx))
    });
    assert!(matches!(failure, Err(DbRuntimeError::Sqlite { .. })));
    counts
}

fn seed_library(store: &SQLStore) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO library(id,name,path) VALUES(1,'lib','/lib')",
            [],
        )
    });
}

fn seed_user(store: &SQLStore, id: &str) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO user(id,user_name,created_at,updated_at) VALUES(?,?,?,?)",
            params![id, id, "2024-01-01T00:00:00Z", "2024-01-01T00:00:00Z"],
        )
    });
}

/// Seed one annotated entity of a registered kind, with a deliberately stale cached
/// average so "the cache was recalculated" cannot pass by accident.
fn seed_entity(store: &SQLStore, entity: AnnotationEntityKind, id: &str, stale_avg: f64) {
    let table = entity.table();
    run(store, |conn| match entity {
        AnnotationEntityKind::Album => conn.execute(
            &format!("INSERT INTO {table}(id,name,average_rating) VALUES(?,?,?)"),
            params![id, "a", stale_avg],
        ),
        AnnotationEntityKind::Artist => conn.execute(
            &format!("INSERT INTO {table}(id,name,average_rating) VALUES(?,?,?)"),
            params![id, "a", stale_avg],
        ),
        AnnotationEntityKind::MediaFile => conn.execute(
            &format!("INSERT INTO {table}(id,title,average_rating) VALUES(?,?,?)"),
            params![id, "t", stale_avg],
        ),
        AnnotationEntityKind::Playlist => conn.execute(
            &format!("INSERT INTO {table}(id,name,owner_id,average_rating) VALUES(?,?,?,?)"),
            params![id, "p", "u1", stale_avg],
        ),
    });
}

/// Insert one annotation row directly (fixture only; the production write under test is
/// the transfer, not an upsert). `rating` is bound as given, so `None` stores NULL.
fn seed_annotation(
    store: &SQLStore,
    user_id: &str,
    entity: AnnotationEntityKind,
    item_id: &str,
    rating: Option<i64>,
) {
    seed_full_annotation(store, user_id, entity, item_id, rating, false, 0);
}

/// Same, with the star/play-count payload a real annotation row carries, to prove the
/// move relocates the whole row rather than a rating column.
fn seed_full_annotation(
    store: &SQLStore,
    user_id: &str,
    entity: AnnotationEntityKind,
    item_id: &str,
    rating: Option<i64>,
    starred: bool,
    play_count: i64,
) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO annotation(user_id,item_id,item_type,rating,starred,play_count,play_date,rated_at) \
             VALUES(?,?,?,?,?,?,?,?)",
            params![
                user_id,
                item_id,
                entity.table(),
                rating,
                starred,
                play_count,
                STAMP,
                STAMP
            ],
        )
    });
}

fn read_avg(store: &SQLStore, entity: AnnotationEntityKind, id: &str) -> f64 {
    let statement = (
        format!("SELECT average_rating FROM {} WHERE id = ?", entity.table()),
        vec![Value::from(id)],
    );
    run(store, |conn| {
        query_optional(conn, &statement, |r| r.get::<_, f64>(0))
            .map(|v| v.expect("seeded row exists"))
    })
}

/// One annotation row, ordered by `(item_type, item_id, user_id)` so assertions are
/// deterministic. `rating` keeps NULL distinct from 0.
#[derive(Debug, PartialEq, Eq)]
struct Ann {
    user: String,
    item_id: String,
    item_type: String,
    rating: Option<i64>,
    starred: bool,
    play_count: i64,
}

fn read_annotations(store: &SQLStore) -> Vec<Ann> {
    run(store, |conn| {
        query_all(
            conn,
            &(
                "SELECT user_id, item_id, item_type, rating, starred, play_count \
                 FROM annotation ORDER BY item_type, item_id, user_id"
                    .to_string(),
                vec![],
            ),
            |r| {
                Ok(Ann {
                    user: r.get(0)?,
                    item_id: r.get(1)?,
                    item_type: r.get(2)?,
                    rating: r.get::<_, Option<i64>>(3)?,
                    starred: r.get::<_, bool>(4)?,
                    play_count: r.get::<_, i64>(5)?,
                })
            },
        )
    })
}

/// The mechanical gate DB-01 and DB-15's repair portion both failed: SQLite's own
/// placeholder count must equal the renderer's arg count on the real fresh schema.
fn assert_param_count(store: &SQLStore, sql: &str, args: &[Value]) -> usize {
    let rendered = sql.to_string();
    let arg_count = args.len();
    let placeholders = run(store, |conn| {
        conn.prepare(&rendered).map(|stmt| stmt.parameter_count())
    });
    assert_eq!(
        placeholders, arg_count,
        "placeholder/arg mismatch for {rendered}"
    );
    placeholders
}

/// Walk the source chain for the concrete `rusqlite::Error` and return SQLite's
/// extended result code (the DB-04 lane's `failure_code` helper, same idea).
fn extended_code(error: &(dyn Error + 'static)) -> Option<i32> {
    let mut node: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(e) = node {
        if let Some(SqliteError::SqliteFailure(code, _)) = e.downcast_ref::<SqliteError>() {
            return Some(code.extended_code);
        }
        node = e.source();
    }
    None
}

/// Walk the source chain of an error looking for a message substring.
fn chain_has(error: &(dyn Error + 'static), needle: &str) -> Vec<String> {
    let mut node: Option<&(dyn Error + 'static)> = Some(error);
    let mut messages = Vec::new();
    while let Some(e) = node {
        messages.push(e.to_string());
        node = e.source();
    }
    if messages
        .iter()
        .any(|m| m.to_lowercase().contains(needle.to_lowercase().as_str()))
    {
        messages
    } else {
        Vec::new()
    }
}

#[test]
fn avg_rating_embeds_subquery_and_binds_exactly() {
    // The ab466928 shape, executed rather than rendered: the aggregate expression is
    // embedded in the SET (Go's `Set("average_rating", Expr(…))`,
    // sql_annotations.go:154-157), so the statement carries three `?` for the three
    // args `[item_id, table, item_id]` (squirrel renders SET args before WHERE args).
    let (store, path) = fresh_store();
    seed_library(&store);
    seed_user(&store, "u1");
    seed_user(&store, "u2");
    seed_entity(&store, ALBUM, PREV, 9.9);
    seed_entity(&store, ALBUM, NEXT, 7.7);
    seed_annotation(&store, "u1", ALBUM, NEXT, Some(5));
    seed_annotation(&store, "u2", ALBUM, NEXT, Some(3));

    let (sql, args) = update_avg_rating_sql(ALBUM.table(), NEXT);
    assert!(
        sql.contains(&average_rating_expr_sql()),
        "the aggregate expression must be embedded, not bound"
    );
    assert_eq!(
        sql,
        "UPDATE album SET average_rating = coalesce((select round(avg(rating), 2) \
         from annotation where item_id = ? and item_type = ? and rating > 0), 0) WHERE id = ?"
    );
    assert_eq!(
        assert_param_count(&store, &sql, &args),
        3,
        "3 placeholders for 3 args"
    );
    assert_eq!(
        args,
        vec![
            Value::Text(NEXT.to_string()),
            Value::Text("album".to_string()),
            Value::Text(NEXT.to_string()),
        ]
    );
    // The update leg runs through the production tx function and reads back 4.0.
    avg_committed(&store, ALBUM, NEXT);
    assert_eq!(read_avg(&store, ALBUM, NEXT), 4.0);

    // Non-vacuity control (EXECUTED), derived from the renderer's own text: swapping
    // the embedded expression for a `?` reproduces the pre-fix two-placeholder shape,
    // and the SAME three args then fail SQLite's bind check. This is the check the
    // landed repair made mechanically possible.
    let mutated = sql.replace(&average_rating_expr_sql(), "?");
    assert_eq!(
        mutated, "UPDATE album SET average_rating = ? WHERE id = ?",
        "control must reproduce the pre-fix shape exactly"
    );
    assert_eq!(
        run(&store, |conn| conn
            .prepare(&mutated)
            .map(|s| s.parameter_count())),
        2
    );
    let err = try_run(&store, |conn| execute(conn, &(mutated, args.clone())))
        .expect_err("two-placeholder shape must not execute with three args");
    let cause = err.to_string();
    assert!(
        cause.contains("Wrong number of parameters passed to query. Got 3, needed 2"),
        "unexpected pre-fix error: {cause}"
    );

    // The transfer leg is gated the same way: 3 placeholders, 3 args, in Go's order
    // (new_id, item_type, prev_id).
    let (move_sql, move_args) = reassign_annotation_sql(ALBUM.table(), PREV, NEXT);
    assert_eq!(
        move_sql,
        "update or ignore annotation set item_id = ? where item_type = ? and item_id = ?"
    );
    assert_eq!(
        assert_param_count(&store, &move_sql, &move_args),
        3,
        "move leg must render 3 placeholders for 3 args"
    );
    assert_eq!(
        move_args,
        vec![
            Value::Text(NEXT.to_string()),
            Value::Text("album".to_string()),
            Value::Text(PREV.to_string()),
        ]
    );
    // Go oracle row counts for a plain move with NO target rows: 2 rows moved, 1 row
    // updated (executeSQL reads RowsAffected then discards it). Counted inside a tx that
    // is aborted, so the pin costs no committed state.
    seed_entity(&store, ALBUM, "src-count", 9.9);
    seed_entity(&store, ALBUM, "dst-count", 1.0);
    seed_annotation(&store, "u1", ALBUM, "src-count", Some(5));
    seed_annotation(&store, "u2", ALBUM, "src-count", Some(3));
    assert_eq!(
        count_legs_rolled_back(&store, ALBUM, "src-count", "dst-count"),
        (2, 1),
        "move 2 rows / avg 1 row, rolled back"
    );
    assert_eq!(
        read_avg(&store, ALBUM, NEXT),
        4.0,
        "control left nothing committed"
    );
    assert_eq!(
        read_avg(&store, ALBUM, "dst-count"),
        1.0,
        "the aborted count transaction did not touch the target cache"
    );
    // The Go conflict count: with a target row for the SAME user, OR IGNORE moves only
    // the non-conflicting row (u2), and the target is still recalculated.
    seed_entity(&store, ALBUM, "src-conf", 9.9);
    seed_entity(&store, ALBUM, "dst-conf", 2.0);
    seed_annotation(&store, "u1", ALBUM, "src-conf", Some(5));
    seed_annotation(&store, "u2", ALBUM, "src-conf", Some(4));
    seed_annotation(&store, "u1", ALBUM, "dst-conf", Some(2));
    assert_eq!(
        count_legs_rolled_back(&store, ALBUM, "src-conf", "dst-conf"),
        (1, 1),
        "Go oracle conflict case: 1 row moved (u2), u1's row ignored, target recalculated"
    );
    assert_eq!(
        read_annotations(&store)
            .iter()
            .filter(|r| r.item_type == "album")
            .count(),
        7,
        "no rows moved by the rolled-back count transactions"
    );

    // The registered enum is the only table input the runtime functions accept, and the
    // annotation `item_type` string is Go's table name, NOT the artwork kind prefix.
    assert_eq!(
        AnnotationEntityKind::ALL
            .iter()
            .map(|k| (*k, k.table()))
            .collect::<Vec<_>>(),
        vec![
            (AnnotationEntityKind::Album, "album"),
            (AnnotationEntityKind::Artist, "artist"),
            (AnnotationEntityKind::MediaFile, "media_file"),
            (AnnotationEntityKind::Playlist, "playlist"),
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn avg_rating_zero_positive_and_rounded_population() {
    // Zero/NULL/unrated rule and the rounding of the positive population, executed on
    // seeded tables through the tx function.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2", "u3"] {
        seed_user(&store, u);
    }
    let mf = MEDIA_FILE;
    seed_entity(&store, mf, "mf_round", 0.0);
    seed_entity(&store, mf, "mf_zero", 0.0);
    seed_entity(&store, mf, "mf_null", 5.5);
    seed_entity(&store, mf, "mf_null_only", 5.5);
    seed_entity(&store, mf, "mf_none", 4.5);
    seed_entity(&store, mf, "mf_other", 0.0);

    // 1, 2, 4 → round(avg,2) = 2.33 (Go's `round(avg(rating), 2)`, :155).
    seed_annotation(&store, "u1", mf, "mf_round", Some(1));
    seed_annotation(&store, "u2", mf, "mf_round", Some(2));
    seed_annotation(&store, "u3", mf, "mf_round", Some(4));
    // NULL + 3 + 0 → only the positive row counts → 3 (`NULL > 0` is NULL).
    seed_annotation(&store, "u1", mf, "mf_null", None);
    seed_annotation(&store, "u2", mf, "mf_null", Some(3));
    seed_annotation(&store, "u3", mf, "mf_null", Some(0));
    // NULL only → coalesce(…, 0) resets the stale 5.5 to 0, never NULL.
    seed_annotation(&store, "u1", mf, "mf_null_only", None);
    // All-zero population → 0, and the stale 4.5 cache is replaced.
    seed_annotation(&store, "u1", mf, "mf_none", Some(0));
    seed_annotation(&store, "u2", mf, "mf_none", Some(0));
    // Non-target with its own population, untouched by every call below.
    seed_annotation(&store, "u1", mf, "mf_other", Some(5));

    for id in [
        "mf_round",
        "mf_zero",
        "mf_null",
        "mf_null_only",
        "mf_none",
        "mf_other",
    ] {
        avg_committed(&store, mf, id);
    }
    assert_eq!(read_avg(&store, mf, "mf_round"), 2.33);
    assert_eq!(read_avg(&store, mf, "mf_zero"), 0.0);
    assert_eq!(read_avg(&store, mf, "mf_null"), 3.0);
    assert_eq!(read_avg(&store, mf, "mf_null_only"), 0.0);
    assert_eq!(read_avg(&store, mf, "mf_none"), 0.0);
    assert_eq!(read_avg(&store, mf, "mf_other"), 5.0);

    // Absent source rows + existing target with a stale cache: Go's update is
    // unconditional, so the cache is wiped to 0 rather than left alone.
    seed_entity(&store, mf, "mf_stale_only", 7.7);
    avg_committed(&store, mf, "mf_stale_only");
    assert_eq!(read_avg(&store, mf, "mf_stale_only"), 0.0);
    assert_eq!(
        count_legs_rolled_back(&store, mf, "mf_stale_only", "mf_other"),
        (0, 1),
        "no rows on the source id, target row still updated"
    );

    // Missing target row: successful no-op, 0 rows affected and no error — Go discards
    // the affected-row count in executeSQL (sql_base_repository.go:336-354).
    let (sql, args) = update_avg_rating_sql(mf.table(), "does-not-exist");
    assert_eq!(assert_param_count(&store, &sql, &args), 3);
    assert_eq!(
        try_run(&store, |conn| execute(conn, &(sql, args))).unwrap(),
        0
    );

    // Non-vacuity control: a numeric-literal average satisfies a literal expectation but
    // is NOT the value the production function produces. Writing the literal 4 for
    // mf_round proves the read-back channel reports whatever is stored, and the
    // production call then restores the aggregate value.
    run(&store, |conn| {
        conn.execute(
            "UPDATE media_file SET average_rating = 4 WHERE id = 'mf_round'",
            [],
        )
    });
    assert_eq!(read_avg(&store, mf, "mf_round"), 4.0);
    avg_committed(&store, mf, "mf_round");
    assert_eq!(
        read_avg(&store, mf, "mf_round"),
        2.33,
        "a literal average must not survive the production aggregate update"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn reassign_keeps_target_conflict_and_moves_other_users() {
    // Go's OR IGNORE (sql_annotations.go:189-191): the target keeps its OWN row for a
    // user who annotated both ids, the conflicting source row stays on prev_id (not
    // merged, not deleted), other users' rows move with their full payload, and only the
    // target's cached average is recalculated in the same tx.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2", "u3"] {
        seed_user(&store, u);
    }
    seed_entity(&store, ALBUM, PREV, 9.9);
    seed_entity(&store, ALBUM, NEXT, 2.0);
    // u1 annotated BOTH ids → conflict; u2/u3 only prev (u3 unrated, with star/playcount).
    seed_full_annotation(&store, "u1", ALBUM, PREV, Some(5), true, 3);
    seed_full_annotation(&store, "u1", ALBUM, NEXT, Some(2), false, 1);
    seed_full_annotation(&store, "u2", ALBUM, PREV, Some(4), false, 2);
    seed_full_annotation(&store, "u3", ALBUM, PREV, None, true, 7);
    let before = read_annotations(&store);

    // Go oracle counts for this fixture: move 2 rows (u2 and u3 — u3's NULL-rating
    // row is NOT ignored, it moves; u1's conflicting target annotation is ignored,
    // so its prev row stays), avg 1 row.
    assert_eq!(
        count_legs_rolled_back(&store, ALBUM, PREV, NEXT),
        (2, 1),
        "u2 and u3 move, u1's conflict ignored"
    );
    assert_eq!(
        read_annotations(&store),
        before,
        "the count transaction rolled back"
    );

    reassign_committed(&store, ALBUM, PREV, NEXT);

    let rows = read_annotations(&store);
    assert_eq!(
        rows.iter()
            .map(|r| (
                r.user.as_str(),
                r.item_id.as_str(),
                r.item_type.as_str(),
                r.rating
            ))
            .collect::<Vec<_>>(),
        vec![
            ("u1", NEXT, "album", Some(2)),
            ("u2", NEXT, "album", Some(4)),
            ("u3", NEXT, "album", None),
            ("u1", PREV, "album", Some(5)),
        ],
        "target conflict row kept, conflicting source row left on prev, others moved"
    );
    // The moved rows travel as whole rows: starred/play_count are not reset.
    let moved_u3 = rows
        .iter()
        .find(|r| r.user == "u3")
        .expect("u3 row moved to the target");
    assert!(moved_u3.starred, "starred travels with the row");
    assert_eq!(moved_u3.play_count, 7, "play_count travels with the row");
    let kept_u1 = rows
        .iter()
        .find(|r| r.user == "u1" && r.item_id == NEXT)
        .expect("u1 keeps its own target row");
    assert_eq!(
        kept_u1.rating,
        Some(2),
        "target's own rating wins over the source's 5"
    );

    // Target average = round(avg(2,4),2) = 3 — the NULL row and the ignored conflict are
    // excluded; prev's stale 9.9 is deliberately untouched (Go updates only newID).
    assert_eq!(read_avg(&store, ALBUM, NEXT), 3.0);
    assert_eq!(
        read_avg(&store, ALBUM, PREV),
        9.9,
        "Go updates only newID's cache"
    );

    // Non-vacuity control (EXECUTED): the merge shortcut (`update or replace`) overwrites
    // the target's own annotation, so it loses user data. Run against this fixture inside
    // a transaction that is then aborted, and read the effect from inside that tx — the
    // committed state above must stay Go's.
    let mut shortcut_rows: Vec<Ann> = Vec::new();
    let mut shortcut_avg = f64::NAN;
    let inside = store.try_with_tx_sync::<(), Infallible>(|tx| {
        tx.execute(
            "update or replace annotation set item_id = ? where item_type = ? and item_id = ?",
            params![NEXT, ALBUM.table(), PREV],
        )
        .map_err(AnnotationWriteError::sqlite)?;
        tx.execute(
            "UPDATE album SET average_rating = coalesce((select round(avg(rating), 2) \
             from annotation where item_id = ? and item_type = ? and rating > 0), 0) WHERE id = ?",
            params![NEXT, ALBUM.table(), NEXT],
        )
        .map_err(AnnotationWriteError::sqlite)?;
        shortcut_rows = query_all(
            tx,
            &(
                "SELECT user_id, item_id, item_type, rating, starred, play_count \
                 FROM annotation ORDER BY item_type, item_id, user_id"
                    .to_string(),
                vec![],
            ),
            |r| {
                Ok(Ann {
                    user: r.get(0)?,
                    item_id: r.get(1)?,
                    item_type: r.get(2)?,
                    rating: r.get::<_, Option<i64>>(3)?,
                    starred: r.get::<_, bool>(4)?,
                    play_count: r.get::<_, i64>(5)?,
                })
            },
        )
        .map_err(AnnotationWriteError::sqlite)?;
        shortcut_avg = tx
            .query_row(
                "SELECT average_rating FROM album WHERE id = ?",
                params![NEXT],
                |r| r.get::<_, f64>(0),
            )
            .map_err(AnnotationWriteError::sqlite)?;
        Err(inject_fk_failure(tx))
    });
    assert!(matches!(
        inside,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ..
        })
    ));
    assert_eq!(shortcut_rows.len(), 3, "shortcut read-back inside the tx");
    // The shortcut deleted u1's own target row (rating 2) and rewrote the source row onto
    // the target with rating 5 — 3 rows total, prev left empty, average 4.5. A lost
    // annotation AND a different average: exactly what OR IGNORE must not look like.
    assert_eq!(
        shortcut_rows
            .iter()
            .filter(|r| r.item_id == NEXT)
            .map(|r| (r.user.as_str(), r.rating))
            .collect::<Vec<_>>(),
        vec![("u1", Some(5)), ("u2", Some(4)), ("u3", None)],
        "OR REPLACE overwrote the target's own annotation"
    );
    assert_eq!(
        shortcut_avg, 4.5,
        "the merge shortcut's average is not OR IGNORE's 3.0"
    );
    assert_eq!(
        read_annotations(&store),
        rows,
        "OR REPLACE control must leave no committed trace"
    );
    assert_eq!(
        read_avg(&store, ALBUM, NEXT),
        3.0,
        "control rolled the cache back to the committed OR IGNORE value (shortcut would be 4.5)"
    );

    // Re-committing through the production function reproduces Go's result exactly, and
    // a rerun is idempotent (no double counting).
    reassign_committed(&store, ALBUM, PREV, NEXT);
    assert_eq!(read_avg(&store, ALBUM, NEXT), 3.0);
    reassign_committed(&store, ALBUM, PREV, NEXT);
    assert_eq!(read_avg(&store, ALBUM, NEXT), 3.0, "rerun converges");
    std::fs::remove_file(&path).ok();
}

#[test]
fn reassign_empty_equal_ids_noop() {
    // Go's guard (`sql_annotations.go:186-188`) returns success WITHOUT executing either
    // statement, so neither the rows nor either cached average may move. The stale caches
    // here are non-zero and the populations are real, so a variant that short-circuits
    // only the move (or that always runs the average) fails.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2"] {
        seed_user(&store, u);
    }
    seed_entity(&store, ALBUM, PREV, 9.9);
    seed_entity(&store, ALBUM, NEXT, 7.7);
    seed_annotation(&store, "u1", ALBUM, PREV, Some(5));
    seed_annotation(&store, "u2", ALBUM, PREV, Some(2));
    seed_annotation(&store, "u1", ALBUM, NEXT, Some(3));
    let before = read_annotations(&store);

    for (prev, new) in [("", NEXT), (PREV, ""), ("", ""), (PREV, PREV), (NEXT, NEXT)] {
        let result = store.try_with_tx_sync::<(), Infallible>(|tx| {
            reassign_annotation_in_tx(tx, ALBUM, prev, new)
        });
        assert!(
            matches!(result, Ok(())),
            "Go returns nil for prev={prev:?} new={new:?}, got {result:?}"
        );
    }

    assert_eq!(read_annotations(&store), before, "no rows moved");
    assert_eq!(read_avg(&store, ALBUM, PREV), 9.9, "prev cache untouched");
    assert_eq!(read_avg(&store, ALBUM, NEXT), 7.7, "target cache untouched");

    // Contrast (EXECUTED, pins the guard's real effect): two distinct non-empty ids with
    // NO source rows still run the average leg and wipe the target's stale cache — Go
    // oracle: move 0 rows, avg 1 row.
    seed_entity(&store, ALBUM, "ghost-target", 6.6);
    assert_eq!(
        count_legs_rolled_back(&store, ALBUM, "ghost-source", "ghost-target"),
        (0, 1),
        "absent source rows do not skip the recalculation"
    );
    reassign_committed(&store, ALBUM, "ghost-source", "ghost-target");
    assert_eq!(read_avg(&store, ALBUM, "ghost-target"), 0.0);
    assert_eq!(
        read_avg(&store, ALBUM, PREV),
        9.9,
        "only the target is recalculated"
    );

    // A missing target row is likewise a successful no-op, not an error.
    let result = store
        .try_with_tx_sync::<(), Infallible>(|tx| update_avg_rating_in_tx(tx, ALBUM, "no-such-row"));
    assert!(matches!(result, Ok(())), "missing target row: {result:?}");

    // Type restriction: identical id strings under two item types are separate
    // populations, and the artwork kind prefix is not an annotation item_type. The album
    // rows above (PREV/NEXT under item_type 'album') share the id strings with the
    // media_file rows seeded here, so a media_file transfer must leave all of them alone.
    let mf = MEDIA_FILE;
    seed_entity(&store, mf, PREV, 1.1);
    seed_entity(&store, mf, NEXT, 2.2);
    seed_annotation(&store, "u1", mf, PREV, Some(7));
    seed_annotation(&store, "u2", mf, PREV, Some(1));
    // A row stored under the artwork prefix (`item_artwork.item_kind` spelling) must be
    // invisible to the media_file transfer, which binds the full table name.
    run(&store, |conn| {
        conn.execute(
            "INSERT INTO annotation(user_id,item_id,item_type,rating) VALUES('u1','prev','al',9)",
            [],
        )
    });

    reassign_committed(&store, mf, PREV, NEXT);
    let after = read_annotations(&store);
    assert_eq!(
        after
            .iter()
            .filter(|r| r.item_type == "media_file")
            .map(|r| (r.user.as_str(), r.item_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("u1", NEXT), ("u2", NEXT)],
        "only media_file rows moved"
    );
    assert_eq!(
        after
            .iter()
            .filter(|r| r.item_type != "media_file")
            .map(|r| (r.user.as_str(), r.item_id.as_str(), r.item_type.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("u1", PREV, "al"),
            ("u1", NEXT, "album"),
            ("u1", PREV, "album"),
            ("u2", PREV, "album"),
        ],
        "album rows and the 'al'-prefixed row stayed put (album ≠ artwork prefix)"
    );
    assert_eq!(read_avg(&store, mf, NEXT), 4.0, "media_file target average");
    assert_eq!(
        read_avg(&store, ALBUM, NEXT),
        7.7,
        "album target average untouched"
    );
    assert_eq!(read_avg(&store, mf, PREV), 1.1, "media_file prev untouched");
    std::fs::remove_file(&path).ok();
}

#[test]
fn reassign_failure_rolls_back_rows_and_cache() {
    // Failure injection AFTER the move and the recalculation: the typed seam (DB-00) must
    // roll the moved rows and the recalculated cache back, and hand back the concrete
    // SQLite cause staged as `Body`. Consumes try_with_tx / try_with_tx_sync /
    // try_with_tx_retry_sync, not a manual SQL bundle, so the rollback contract under test
    // is the production one.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2"] {
        seed_user(&store, u);
    }
    seed_entity(&store, ALBUM, PREV, 9.9);
    seed_entity(&store, ALBUM, NEXT, 2.0);
    seed_annotation(&store, "u1", ALBUM, PREV, Some(5));
    seed_annotation(&store, "u2", ALBUM, PREV, Some(4));
    seed_annotation(&store, "u1", ALBUM, NEXT, Some(2));
    let before = read_annotations(&store);
    assert_eq!(read_avg(&store, ALBUM, NEXT), 2.0);

    let failure = store.try_with_tx_sync::<(), Infallible>(|tx| {
        reassign_annotation_in_tx(tx, ALBUM, PREV, NEXT)?;
        Err(inject_fk_failure(tx))
    });
    let err = match failure {
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            source,
        }) => source,
        other => panic!("expected a Body-staged SQLite failure, got {other:?}"),
    };
    assert_eq!(
        extended_code(&err),
        Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        "the injected FK violation is the classified cause"
    );
    let messages = chain_has(&err, "foreign key");
    assert!(
        !messages.is_empty(),
        "concrete rusqlite cause stays reachable through source(): {err:?}"
    );
    assert_eq!(read_annotations(&store), before, "moved rows rolled back");
    assert_eq!(
        read_avg(&store, ALBUM, NEXT),
        2.0,
        "recalculated cache rolled back"
    );
    assert_eq!(read_avg(&store, ALBUM, PREV), 9.9, "prev cache unchanged");
    // The failed tx did not poison the pool: both handles are idle and in autocommit.
    assert_eq!(store.idle_count().unwrap(), 2, "the lease was returned");
    store
        .with_connection_sync(|conn| {
            assert!(
                conn.is_autocommit(),
                "no transaction left open on the handle"
            );
            Ok(())
        })
        .unwrap();

    // Same injection through the free-function seam on a plain connection (Go's `WithTx`).
    let mut conn = rusqlite::Connection::open(format!("file:{}?mode=rwc", path.display()))
        .expect("re-open the same file");
    conn.pragma_update(None, "foreign_keys", "on").unwrap();
    let result = try_with_tx::<(), Infallible>(&mut conn, |tx| {
        reassign_annotation_in_tx(tx, ALBUM, PREV, NEXT)?;
        Err(inject_fk_failure(tx))
    });
    assert!(matches!(
        result,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ..
        })
    ));
    assert_eq!(
        read_annotations(&store),
        before,
        "free-function seam rolled back too"
    );

    // Commit path through the retry seam, then a second attempt over the same ids: the
    // transfer converges, which is what makes Go's WithTxRetry rerun safe. Busy/retry
    // classification itself is covered by runtime_typed_transactions.rs and
    // busy_retry_mapping.rs (DB-00), so no busy injection is duplicated here.
    store
        .try_with_tx_retry_sync::<(), Infallible>(|tx| {
            reassign_annotation_in_tx(tx, ALBUM, PREV, NEXT)
        })
        .unwrap_or_else(|e| panic!("retryable commit failed: {e}"));
    assert_eq!(
        read_annotations(&store)
            .iter()
            .map(|r| (r.user.as_str(), r.item_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("u1", NEXT), ("u2", NEXT), ("u1", PREV)],
        "committed transfer: conflict row left on prev, others moved"
    );
    assert_eq!(read_avg(&store, ALBUM, NEXT), 3.0);
    assert_eq!(read_avg(&store, ALBUM, PREV), 9.9);
    store
        .try_with_tx_retry_sync::<(), Infallible>(|tx| {
            reassign_annotation_in_tx(tx, ALBUM, PREV, NEXT)
        })
        .unwrap_or_else(|e| panic!("second attempt failed: {e}"));
    assert_eq!(
        read_avg(&store, ALBUM, NEXT),
        3.0,
        "rerun is idempotent, no double counting"
    );

    // `AnnotationWriteError`'s Domain arm is uninhabited (Infallible), so a domain failure
    // can only come from a caller wrapping its own error type around the same tx.
    let plain = store.try_with_tx_sync::<(), Infallible>(|tx| {
        reassign_annotation_in_tx(tx, ALBUM, PREV, NEXT).map_err(|e| match e {
            DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
            DbOperationError::Domain(infallible) => match infallible {},
        })?;
        Ok(())
    });
    assert!(matches!(plain, Ok(())), "Infallible Domain arm: {plain:?}");

    // Every registered kind transfers on the real schema, and a reassign restricted to one
    // kind leaves the other kinds' rows and caches alone.
    let (store2, path2) = fresh_store();
    seed_library(&store2);
    seed_user(&store2, "u1");
    for kind in AnnotationEntityKind::ALL {
        seed_entity(&store2, kind, PREV, 9.9);
        seed_entity(&store2, kind, NEXT, 1.0);
        seed_annotation(&store2, "u1", kind, PREV, Some(6));
    }
    for kind in AnnotationEntityKind::ALL {
        reassign_committed(&store2, kind, PREV, NEXT);
        assert_eq!(read_avg(&store2, kind, NEXT), 6.0, "{kind:?} target");
        assert_eq!(read_avg(&store2, kind, PREV), 9.9, "{kind:?} prev");
    }
    let all = read_annotations(&store2);
    assert_eq!(all.len(), 4, "one row per kind");
    assert!(
        all.iter().all(|r| r.item_id == NEXT),
        "every kind's row carries the target id"
    );
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&path2).ok();
}
