//! S-D3g1 — the EXECUTED read sides of `library_repo`, `property_repo` and
//! `tag_repo`, run against a real SQLite DB built from the fresh schema
//! (harness shape copied from `tests/runtime_hydrate_playlist_radio_artwork.rs`:
//! one isolated temp file per invocation, `apply_fresh_schema` on a pooled
//! connection so `SEEDEDRAND`/`NATSORT` are registered per connection).
//!
//! Go truth under test: `libraryRepository.Get`/`GetPath`/`GetAll`/`CountAll`/
//! `ScanInProgress`/`GetUsersWithLibraryAccess`
//! (`persistence/library_repository.go:35-67,283-307`), `propertyRepository.Get`/
//! `Put`/`DefaultGet`/`Delete` (`persistence/property_repository.go:14-49`),
//! `tagRepository.GetAll` (`persistence/tag_repository.go:78-83`) over
//! `baseTagRepository.newSelect`/`Count`/`Read`
//! (`persistence/sql_tags.go:167-228`), and the shared `queryOne`/`queryAll`/
//! `count` (`persistence/sql_base_repository.go:336-355,375-388,441-457,565-576`).
//!
//! Ground truth for the executed values below comes from the parent's Go oracles
//! (`/tmp/sd3g1_oracle`, `/tmp/sd3g1_oracle2`, `/tmp/sd3g1_oracle4`,
//! `/tmp/sd3g1_oracle5`, `/tmp/sd3g1_oracle6`, `/tmp/sd3g1_oracle8`,
//! `/tmp/sd3g1_oracle9`) — each test names its oracle and, where a claim is only
//! derived from the Go source rather than executed, says INFERRED in its comment.

use nd_db::library_repo::{LibraryError, LibraryRepo};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::property_repo::{PropertyError, PropertyRepo};
use nd_db::sql_restful::{Filter, QueryOptions, RestQueryOptions};
use nd_db::store::SQLStore;
use nd_db::tag_repo::{Tag, TagError, TagRepo};
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
/// The logged user (Go's `loggedUser(ctx).ID`). In `sql_tags.go` it only decides
/// whether the INNER `user_library` join is added (`:170-175`), so its admin flag
/// is irrelevant here — no tag read has an ownership predicate.
const ADM: &str = "adm";

/// One isolated on-disk DB per invocation: a unique temp path, freshly migrated,
/// pooled so `SEEDEDRAND`/`NATSORT` are registered per connection.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-hydrate-lib-prop-tag-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one closure on a leased connection. Lease/store errors panic; the repo's
/// own error value is handed back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Execute a raw statement with typed args.
fn insert(store: &SQLStore, sql: &str, args: &[rusqlite::types::Value]) {
    store
        .with_connection_sync(|conn| {
            conn.execute(sql, rusqlite::params_from_iter(args.iter()))
                .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"));
}

fn v(text: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(text.to_string())
}
fn i64(value: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(value)
}

/// The fresh schema seeds NO `library` row (unlike Go's migration chain, which
/// inserts library 1 and stamps `default_new_users`), so every test seeds its own.
///
/// `updated_at`/`created_at` are seeded explicitly rather than left to the DDL's
/// `current_timestamp` default, so the record assertions are deterministic.
fn seed_library(store: &SQLStore, id: i64, name: &str, path: &str, total_songs: i64) {
    insert(
        store,
        "INSERT INTO library (id, name, path, total_songs, last_scan_at, \
         last_scan_started_at, full_scan_in_progress, updated_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            i64(id),
            v(name),
            v(path),
            i64(total_songs),
            v(T0),
            v(T0),
            i64(0),
            v(T1),
            v(T0),
        ],
    );
}

/// The four-row fixture the library assertions use: ids 1..4 with names chosen so
/// a `name asc`/`name desc` sort has an unambiguous answer.
fn seed_four_libraries(store: &SQLStore) {
    for (id, name, path, songs) in [
        (1, "Alpha", "/music-A", 7),
        (2, "Beta", "/music-B", 0),
        (3, "gamma", "/music-C", 1),
        (4, "Delta", "/music-D", 9),
    ] {
        seed_library(store, id, name, path, songs);
    }
}

/// `user` row: an admin, with `user_library` access to `library_ids`.
fn seed_admin(store: &SQLStore, library_ids: &[i64]) {
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(ADM),
            v(ADM),
            v(ADM),
            v("adm@x.y"),
            v("CIPHER"),
            i64(1),
            v(T0),
            v(T1),
        ],
    );
    for lib in library_ids {
        insert(
            store,
            "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
            &[v(ADM), i64(*lib)],
        );
    }
}

/// A dictionary tag plus its per-library counts. `library_tag` FKs both sides, so
/// the tag and the library must exist first.
fn seed_tag(store: &SQLStore, name: &str, value: &str, library_id: i64, albums: i64, songs: i64) {
    let id = nd_id::new_tag_id(name, value);
    insert(
        store,
        "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?)",
        &[v(&id), v(name), v(value)],
    );
    if library_id > 0 {
        insert(
            store,
            "INSERT INTO library_tag (tag_id, library_id, album_count, media_file_count) \
             VALUES (?, ?, ?, ?)",
            &[v(&id), i64(library_id), i64(albums), i64(songs)],
        );
    }
}

#[test]
fn library_get_returns_the_full_record_from_an_integer_id_bind() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Alpha", "/music-A", 7);

    let record = lease(&store, |conn| LibraryRepo::new(conn).get(1)).expect("row 1 exists");

    // Executed against the real Go repository (/tmp/sd3g1_oracle): Get(1) read
    // name=Alpha, path=/music-A, totalSongs=0, defaultNew=true, and the seeded
    // last_scan_at back as 2024-01-01. Here the seed sets total_songs = 7 and
    // default_new_users stays at the DDL default (false), so only the seeded
    // columns are asserted — see the sibling test for the id-not-found arm.
    assert_eq!(record.id, 1);
    assert_eq!(record.name, "Alpha");
    assert_eq!(record.path, "/music-A");
    assert_eq!(record.total_songs, 7);
    assert_eq!(record.total_duration, 0.0);
    assert!(!record.full_scan_in_progress);
    assert!(!record.default_new_users);
    assert_eq!(
        record.last_scan_at,
        nd_db::time::parse(T0).expect("T0 parses")
    );
    assert_eq!(
        record.updated_at,
        nd_db::time::parse(T1).expect("T1 parses")
    );
    // `remote_path`/`pid_*`/`scanned_pid_*` are DDL-defaulted to '' and Go reads
    // them as '' too (INFERRED from `schema_ddl.rs:52` defaults + Go's non-pointer
    // string fields; not separately oracle-executed for the pid legs).
    assert_eq!(record.remote_path, "");
    assert_eq!(record.pid_album, "");
    assert_eq!(record.scanned_pid_album, "");
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_on_an_unknown_id_is_not_found_and_binds_an_integer() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Alpha", "/music-A", 7);

    let error = lease(&store, |conn| LibraryRepo::new(conn).get(99)).expect_err("id 99 has no row");
    assert!(matches!(error, LibraryError::NotFound), "got {error:?}");

    // The id leg is INTEGER, not TEXT: `library.id` is
    // `integer primary key autoincrement` (`nd-dbschema/src/schema_ddl.rs:52`) and
    // Go's `Get(ctx, id int)` binds a Go `int`, so the executed statement is
    // `SELECT * FROM library WHERE id = ?` with an integer arg. Pinned by the
    // rendered pair as well as by the read succeeding above.
    let (sql, args) = nd_db::library_repo::get_sql(3);
    assert_eq!(sql, "SELECT * FROM library WHERE id = ?");
    assert_eq!(args, vec![nd_db::sqlrepo::Value::Integer(3)]);
    // Executed Go (/tmp/sd3g1_oracle): Get(99) returned err="data not found"
    // beside a non-nil pointer to a zero Library; the port carries no dead pointer.
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_path_returns_only_the_path_and_reports_unknown_ids() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    let got = lease(&store, |conn| LibraryRepo::new(conn).get_path(2)).expect("library 2");
    assert_eq!(got, "/music-B");
    let missing =
        lease(&store, |conn| LibraryRepo::new(conn).get_path(99)).expect_err("no library 99");
    assert!(matches!(missing, LibraryError::NotFound), "got {missing:?}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_path_reads_an_empty_path_as_the_empty_string_not_not_found() {
    let (store, path) = fresh_store();
    // The fresh DDL allows `path` to be '' only via an explicit insert (the column
    // is `not null unique`), which is exactly Go's state for an unscanned library:
    // executed (/tmp/sd3g1_oracle4) on a pristine migrated DB, library 1 had
    // path = "" and Go's GetPath(1) returned ("", nil).
    insert(
        &store,
        "INSERT INTO library (id, name, path) VALUES (?, ?, ?)",
        &[i64(1), v("Music Library"), v("")],
    );

    let got =
        lease(&store, |conn| LibraryRepo::new(conn).get_path(1)).expect("empty path is a hit");
    assert_eq!(got, "");
    std::fs::remove_file(&path).ok();
}

/// The ids of a `Libraries` result, for order assertions.
fn ids(libs: &[nd_db::library_repo::LibraryRecord]) -> Vec<i64> {
    libs.iter().map(|lib| lib.id).collect()
}

#[test]
fn library_get_all_plain_returns_every_row_and_binds_nothing() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    let libs = lease(&store, |conn| LibraryRepo::new(conn).get_all_plain()).expect("4 rows");
    assert_eq!(ids(&libs), vec![1, 2, 3, 4]);

    // Executed Go trace (/tmp/sd3g1_oracle): GetAll(ctx) with an empty option list
    // is `SELECT * FROM library` with no args — `applyOptions`/`applyFilters` only
    // run when the option list is non-empty (`sql_base_repository.go:142-145`), so
    // there is no ORDER BY leg and row order is SQLite's (here the rowid order).
    let (sql, args) = nd_db::library_repo::get_all_sql(&[], 0, 0);
    assert_eq!(sql, "SELECT * FROM library");
    assert!(args.is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_all_sorts_by_a_mapped_field_and_pages_with_literals() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    // `totalSongs` is a whitelisted field name, so sanitize_sort keeps it and
    // `build_sort_order` snake-cases it (`SORT_MAPPINGS` is empty for this repo).
    // Executed Go (/tmp/sd3g1_oracle7, same four rows): `totalSongs`/`desc` returned
    // ids [4, 1, 3, 2] (total_songs 9, 7, 1, 0). GetAll traces ONE order leg —
    // `applyOptions` only appends `buildSortOrder` (`sql_base_repository.go:158`), so
    // `SELECT * FROM library ORDER BY total_songs desc`. The two-leg form
    // `ORDER BY total_songs desc, library.id` belongs to CountAll, whose `count`
    // appends `library.id` after the caller's leg (`:569`); it is pinned separately by
    // `library_count_all_query_executes_and_ignores_the_caller_page` and by
    // `library_repo.rs:69-81`. An earlier comment here mis-attributed that CountAll
    // trace to GetAll; corrected.
    // `get_all` takes the REST shape and parses it itself (library_repository.go:325),
    // so the sort arrives in the API's camelCase form and `sanitize_sort` snake-cases
    // it on the way in.
    let options = RestQueryOptions {
        sort: "totalSongs".to_string(),
        order: "desc".to_string(),
        ..RestQueryOptions::default()
    };
    let libs = lease(&store, |conn| {
        LibraryRepo::new(conn).get_all(Some(&options))
    })
    .expect("sorted read");
    assert_eq!(ids(&libs), vec![4, 1, 3, 2]);
    // The parsed shape `get_all` builds internally, checked separately so a regression
    // in `parse_rest_options` is visible rather than only implied by the row set.
    let parsed = nd_db::library_repo::parse_rest_options(Some(&options));
    assert_eq!(parsed.sort, "total_songs");
    assert_eq!(parsed.order, "desc");

    // Max/Offset page: executed Go (/tmp/sd3g1_oracle7) `Sort:"Name", Order:"DESC",
    // Max:3` returned ids [3, 4, 2] — `name` is NOT a library sort mapping, so the
    // leg is the snake-cased column name and SQLite's BINARY collation puts
    // lowercase `gamma` (id 3) first.
    let options = RestQueryOptions {
        sort: "Name".to_string(),
        order: "DESC".to_string(),
        max: 3,
        offset: 0,
        ..RestQueryOptions::default()
    };
    let libs = lease(&store, |conn| {
        LibraryRepo::new(conn).get_all(Some(&options))
    })
    .expect("first page");
    assert_eq!(ids(&libs), vec![3, 4, 2]);

    // The renderer takes the PARSER's output, not the raw camelCase/uppercase request
    // (`get_all_query_sql`'s WARNING): `sanitize_sort` lowercases the order and
    // `toSnakeCase` the field.
    let parsed = nd_db::library_repo::parse_rest_options(Some(&options));
    let (sql, args) = nd_db::library_repo::get_all_query_sql(Some(&parsed))
        .expect("the paged select materializes");
    assert_eq!(sql, "SELECT * FROM library ORDER BY name desc LIMIT 3");
    // squirrel renders LIMIT/OFFSET as decimal literals and binds nothing
    // (`select.go:139-147`), so this statement has no args at all.
    assert!(args.is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_all_offset_without_limit_executes_where_go_fails() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    // Deviation pinned, NOT fixed. Go's `applyOptions` sets the two bounds
    // independently (`sql_base_repository.go:151-156`), so `Offset` with no `Max`
    // renders a bare `OFFSET 2`, which SQLite rejects — executed Go returned
    // `near "2": syntax error` (/tmp/sd3g1_oracle2). The port emits
    // `LIMIT -1 OFFSET n` (`sqlrepo.rs:313-319`) and so SKIPS rows instead.
    let options = RestQueryOptions {
        offset: 2,
        ..RestQueryOptions::default()
    };
    let libs = lease(&store, |conn| {
        LibraryRepo::new(conn).get_all(Some(&options))
    })
    .expect("the port executes the offset-only page Go cannot");
    assert_eq!(ids(&libs), vec![3, 4]);

    let parsed = nd_db::library_repo::parse_rest_options(Some(&options));
    let (sql, args) = nd_db::library_repo::get_all_query_sql(Some(&parsed))
        .expect("the offset-only page materializes");
    assert_eq!(sql, "SELECT * FROM library LIMIT -1 OFFSET 2");
    assert!(args.is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_get_all_filters_bind_before_the_order_leg() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    // Executed Go (/tmp/sd3g1_oracle2): the REST filter pair renders
    // `WHERE (name LIKE ? AND path LIKE ?) ORDER BY name desc LIMIT 3` — squirrel's
    // clause KIND order puts WHERE ahead of ORDER BY even though Go called
    // `applyOptions` first (`sql_base_repository.go:141-145`).
    let options = QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        max: 3,
        filters: Some(vec![
            Filter::Like {
                column: "library.name".to_string(),
                pattern: "a%".to_string(),
            },
            Filter::Eq {
                column: "library.id".to_string(),
                value: nd_db::sqlrepo::Value::Integer(1),
            },
        ]),
        ..QueryOptions::default()
    };
    let (sql, args) = nd_db::library_repo::get_all_query_sql(Some(&options))
        .expect("the filtered select materializes");
    assert_eq!(
        sql,
        "SELECT * FROM library WHERE library.name LIKE ? AND library.id = ? \
         ORDER BY name desc LIMIT 3"
    );
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text("a%".to_string()),
            nd_db::sqlrepo::Value::Integer(1),
        ]
    );

    // Executed row set (/tmp/sd3g1_oracle7): the REST filter `name=a` renders the
    // default starts-with clause `library.name LIKE 'a%'` (`sql_restful.go:43-44`,
    // port: `starts_with_filter`, `sql_restful.rs:348-355`), which matched only
    // Alpha (SQLite LIKE is ASCII-case-insensitive, so `a%` hits "Alpha" and not
    // "gamma") — id 1. The Eq leg above pins the arg order, not a new row set; the
    // executed read below goes through `get_all`, which parses this same pair out of
    // the REST filter map (`id` ends in "id" so it takes the exact-match arm,
    // `sql_restful.go:44-47`).
    let rest = RestQueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        max: 3,
        filters: [
            ("name".to_string(), "a".to_string()),
            ("id".to_string(), "1".to_string()),
        ]
        .into_iter()
        .collect(),
        ..RestQueryOptions::default()
    };
    let libs = lease(&store, |conn| LibraryRepo::new(conn).get_all(Some(&rest)))
        .expect("filtered read runs");
    assert_eq!(
        ids(&libs),
        vec![1],
        "Eq id=1 restricts the LIKE match to Alpha"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_count_all_returns_the_distinct_row_count() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    let count = lease(&store, |conn| LibraryRepo::new(conn).count_all()).expect("count runs");
    assert_eq!(count, 4);

    // Executed Go trace (/tmp/sd3g1_oracle4): `SELECT count(distinct library.id) as
    // count FROM library ORDER BY library.id` — the ORDER BY leg comes from
    // `count`'s own `.OrderBy(table+".id")` (`sql_base_repository.go:565-570`),
    // which the comment at `:565` claims removes ordering but never does
    // (`RemoveOrderBy` is not called; squirrel `OrderBy` appends, select.go:362-373).
    let (sql, args) = nd_db::library_repo::count_all_sql();
    assert_eq!(
        sql,
        "SELECT count(distinct library.id) as count FROM library ORDER BY library.id"
    );
    assert!(args.is_empty());

    // Empty table: dbx's `count` scans one aggregate row, so 0 rather than an error
    // (INFERRED from `sql_base_repository.go:565-576` — the aggregate always emits
    // one row; not separately oracle-executed on an empty library table).
    insert(&store, "DELETE FROM library", &[]);
    let count = lease(&store, |conn| LibraryRepo::new(conn).count_all()).expect("count on empty");
    assert_eq!(count, 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn library_count_all_keeps_filters_and_appends_the_reset_order_leg() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    // Executed Go (/tmp/sd3g1_oracle7): `Count(Filters:{name:a}, Sort:"bogus")`
    // returned 1 with the caller sort dropped by sanitizeSort, and the trace showed
    // `WHERE library.name = ? ORDER BY name desc, library.id` for a mapped sort —
    // i.e. the caller leg comes FIRST and `count`'s `library.id` leg is APPENDED
    // after it (`sql_base_repository.go:571` applies filters, `:569` appends order).
    let options = QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        filters: Some(vec![Filter::Eq {
            column: "library.name".to_string(),
            value: nd_db::sqlrepo::Value::Text("Alpha".to_string()),
        }]),
        ..QueryOptions::default()
    };
    let (sql, args) = nd_db::library_repo::count_all_query_sql(Some(&options))
        .expect("the filtered count materializes");
    assert_eq!(
        sql,
        "SELECT count(distinct library.id) as count FROM library \
         WHERE library.name = ? ORDER BY name desc, library.id"
    );
    assert_eq!(args, vec![nd_db::sqlrepo::Value::Text("Alpha".to_string())]);
    std::fs::remove_file(&path).ok();
}

/// `ScanInProgress` (`library_repository.go:190-194`): `count > 0` over
/// `last_scan_started_at <> ZERO_TIME`, executed. The bound is `Value::Integer(0)`
/// because this repo still writes epoch integers into that `datetime` column
/// (`ZERO_TIME` and the `scan_*` binds, with `time.rs:42-49` declaring the
/// conversion unconverted), so the comparison is INTEGER-vs-stored-value.
///
/// Executed here: rows stamped with a TEXT instant count (SQLite ranks INTEGER and
/// TEXT apart, so `<> 0` is true for every TEXT value), rows stamped with the
/// integer zero do not, and one stamped row flips it back. The mirror-image
/// divergence is recorded, NOT asserted as parity: Go binds `time.Time{}`, which its
/// driver renders as the TEXT zero instant, so a row stored as INTEGER 0 would also
/// satisfy `<> '0001-…'` and Go would report in-progress where the port reports
/// none. That arm is INFERRED from SQLite affinity rules plus Go's bind shape
/// (`library_repository.go:191`), not executed against the Go repository.
#[test]
fn library_scan_in_progress_follows_the_stamps_this_port_writes() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    let started = lease(&store, |conn| LibraryRepo::new(conn).scan_in_progress())
        .expect("the count statement executes");
    assert!(started, "TEXT-stamped rows satisfy <> 0");

    insert(
        &store,
        "UPDATE library SET last_scan_started_at = ?",
        &[i64(0)],
    );
    let idle = lease(&store, |conn| LibraryRepo::new(conn).scan_in_progress())
        .expect("the count statement executes");
    assert!(!idle, "integer-zero rows are excluded");

    insert(
        &store,
        "UPDATE library SET last_scan_started_at = ? WHERE id = 2",
        &[i64(1_704_067_200)],
    );
    assert!(
        lease(&store, |conn| LibraryRepo::new(conn).scan_in_progress())
            .expect("the count statement executes"),
        "one stamped row is enough, so this is a real count"
    );
    std::fs::remove_file(&path).ok();
}

/// A named `user` row with `user_library` access to `library_ids`. `user_name` is
/// `unique collate NOCASE` in the fresh DDL, so ids must differ case-insensitively.
fn seed_named_user(store: &SQLStore, id: &str, name: &str, library_ids: &[i64]) {
    let email = format!("{id}@x.y");
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(id),
            v(id),
            v(name),
            v(&email),
            v("CIPHER"),
            i64(0),
            v(T0),
            v(T1),
        ],
    );
    for lib in library_ids {
        insert(
            store,
            "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
            &[v(id), i64(*lib)],
        );
    }
}

/// `GetUsersWithLibraryAccess` (`library_repository.go:297-307`) executed. Go's
/// statement carries `ORDER BY u.name` unconditionally (`:302`), and `SELECT u.*`
/// emits no `libraries_json` alias, so Go's `dbUser.PostScan` takes its skip arm and
/// the association list stays empty — the port mirrors that by constructing
/// `libraries: Vec::new()` in `hydrate::row_to_user_without_libraries`.
#[test]
fn library_users_with_access_come_back_named_ordered_and_without_libraries() {
    let (store, path) = fresh_store();
    seed_library(&store, 3, "Gamma", "/music-G", 0);
    // Inserted newest-first so the order assertion cannot pass by scan order.
    seed_named_user(&store, "bob", "Bob", &[3]);
    seed_named_user(&store, "alice", "Alice", &[3]);

    let users = lease(&store, |conn| {
        LibraryRepo::new(conn).get_users_with_library_access(3)
    })
    .expect("the join executes");
    assert_eq!(users.len(), 2, "both users reach library 3");
    assert_eq!(users[0].name, "Alice");
    assert_eq!(users[1].name, "Bob");
    assert!(users[0].libraries.is_empty());
    assert!(users[1].libraries.is_empty());
    assert_eq!(users[0].id, "alice");
    assert!(!users[0].is_admin);
    assert_eq!(users[0].password, "CIPHER");
    assert_eq!(users[0].last_login_at, None);

    // An id with no associations is an empty Vec with no error: `queryAll` never maps
    // ErrNoRows to NotFound (`sql_base_repository.go:450-453`).
    let none = lease(&store, |conn| {
        LibraryRepo::new(conn).get_users_with_library_access(99)
    })
    .expect("no rows is not an error");
    assert!(none.is_empty());
    std::fs::remove_file(&path).ok();
}

/// `CountAll` with `QueryOptions` executed end-to-end, which the renderer assertion
/// does not cover. Bounds are dropped on the count path — `Repository::count`
/// (`sqlrepo.rs:294-301`) clears `limit`/`offset` exactly where Go's `count` calls
/// `RemoveOffset().RemoveLimit()` (`sql_base_repository.go:566-567`) — so a caller
/// page does NOT shift the count. That is deliberately the opposite of the `get_all`
/// offset-without-limit divergence, because Go drops the bounds here too.
#[test]
fn library_count_all_query_executes_and_ignores_the_caller_page() {
    let (store, path) = fresh_store();
    seed_four_libraries(&store);

    let (all, filtered) = lease(&store, |conn| {
        let repo = LibraryRepo::new(conn);
        (
            repo.count_all_query(None).expect("unfiltered count"),
            repo.count_all_query(Some(&QueryOptions {
                sort: "name".to_string(),
                order: "desc".to_string(),
                max: 3,
                offset: 2,
                filters: Some(vec![Filter::Eq {
                    column: "library.name".to_string(),
                    value: nd_db::sqlrepo::Value::Text("Alpha".to_string()),
                }]),
                ..QueryOptions::default()
            }))
            .expect("filtered count"),
        )
    });
    assert_eq!(all, 4, "all four rows");
    assert_eq!(
        filtered, 1,
        "the filter narrows it; max/offset change nothing"
    );
    std::fs::remove_file(&path).ok();
}

/// Run `Put`'s pair in Go's order on one connection: UPDATE first, and the INSERT
/// only when the update matched no row (`property_repository.go:14-27`). Returns
/// whether the insert ran.
///
/// `PropertyRepo` deliberately exposes no `put`/`delete` method — this slice's scope
/// is the read side — so the pair is executed through the statements
/// [`nd_db::property_repo::put_sql`] renders. That is what makes the ORDER claim
/// executed rather than inferred: the rendered SQL is what runs.
fn put_property(store: &SQLStore, id: &str, value: &str) -> bool {
    let pair = nd_db::property_repo::put_sql(id, value);
    store
        .with_connection_sync(|conn| {
            let updated = conn
                .execute(
                    &pair.update.0,
                    rusqlite::params_from_iter(pair.update.1.iter()),
                )
                .unwrap_or_else(|e| panic!("{} --{:?}--: {e}", pair.update.0, pair.update.1));
            let inserted = if updated > 0 {
                false
            } else {
                conn.execute(
                    &pair.insert.0,
                    rusqlite::params_from_iter(pair.insert.1.iter()),
                )
                .unwrap_or_else(|e| panic!("{} --{:?}--: {e}", pair.insert.0, pair.insert.1));
                true
            };
            Ok(inserted)
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

fn delete_property(store: &SQLStore, id: &str) {
    let (sql, args) = nd_db::property_repo::delete_sql(id);
    store
        .with_connection_sync(|conn| {
            conn.execute(&sql, rusqlite::params_from_iter(args.iter()))
                .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"));
}

#[test]
fn property_get_returns_the_value_column_only() {
    let (store, path) = fresh_store();
    insert(
        &store,
        "INSERT INTO property (id, value) VALUES (?, ?)",
        &[v("k"), v("v1")],
    );

    let value = lease(&store, |conn| PropertyRepo::new(conn).get("k")).expect("k exists");
    assert_eq!(value, "v1");

    // Executed Go (/tmp/sd3g1_oracle): Get("k") returned "v1" with err=<nil>, and
    // the trace is `SELECT value FROM property WHERE id = {:p0}` — the value column
    // only, no id in the projection, and no user/library clause because Get builds a
    // plain `Select(...).From(table).Where(Eq{"id": id})` rather than `newSelect`
    // (`property_repository.go:30-33`).
    let (sql, args) = nd_db::property_repo::get_sql("k");
    assert_eq!(sql, "SELECT value FROM property WHERE id = ?");
    assert_eq!(args, vec![nd_db::sqlrepo::Value::Text("k".to_string())]);

    // A stored empty string is a HIT, not NotFound (the row exists; Go's non-pointer
    // string scans ''). INFERRED for Go specifically — no oracle arm read an empty
    // property value — but pinned here against the real DDL default `''`.
    insert(
        &store,
        "INSERT INTO property (id, value) VALUES (?, ?)",
        &[v("empty"), v("")],
    );
    let value = lease(&store, |conn| PropertyRepo::new(conn).get("empty")).expect("empty hit");
    assert_eq!(value, "");
    std::fs::remove_file(&path).ok();
}

#[test]
fn property_get_on_a_missing_id_is_not_found() {
    let (store, path) = fresh_store();
    insert(
        &store,
        "INSERT INTO property (id, value) VALUES (?, ?)",
        &[v("k"), v("v1")],
    );

    let error =
        lease(&store, |conn| PropertyRepo::new(conn).get("missing")).expect_err("no such id");
    assert!(matches!(error, PropertyError::NotFound), "got {error:?}");

    // Executed Go (/tmp/sd3g1_oracle): Get("missing") returned err="data not found"
    // beside the value ""; Go's `queryOne` maps `sql.ErrNoRows` to model.ErrNotFound
    // (`sql_base_repository.go:375-388`). The port cannot carry the "" beside the
    // error, and no Go caller reads the value before checking the error.
    std::fs::remove_file(&path).ok();
}

#[test]
fn property_default_get_folds_not_found_into_the_default() {
    let (store, path) = fresh_store();
    insert(
        &store,
        "INSERT INTO property (id, value) VALUES (?, ?)",
        &[v("k"), v("v1")],
    );

    // Executed Go (/tmp/sd3g1_oracle): `DefaultGet("missing", "fallback")` returned
    // "fallback" with err=<nil>, and `DefaultGet("k", "fallback")` returned the
    // stored "v1". Both arms are pinned.
    let got = lease(&store, |conn| {
        PropertyRepo::new(conn).default_get("missing", "fallback")
    })
    .expect("NotFound is folded away");
    assert_eq!(got, "fallback");

    let got = lease(&store, |conn| {
        PropertyRepo::new(conn).default_get("k", "fallback")
    })
    .expect("hit wins over the default");
    assert_eq!(got, "v1");

    // Go's third arm — a NON-NotFound error returns the default BESIDE the error
    // (`property_repository.go:42-44`) — is not representable in a `Result`, so the
    // port returns `Err` alone. Documented in `property_repo.rs`; INFERRED, no
    // executed Go arm for it (the fresh schema makes a scan failure unreachable).
    std::fs::remove_file(&path).ok();
}

#[test]
fn property_put_update_then_insert_pair_runs_in_go_order() {
    let (store, path) = fresh_store();

    // First Put on a fresh id: the UPDATE matches nothing, so the INSERT runs.
    // Executed Go (/tmp/sd3g1_oracle): Put("k","v1") on an empty table created the
    // row, and the trace order is the UPDATE then the INSERT.
    assert!(
        put_property(&store, "k", "v1"),
        "insert runs when update hit 0"
    );
    let value = lease(&store, |conn| PropertyRepo::new(conn).get("k")).expect("row created");
    assert_eq!(value, "v1");

    // Second Put on the same id: the UPDATE matches one row, so the INSERT is
    // SKIPPED (`property_repository.go:19-21`). Asserting "no insert" by value plus
    // row count: a second INSERT would hit the `id` PRIMARY KEY and panic.
    assert!(
        !put_property(&store, "k", "v2"),
        "insert is skipped when update hit 1"
    );
    let value = lease(&store, |conn| PropertyRepo::new(conn).get("k")).expect("still one row");
    assert_eq!(value, "v2");

    let count = lease(&store, |conn| {
        conn.query_row::<i64, _, _>(
            "SELECT count(*) FROM property WHERE id = ?",
            [v("k")],
            |r| r.get(0),
        )
    })
    .expect("counted");
    assert_eq!(count, 1, "the upsert never duplicates the row");

    // Bind order, executed against SQLite: the UPDATE binds value BEFORE id (squirrel
    // `Set` renders ahead of `Where`), the INSERT binds id BEFORE value
    // (`property_repository.go:23-24`).
    let pair = nd_db::property_repo::put_sql("k", "v3");
    assert_eq!(
        pair.update,
        (
            "UPDATE property SET value = ? WHERE id = ?".to_string(),
            vec![
                nd_db::sqlrepo::Value::Text("v3".to_string()),
                nd_db::sqlrepo::Value::Text("k".to_string()),
            ]
        )
    );
    assert_eq!(
        pair.insert,
        (
            "INSERT INTO property (id, value) VALUES (?, ?)".to_string(),
            vec![
                nd_db::sqlrepo::Value::Text("k".to_string()),
                nd_db::sqlrepo::Value::Text("v3".to_string()),
            ]
        )
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn property_delete_removes_the_row_and_get_then_reports_not_found() {
    let (store, path) = fresh_store();
    insert(
        &store,
        "INSERT INTO property (id, value) VALUES (?, ?)",
        &[v("k"), v("v1")],
    );

    delete_property(&store, "k");
    let error = lease(&store, |conn| PropertyRepo::new(conn).get("k")).expect_err("row deleted");
    assert!(matches!(error, PropertyError::NotFound), "got {error:?}");

    // Executed Go (/tmp/sd3g1_oracle): `Delete("k")` returned err=<nil> and the
    // following Get returned "data not found"; `Delete("missing")` also returned
    // err=<nil> — `delete` never inspects the row count
    // (`sql_base_repository.go:644-648`), so a miss is silent. Pinning the silent
    // miss with the rendered statement, which binds only the id.
    delete_property(&store, "never-existed");
    let (sql, args) = nd_db::property_repo::delete_sql("k");
    assert_eq!(sql, "DELETE FROM property WHERE id = ?");
    assert_eq!(args, vec![nd_db::sqlrepo::Value::Text("k".to_string())]);
    std::fs::remove_file(&path).ok();
}

/// The tag fixture: libraries 1 and 2, admin `ADM` joined to library 1 ONLY, and a
/// dictionary with three shapes — a tag with counts in the accessible library
/// (`genre/Rock`, 2 albums / 5 songs), a tag in it with zero counts (`genre/Pop`), a
/// tag in the INACCESSIBLE library (`genre/OnlyLib2`, 9/99), a tag with no
/// `library_tag` row at all (`genre/Lonely`), plus one tag of another name
/// (`mood/Chill`). Row sets below are the ones the parent executed against the real
/// Go repositories (`/tmp/sd3g1_oracle6`, `/tmp/sd3g1_oracle8`).
fn seed_tag_fixture(store: &SQLStore) {
    seed_library(store, 1, "Alpha", "/music-A", 0);
    seed_library(store, 2, "Beta", "/music-B", 0);
    seed_admin(store, &[1]);
    seed_tag(store, "genre", "Rock", 1, 2, 5);
    seed_tag(store, "genre", "Pop", 1, 0, 0);
    seed_tag(store, "mood", "Chill", 1, 0, 0);
    seed_tag(store, "genre", "OnlyLib2", 2, 9, 99);
    seed_tag(store, "genre", "Lonely", 0, 0, 0);
}

/// `TagList` as `value/albums/songs`, sorted for order-independent comparison.
fn tag_cells(tags: &[Tag]) -> Vec<String> {
    let mut cells: Vec<String> = tags
        .iter()
        .map(|t| format!("{}/{}/{}", t.tag_value, t.album_count, t.song_count))
        .collect();
    cells.sort();
    cells
}

#[test]
fn tag_get_all_scopes_rows_to_accessible_libraries_with_an_inner_join() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);

    // THE INNER-vs-LEFT PROOF. Go's `applyLibraryFiltering` LEFT-joins `library_tag`
    // (`sq.LeftJoin`, sql_tags.go:168) but INNER-joins `user_library` (`sq.Join`,
    // sql_tags.go:173, rendered literally as `JOIN` by squirrel select.go:297-299),
    // so a tag whose only `library_tag` row sits in an inaccessible library, and a
    // dictionary row with no `library_tag` row at all, BOTH drop out. Executed Go
    // (/tmp/sd3g1_oracle6): GetAll("genre") for this admin returned exactly
    // `Rock/2/5, Pop/0/0`. The parent's independent SQLite measurement of the same
    // rows (/tmp/sd3g1_leftjoin) got plain JOIN count = 1 vs LEFT JOIN count = 2, so
    // rendering that leg as LEFT — what this module did before — over-counts.
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all_plain("genre", Some(ADM))
    })
    .expect("aggregate read");
    assert_eq!(tag_cells(&tags), vec!["Pop/0/0", "Rock/2/5"]);

    // Same fixture with NO logged-in user: the join is never added
    // (`sql_tags.go:170-175`), so every dictionary row appears — executed Go
    // returned all four (`Lonely/0/0, OnlyLib2/9/99, Pop/0/0, Rock/2/5`), which is
    // 4 rows where the scoped read returned 2.
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all_plain("genre", None)
    })
    .expect("headless aggregate read");
    assert_eq!(
        tag_cells(&tags),
        vec!["Lonely/0/0", "OnlyLib2/9/99", "Pop/0/0", "Rock/2/5"]
    );

    // Executed statement, both legs byte-identical to the Go trace: bare `JOIN` on
    // `user_library`, `LEFT JOIN` on `library_tag`, GROUP BY last, and the user id
    // bound FIRST (squirrel binds JOIN args before WHERE args — Go's args map is
    // p0=adm, p1=genre).
    let (sql, args) = nd_db::tag_repo::get_all_plain_sql("genre", Some(ADM))
        .expect("the aggregate select materializes");
    assert_eq!(
        sql,
        "SELECT tag.id, tag.tag_name, tag.tag_value, \
         COALESCE(SUM(library_tag.album_count), 0) as album_count, \
         COALESCE(SUM(library_tag.media_file_count), 0) as song_count \
         FROM tag \
         LEFT JOIN library_tag on library_tag.tag_id = tag.id \
         JOIN user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ? \
         WHERE tag.tag_name = ? \
         GROUP BY tag.id, tag.tag_name, tag.tag_value"
    );
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text(ADM.to_string()),
            nd_db::sqlrepo::Value::Text("genre".to_string()),
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_get_all_on_an_empty_tag_name_matches_nothing_without_error() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);

    // Executed Go (/tmp/sd3g1_oracle8): GetAll("") bound the EMPTY string
    // (`WHERE tag.tag_name = ''`) and returned len=0 with err=<nil>. This is NOT the
    // identity case — an empty option list is, and that one renders no WHERE body at
    // all — so the empty name must bind rather than drop the clause.
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all_plain("", Some(ADM))
    })
    .expect("empty name is a valid query");
    assert!(tags.is_empty());

    let (sql, args) = nd_db::tag_repo::get_all_plain_sql("", Some(ADM)).expect("materializes");
    assert!(sql.ends_with("WHERE tag.tag_name = ? GROUP BY tag.id, tag.tag_name, tag.tag_value"));
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text(ADM.to_string()),
            nd_db::sqlrepo::Value::Text(String::new()),
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_get_all_maps_the_name_sort_to_tag_value_and_pages() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);

    // `setSortMappings({"name": "tag_value"})` (`sql_tags.go:173-175`). Executed Go
    // (/tmp/sd3g1_oracle6, /tmp/sd3g1_oracle8): Sort "name"/"desc" traced
    // `… GROUP BY tag.id, tag.tag_name, tag.tag_value ORDER BY tag_value desc` and
    // returned Rock before Pop — the ORDER BY leg sits AFTER GROUP BY, which is
    // squirrel's clause kind order.
    // `get_all` takes the REST shape and parses it itself (`sql_tags.go:224`).
    let options = RestQueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        ..RestQueryOptions::default()
    };
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some(ADM), Some(&options))
    })
    .expect("sorted aggregate read");
    let parsed = nd_db::tag_repo::parse_rest_options(Some(&options));
    assert_eq!(
        tags.iter().map(|t| t.tag_value.clone()).collect::<Vec<_>>(),
        vec!["Rock", "Pop"]
    );

    // The renderer takes the parser's output (`get_all_sql`'s WARNING).
    let (sql, args) = nd_db::tag_repo::get_all_sql("genre", Some(ADM), Some(&parsed), None)
        .expect("the sorted aggregate materializes");
    assert!(
        sql.ends_with("GROUP BY tag.id, tag.tag_name, tag.tag_value ORDER BY tag_value desc"),
        "{sql}"
    );
    assert_eq!(args.len(), 2, "user id then tag name, no sort bind");

    // The un-mapped `Sort:"album_count"` keeps its snake-cased alias — executed Go
    // traced `ORDER BY album_count desc LIMIT 2` (/tmp/sd3g1_oracle6). Executed row
    // set here: Rock (2 albums) then Pop (0), matching Go's desc order.
    let options = RestQueryOptions {
        sort: "albumCount".to_string(),
        order: "desc".to_string(),
        max: 2,
        ..RestQueryOptions::default()
    };
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some(ADM), Some(&options))
    })
    .expect("alias-sorted aggregate read");
    assert_eq!(
        tags.iter().map(|t| t.tag_value.clone()).collect::<Vec<_>>(),
        vec!["Rock", "Pop"]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_get_all_renders_caller_filters_before_the_tag_name_clause() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);

    // Clause order is a real parity question here, and the executed traces settle
    // it: `sqlRepository.newSelect` runs `applyOptions`/`applyFilters`
    // (`sql_base_repository.go:140-146`) BEFORE the caller appends
    // `Where(Eq{"tag.tag_name": name})` (`tag_repository.go:79`), and squirrel ANDs
    // repeated `.Where()` calls in CALL order — so the filters come FIRST. Executed
    // Go (/tmp/sd3g1_oracle6): `GetAll("genre", Filters: Eq{"library_tag.library_id":
    // 2})` traced `WHERE library_tag.library_id = ? AND tag.tag_name = ?` with args
    // [adm, 2, genre].
    let options = QueryOptions {
        filters: Some(vec![Filter::Eq {
            column: "library_tag.library_id".to_string(),
            value: nd_db::sqlrepo::Value::Text("2".to_string()),
        }]),
        ..QueryOptions::default()
    };
    // The REST spelling of the same key goes through the registered `library_id`
    // mapping (`sql_tags.go:157`), which is what the executed read below receives.
    let rest_filter = RestQueryOptions {
        filters: [("library_id".to_string(), "2".to_string())]
            .into_iter()
            .collect(),
        ..RestQueryOptions::default()
    };
    let (sql, args) = nd_db::tag_repo::get_all_sql("genre", Some(ADM), Some(&options), None)
        .expect("the filtered aggregate materializes");
    assert!(
        sql.ends_with(
            "WHERE library_tag.library_id = ? AND tag.tag_name = ? \
             GROUP BY tag.id, tag.tag_name, tag.tag_value"
        ),
        "{sql}"
    );
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text(ADM.to_string()),
            nd_db::sqlrepo::Value::Text("2".to_string()),
            nd_db::sqlrepo::Value::Text("genre".to_string()),
        ]
    );

    // Executed row set for that filter (/tmp/sd3g1_oracle8, admin with access to
    // library 1 only): the `library_id = 2` clause matched nothing, because the only
    // tag in library 2 is removed by the INNER join before the WHERE is even reached.
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some(ADM), Some(&rest_filter))
    })
    .expect("filtered aggregate read");
    assert!(tags.is_empty());

    // The REST spelling of the same two keys goes through the registered filter
    // mappings, and executed Go (/tmp/sd3g1_oracle8) shows they are NOT the naive
    // whitelist arms: `Filters:{"name":"oc"}` traces `WHERE (tag_value LIKE ?)` bound
    // `%oc%` (`containsFilter("tag_value")`, sql_tags.go:167), and
    // `Filters:{"library_id":"2"}` traces `WHERE (library_tag.library_id = ?)`
    // (`tagLibraryIdFilter`, sql_tags.go:168). The un-parenthesized AND-join here is
    // deliberate: Go's `And{}` renders `(a AND b)` (`squirrel expr.go:369-388`), which
    // is row-set identical against this single-clause-chain WHERE.
    let rest = nd_db::sql_restful::RestQueryOptions {
        filters: [("name".to_string(), "oc".to_string())]
            .into_iter()
            .collect(),
        ..nd_db::sql_restful::RestQueryOptions::default()
    };
    let parsed = nd_db::tag_repo::parse_rest_options(Some(&rest));
    let (sql, args) = nd_db::tag_repo::get_all_sql("genre", Some(ADM), Some(&parsed), None)
        .expect("the REST-shaped aggregate materializes");
    assert!(
        sql.contains("WHERE tag_value LIKE ? AND tag.tag_name = ?"),
        "{sql}"
    );
    assert_eq!(
        args.len(),
        3,
        "user id, the LIKE pattern, then the tag name"
    );
    assert_eq!(args[1], nd_db::sqlrepo::Value::Text("%oc%".to_string()));

    // Executed row set (/tmp/sd3g1_oracle8): that read returned `Rock/2/5` alone. The
    // executed read takes the REST shape and parses it internally, so the `rest` value
    // above is what it is handed.
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some(ADM), Some(&rest))
    })
    .expect("REST-shaped read");
    assert_eq!(tag_cells(&tags), vec!["Rock/2/5"]);
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_count_all_is_the_distinct_aggregate_over_the_same_joins() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);

    // Executed Go (/tmp/sd3g1_oracle8): `Count()` for this admin = 3 (Rock, Pop,
    // Chill — OnlyLib2 and Lonely are excluded by the INNER join), headless = 5.
    // A LEFT rendering of the `user_library` leg would report 4 here, so this number
    // is the second INNER-vs-LEFT proof on the count path.
    let count = lease(&store, |conn| TagRepo::new(conn).count_all(Some(ADM), None)).expect("count");
    assert_eq!(count, 3);
    let count =
        lease(&store, |conn| TagRepo::new(conn).count_all(None, None)).expect("headless count");
    assert_eq!(count, 5);

    // `count`'s own shape (`sql_tags.go:203-211` + `sql_base_repository.go:565-576`):
    // `COUNT(DISTINCT tag.id)`, NO GROUP BY, no tag-name clause (this dictionary
    // instance has `tagFilter == nil`, `tag_repository.go:22`), and the reset
    // `ORDER BY tag.id`. Executed Go traced exactly that, and
    // `Count(Sort:"name", Order:"desc")` traced `… ORDER BY tag.id` — Count never
    // calls `applyOptions`, so a caller sort cannot reach this statement.
    let (sql, args) =
        nd_db::tag_repo::count_all_sql(Some(ADM), None).expect("the count materializes");
    assert_eq!(
        sql,
        "SELECT count(distinct tag.id) as count FROM tag \
         LEFT JOIN library_tag on library_tag.tag_id = tag.id \
         JOIN user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ? \
         ORDER BY tag.id"
    );
    assert_eq!(args, vec![nd_db::sqlrepo::Value::Text(ADM.to_string())]);

    let sorted = QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        ..QueryOptions::default()
    };
    let (sql, _) = nd_db::tag_repo::count_all_sql(Some(ADM), Some(&sorted)).expect("materializes");
    assert!(
        sql.ends_with("ORDER BY tag.id"),
        "caller sort is dropped: {sql}"
    );

    // Executed filter arms (/tmp/sd3g1_oracle8): `Count(name=oc)` = 1 and
    // `Count(library_id=2)` = 0 for this admin. Note the WHERE order is the REVERSE
    // of GetAll's: `count` appends its filters AFTER the clauses already on the
    // builder (`sql_base_repository.go:571`).
    let rest = nd_db::sql_restful::RestQueryOptions {
        filters: [("name".to_string(), "oc".to_string())]
            .into_iter()
            .collect(),
        ..nd_db::sql_restful::RestQueryOptions::default()
    };
    let options = nd_db::tag_repo::parse_rest_options(Some(&rest));
    let count = lease(&store, |conn| {
        TagRepo::new(conn).count_all(Some(ADM), Some(&options))
    })
    .expect("filtered count");
    assert_eq!(count, 1);
    let (sql, args) =
        nd_db::tag_repo::count_all_sql(Some(ADM), Some(&options)).expect("materializes");
    assert!(sql.contains("WHERE tag_value LIKE ?"), "{sql}");
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text(ADM.to_string()),
            nd_db::sqlrepo::Value::Text("%oc%".to_string()),
        ]
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_read_binds_an_unqualified_id_and_reports_inaccessible_tags_as_not_found() {
    let (store, path) = fresh_store();
    seed_tag_fixture(&store);
    let rock = nd_id::new_tag_id("genre", "Rock");
    let only_lib2 = nd_id::new_tag_id("genre", "OnlyLib2");

    // Executed Go (/tmp/sd3g1_oracle8): `Read(rock)` for this admin returned the tag
    // with album 2 / song 5; `Read(onlylib2)` returned err="data not found" even
    // though the dictionary row exists, because the INNER join removes the aggregate
    // row rather than yielding a zero-count tag.
    let tag =
        lease(&store, |conn| TagRepo::new(conn).read(Some(ADM), &rock)).expect("accessible tag");
    assert_eq!(tag.tag_value, "Rock");
    assert_eq!(tag.album_count, 2);
    assert_eq!(tag.song_count, 5);
    assert_eq!(tag.id, rock);
    assert_eq!(tag.tag_name, "genre");

    let error = lease(&store, |conn| {
        TagRepo::new(conn).read(Some(ADM), &only_lib2)
    })
    .expect_err("inaccessible tag");
    assert!(matches!(error, TagError::NotFound), "got {error:?}");

    // `Read` (`sql_tags.go:212-216`): `Where(Eq{"id": id})` — UNQUALIFIED — and no
    // options, so no filters, no page, no sort leg, and no tag-name clause. Executed
    // trace: `WHERE id = ? GROUP BY …` with args [adm, <tag id>].
    let (sql, args) = nd_db::tag_repo::read_sql(Some(ADM), &rock).expect("the read statement");
    assert!(sql.contains("WHERE id = ?"), "{sql}");
    assert!(
        !sql.contains("tag.tag_name = ?"),
        "no tag-name clause: {sql}"
    );
    assert_eq!(
        args,
        vec![
            nd_db::sqlrepo::Value::Text(ADM.to_string()),
            nd_db::sqlrepo::Value::Text(rock.clone()),
        ]
    );
    std::fs::remove_file(&path).ok();
}
