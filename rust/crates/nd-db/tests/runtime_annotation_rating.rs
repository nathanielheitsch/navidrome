//! DB-15: `update_avg_rating_sql` EXECUTED against the real fresh schema.
//!
//! Go truth under test: `persistence/sql_annotations.go:151-160`
//! (`updateAvgRating`), which renders
//! `Update(table).Where(Eq{"id": itemID}).Set("average_rating", Expr("coalesce((select
//! round(avg(rating), 2) from annotation where item_id = ? and item_type = ? and
//! rating > 0), 0)", itemID, tableName))`. squirrel puts the `Expr` text *inside* the
//! SET and renders SET args before the WHERE arg, so the materialized statement
//! (`sql_base_repository.go:358-372` `toSQL()`) has three `?` and args
//! `[itemID, tableName, itemID]`.
//!
//! Every statement here comes from the production renderer and runs through
//! `nd_db::execution::execute`; fixtures are manual `INSERT`s only — no handwritten
//! replacement UPDATE. Scope is the render/execute repair plus the documented
//! annotation-read seam, NOT the annotation service: no `SetStar`/`SetRating`/
//! `IncPlayCount` upsert flow and no `ReassignAnnotation` transaction wrapper (the
//! rest of DB-15 stays blocked on DB-00/03).
//!
//! Pinned deliberately: `annotation` has **no `id` column**
//! (`nd-dbschema/src/schema_ddl.rs:59`, PK `unique(user_id, item_id, item_type)`),
//! so `annotation_count_sql`'s `count(distinct annotation.id)` cannot be prepared.

use nd_db::execution::{execute, query_optional};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_annotations::{
    ann_id, annotation_count_sql, annotation_exists_sql, average_rating_expr_sql,
    get_annotations_sql, update_avg_rating_sql,
};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::params;

const STAMP: &str = "2024-01-02 03:04:05+00:00";
const MEDIA_FILE: &str = "media_file";
const ALBUM: &str = "album";

/// One isolated on-disk DB per invocation: unique temp path, freshly migrated,
/// pooled, `_foreign_keys=on` (Go's `db/db.go:55` DSN parameter) so `annotation`'s
/// FK onto `user` is really enforced. Harness shape copied from
/// `tests/runtime_missing_state.rs:68-95`.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-annotation-rating-",
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

/// Same, but handing back the error for the negative cases (the pre-fix shape and
/// the non-preparable count renderer) instead of panicking.
fn try_run<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> Result<T, nd_db::store::StoreError> {
    store.with_connection_sync(call)
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

/// Manual fixture INSERTs only: no repository write path is in scope for DB-15's
/// render-only repair.
fn seed_media_file(store: &SQLStore, id: &str) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO media_file(id,title) VALUES(?,?)",
            params![id, "t"],
        )
    });
}

fn seed_album(store: &SQLStore, id: &str) {
    run(store, |conn| {
        conn.execute("INSERT INTO album(id,name) VALUES(?,?)", params![id, "a"])
    });
}

/// One annotation row. `item_type` is the repository table name Go binds
/// (`sql_annotations.go:155`), so `album` and `media_file` never collide.
fn seed_annotation(store: &SQLStore, user_id: &str, item_type: &str, item_id: &str, rating: i64) {
    run(store, |conn| {
        conn.execute(
            &format!(
                "INSERT INTO annotation(user_id,item_id,item_type,rating,rated_at) \
                 VALUES(?,?,?,?,'{STAMP}')"
            ),
            params![user_id, item_id, item_type, rating],
        )
    });
}

fn read_avg(store: &SQLStore, table: &str, id: &str) -> f64 {
    let statement = (
        format!("SELECT average_rating FROM {table} WHERE id = ?"),
        vec![Value::from(id)],
    );
    run(store, |conn| {
        query_optional(conn, &statement, |r| r.get::<_, f64>(0)).map(|v| v.expect("row exists"))
    })
}

/// The mechanical gate the pre-fix renderer failed: the rendered statement's
/// SQLite placeholder count must equal the renderer's arg count.
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

/// Render → param-count check → execute through the production renderer and
/// `nd_db::execution::execute`, returning rows affected.
fn render_and_execute(store: &SQLStore, table: &str, id: &str) -> usize {
    let (sql, args) = update_avg_rating_sql(table, id);
    assert_eq!(
        assert_param_count(store, &sql, &args),
        3,
        "{table}/{id} must render 3 placeholders for 3 args"
    );
    run(store, |conn| execute(conn, &(sql, args)))
}

#[test]
fn update_avg_rating_executes_and_excludes_zero_ratings_across_all_users() {
    // Go: ratings 5, 3 and 0 from three different users → `rating > 0` drops the
    // zero and round(avg(5,3),2) = 4. There is deliberately no user_id predicate
    // (`sql_annotations.go:155-156`), so the average spans every user's rating.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2", "u3"] {
        seed_user(&store, u);
    }
    seed_media_file(&store, "mf1");
    seed_annotation(&store, "u1", MEDIA_FILE, "mf1", 5);
    seed_annotation(&store, "u2", MEDIA_FILE, "mf1", 3);
    seed_annotation(&store, "u3", MEDIA_FILE, "mf1", 0);

    let (sql, args) = update_avg_rating_sql(MEDIA_FILE, "mf1");
    assert_eq!(assert_param_count(&store, &sql, &args), 3);
    assert_eq!(
        run(&store, |conn| execute(conn, &(sql.clone(), args.clone()))),
        1
    );
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf1"), 4.0);

    // The expression text is embedded rather than bound, and the arg list is
    // Go's order: subquery (item_id, item_type) then the outer WHERE id.
    assert!(sql.contains(&average_rating_expr_sql()));
    assert_eq!(
        args,
        vec![
            Value::Text("mf1".to_string()),
            Value::Text(MEDIA_FILE.to_string()),
            Value::Text("mf1".to_string())
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn update_avg_rating_isolates_item_id_and_item_type() {
    // The same id string under two item types must not cross-contaminate (Go binds
    // item_type as a value, `:155`, never interpolates it), and a non-target row's
    // cached average must not move.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2", "u3"] {
        seed_user(&store, u);
    }
    seed_media_file(&store, "mf1");
    seed_media_file(&store, "mf2");
    seed_album(&store, "mf1");
    seed_annotation(&store, "u1", MEDIA_FILE, "mf1", 5);
    seed_annotation(&store, "u2", MEDIA_FILE, "mf1", 3);
    // album 'mf1' has its own population, including one unrated row. annotation's
    // PK is unique(user_id,item_id,item_type), so the zero needs a third user.
    seed_annotation(&store, "u1", ALBUM, "mf1", 1);
    seed_annotation(&store, "u2", ALBUM, "mf1", 5);
    seed_annotation(&store, "u3", ALBUM, "mf1", 0);
    // A different media_file with its own rating, untouched by the mf1 update.
    seed_annotation(&store, "u1", MEDIA_FILE, "mf2", 2);

    assert_eq!(render_and_execute(&store, MEDIA_FILE, "mf1"), 1);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf1"), 4.0);
    assert_eq!(read_avg(&store, ALBUM, "mf1"), 0.0, "album leg untouched");
    assert_eq!(
        read_avg(&store, MEDIA_FILE, "mf2"),
        0.0,
        "non-target media_file untouched"
    );

    // The album leg with the same id string yields the album's own average
    // (round(avg(1,5),2) = 3.0; the 0 row is excluded).
    assert_eq!(render_and_execute(&store, ALBUM, "mf1"), 1);
    assert_eq!(read_avg(&store, ALBUM, "mf1"), 3.0);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf1"), 4.0);

    // mf2 by its own renderer call: still its own single rating.
    assert_eq!(render_and_execute(&store, MEDIA_FILE, "mf2"), 1);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf2"), 2.0);
    assert_eq!(read_avg(&store, ALBUM, "mf1"), 3.0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn update_avg_rating_coalesces_no_positive_ratings_to_zero() {
    // Go's `coalesce(…, 0)` (`:155`) is the unrated rule, and the column is
    // `REAL NOT NULL DEFAULT 0`, so the executed value is 0, never NULL: an item
    // with no annotation rows at all, an item whose only rating is 0, and an item
    // whose cached average is stale (seeded non-zero) while its only ratings are 0.
    let (store, path) = fresh_store();
    seed_library(&store);
    seed_user(&store, "u1");
    seed_user(&store, "u2");
    seed_media_file(&store, "mf_none");
    seed_media_file(&store, "mf_zero");
    run(&store, |conn| {
        conn.execute(
            "INSERT INTO media_file(id,title,average_rating) VALUES(?, 't', 4.5)",
            params!["mf_stale"],
        )
    });
    seed_annotation(&store, "u1", MEDIA_FILE, "mf_zero", 0);
    seed_annotation(&store, "u1", MEDIA_FILE, "mf_stale", 0);
    seed_annotation(&store, "u2", MEDIA_FILE, "mf_stale", 0);

    for id in ["mf_none", "mf_zero", "mf_stale"] {
        assert_eq!(render_and_execute(&store, MEDIA_FILE, id), 1, "{id}");
    }
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf_none"), 0.0);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf_zero"), 0.0);
    // The seeded 4.5 stale cache is replaced by the coalesced 0, not left alone
    // and not written as NULL.
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf_stale"), 0.0);

    // Nonexistent id: the statement prepares and executes with 0 rows and no
    // error, which is Go's behavior for updateAvgRating on a missing row.
    let (sql, args) = update_avg_rating_sql(MEDIA_FILE, "does-not-exist");
    assert_eq!(assert_param_count(&store, &sql, &args), 3);
    assert_eq!(
        try_run(&store, |conn| execute(conn, &(sql, args))).unwrap(),
        0
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn update_avg_rating_rounds_to_two_decimals_of_the_positive_population() {
    // round(avg(rating), 2) (`:155`), over all users' positive ratings only.
    let (store, path) = fresh_store();
    seed_library(&store);
    for u in ["u1", "u2", "u3"] {
        seed_user(&store, u);
    }
    seed_media_file(&store, "mf1");
    seed_media_file(&store, "mf2");
    seed_album(&store, "a1");
    // 1, 2, 4 → avg 2.333… → 2.33
    seed_annotation(&store, "u1", MEDIA_FILE, "mf1", 1);
    seed_annotation(&store, "u2", MEDIA_FILE, "mf1", 2);
    seed_annotation(&store, "u3", MEDIA_FILE, "mf1", 4);
    // 4, 5 → 4.5 (the parent's executed reproduction value)
    seed_annotation(&store, "u1", ALBUM, "a1", 4);
    seed_annotation(&store, "u2", ALBUM, "a1", 5);
    seed_annotation(&store, "u3", ALBUM, "a1", 0);
    // Untouched non-target with its own population.
    seed_annotation(&store, "u1", MEDIA_FILE, "mf2", 5);

    assert_eq!(render_and_execute(&store, MEDIA_FILE, "mf1"), 1);
    assert_eq!(render_and_execute(&store, ALBUM, "a1"), 1);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf1"), 2.33);
    assert_eq!(read_avg(&store, ALBUM, "a1"), 4.5);
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf2"), 0.0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn pre_fix_two_placeholder_shape_cannot_execute_with_three_args() {
    // Non-vacuity control derived from the renderer's own output rather than a
    // handwritten statement: replacing the embedded expression with a `?`
    // reproduces the pre-DB-15 shape, and the SAME three args then fail to bind
    // (`InvalidParameterCount`). That is why the repair had to embed the
    // expression, and why a renderer assertion alone was never enough.
    let (store, path) = fresh_store();
    seed_library(&store);
    seed_user(&store, "u1");
    seed_media_file(&store, "mf1");
    seed_annotation(&store, "u1", MEDIA_FILE, "mf1", 5);

    let (sql, args) = update_avg_rating_sql(MEDIA_FILE, "mf1");
    let mutated = sql.replace(&average_rating_expr_sql(), "?");
    assert_eq!(
        mutated, "UPDATE media_file SET average_rating = ? WHERE id = ?",
        "mutation must reproduce the pre-fix shape exactly"
    );
    let placeholders = run(&store, |conn| {
        conn.prepare(&mutated).map(|stmt| stmt.parameter_count())
    });
    assert_eq!(placeholders, 2);
    let err = try_run(&store, |conn| execute(conn, &(mutated, args)))
        .expect_err("pre-fix shape must not execute with 3 args against 2 placeholders");
    let cause = err.to_string();
    assert!(
        cause.contains("Wrong number of parameters passed to query. Got 3, needed 2"),
        "unexpected error: {cause}"
    );
    // The failed statement left the cached average at its DDL default.
    assert_eq!(read_avg(&store, MEDIA_FILE, "mf1"), 0.0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn annotation_count_renderer_fails_to_prepare_without_id_column() {
    // Precise scope: of the three standalone annotation SELECT renderers only
    // `annotation_count_sql` is non-executable. `annotation` has no `id` column
    // (`nd-dbschema/src/schema_ddl.rs:59`; the PK is
    // `unique(user_id, item_id, item_type)`), so squirrel's generic
    // `count(distinct annotation.id)` — the shape `sqlRepository.count()`
    // (`sql_base_repository.go:565-575`) would produce — cannot be prepared. Go has
    // no annotation repository (`persistence/persistence.go`), so these three
    // renderers are an invented seam; the failure is pinned here instead of shipped
    // silently, while the other two prepare and execute normally.
    let (store, path) = fresh_store();
    seed_library(&store);
    seed_user(&store, "u1");
    seed_media_file(&store, "mf1");
    seed_annotation(&store, "u1", MEDIA_FILE, "mf1", 5);

    let (get_sql, get_args) = get_annotations_sql(MEDIA_FILE, "u1", &["mf1", "mf2"]);
    assert_eq!(assert_param_count(&store, &get_sql, &get_args), 4);
    assert_eq!(
        run(&store, |conn| {
            query_optional(conn, &(get_sql.clone(), get_args.clone()), |r| {
                r.get::<_, String>("item_id")
            })
        })
        .as_deref(),
        Some("mf1")
    );

    let (exists_sql, exists_args) = annotation_exists_sql(MEDIA_FILE, "u1", &["mf1"]);
    assert_eq!(assert_param_count(&store, &exists_sql, &exists_args), 3);
    assert_eq!(
        run(&store, |conn| {
            query_optional(conn, &(exists_sql, exists_args), |r| {
                r.get::<_, i64>("exist")
            })
        }),
        Some(1)
    );

    // The count renderer renders fine, carries 3 args, and cannot be prepared.
    let (count_sql, count_args) = annotation_count_sql(MEDIA_FILE, "u1", &["mf1"]);
    assert!(count_sql.contains("count(distinct annotation.id)"));
    assert_eq!(count_args.len(), 3);
    let err = try_run(&store, |conn| {
        conn.prepare(&count_sql).map(|stmt| stmt.parameter_count())
    })
    .expect_err("annotation_count_sql must not prepare on the fresh schema");
    let cause = err.to_string();
    assert!(
        cause.contains("no such column: annotation.id"),
        "unexpected prepare error: {cause}"
    );

    // `item_id` is the only id-ish column available: count(distinct item_id)
    // prepares and returns 1, a DIFFERENT aggregate than Go's shape. The renderer
    // must not be "fixed" to it.
    let (where_sql, where_args) = ann_id(MEDIA_FILE, "u1", &["mf1"]);
    let substitute = (
        format!("SELECT count(distinct item_id) as count FROM annotation WHERE {where_sql}"),
        where_args,
    );
    assert_eq!(
        run(&store, |conn| query_optional(conn, &substitute, |r| r
            .get::<_, i64>("count"))),
        Some(1)
    );
    std::fs::remove_file(&path).ok();
}
