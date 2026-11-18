//! S-D3f — the EXECUTED REST reads of `library_repo`, `tag_repo`, `radio_repo` and
//! `player_repo` now parse their rest options INTERNALLY, which is where Go's
//! `sanitizeSort` gate lives: `library_repository.go:325`, `sql_tags.go:224`,
//! `radio_repository.go:121` and `player_repository.go:114` each call
//! `parseRestOptions` INSIDE `ReadAll`, so `sanitizeSort`
//! (`persistence/sql_restful.go:69-86`) always runs before
//! `applyOptions` renders an ORDER BY leg (`sql_base_repository.go:157-158`).
//!
//! Before this slice those four executed reads accepted an already-built
//! `QueryOptions`/`Options`, so a caller could hand an unsanitized sort (or, for radio,
//! an unsanitized `Filter::Raw`) straight to the renderer. Each test below measures the
//! PRE-FIX behavior by driving the raw renderer — which is exactly what the executed
//! read used to do, since it forwarded its options unchanged — and then asserts the
//! gated behavior through the repository method.
//!
//! Harness: a real SQLite DB from `nd_dbschema::schema::apply_fresh_schema` on a pooled
//! lease (one isolated temp file per invocation), the same shape as
//! `tests/runtime_hydrate_library_property_tag.rs`. No SQL is hand-written for a read
//! under test: every statement comes from the repository or its public renderer.

use nd_db::library_repo::LibraryRepo;
use nd_db::player_repo::{LoggedUser, Options, PlayerRepo, PLAYER_TABLE};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::radio_repo::{self, RadioRepo};
use nd_db::sql_restful::{Filter, QueryOptions, RestQueryOptions};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::tag_repo::TagRepo;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
/// The logged admin user for the tag aggregate's INNER `user_library` leg
/// (`sql_tags.go:171-174`).
const ADM: &str = "adm";

fn fresh_store(prefix: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(prefix, ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one closure on a leased connection; store/lease failures panic, the repo's own
/// error value is handed back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

fn insert(store: &SQLStore, sql: &str, args: &[Value]) {
    store
        .with_connection_sync(|conn| {
            conn.execute(sql, rusqlite::params_from_iter(args.iter()))
                .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"));
}

/// Execute a raw SELECT and return the first column as text. Used ONLY to prove
/// non-vacuity — i.e. that a hostile predicate really would flip row order if it
/// reached ORDER BY — never to stand in for a read under test.
fn raw_order(store: &SQLStore, sql: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    })
}

// --------------------------------------------------------------------------- library

fn seed_library(store: &SQLStore, id: i64, name: &str, path: &str, total_songs: i64) {
    insert(
        store,
        "INSERT INTO library (id, name, path, total_songs, last_scan_at, \
         last_scan_started_at, full_scan_in_progress, updated_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Value::Integer(id),
            Value::Text(name.to_string()),
            Value::Text(path.to_string()),
            Value::Integer(total_songs),
            Value::Text(T0.to_string()),
            Value::Text(T0.to_string()),
            Value::Integer(0),
            Value::Text(T1.to_string()),
            Value::Text(T0.to_string()),
        ],
    );
}

/// Four libraries whose rowid order, `total_songs % 2` order and `total_songs` order are
/// all DIFFERENT, so a leaked order leg is visible rather than coincidental. Only ids 1
/// and 3 carry `secret-a` in `path`.
fn seed_libraries_for_sort_gate(store: &SQLStore) {
    seed_library(store, 1, "Alpha", "/music/secret-a", 7);
    seed_library(store, 2, "Beta", "/music/beta", 2);
    seed_library(store, 3, "Gamma", "/music/secret-c", 9);
    seed_library(store, 4, "Delta", "/music/delta", 4);
}

fn library_ids(list: &[nd_db::library_repo::LibraryRecord]) -> Vec<i64> {
    list.iter().map(|l| l.id).collect()
}

/// The gate: three hostile sorts are dropped before ORDER BY, so `get_all` returns the
/// no-sort (rowid) order for every one of them, while the raw renderer — the pre-fix
/// executed path — either reorders the rows or fails to execute.
#[test]
fn library_hostile_sorts_are_dropped_and_row_order_is_stable() {
    let (store, path) = fresh_store("nd-db-gate-library-");
    seed_libraries_for_sort_gate(&store);

    // Baseline: options == None is Go's empty option list — no ORDER BY at all.
    let baseline =
        lease(&store, |conn| LibraryRepo::new(conn).get_all(None)).expect("unsorted read");
    assert_eq!(library_ids(&baseline), vec![1, 2, 3, 4]);

    // Proof of non-vacuity, measured: the same predicates really do reorder the rows
    // when they reach ORDER BY, so "order is unchanged" below is a real result.
    assert_eq!(
        raw_order(
            &store,
            "SELECT CAST(id AS TEXT) FROM library ORDER BY total_songs % 2 asc"
        ),
        vec!["2", "4", "1", "3"],
        "the parity predicate would flip order if it reached ORDER BY"
    );
    assert_eq!(
        raw_order(
            &store,
            "SELECT CAST(id AS TEXT) FROM library ORDER BY (CASE WHEN path LIKE '%secret-a%' \
             THEN 1 ELSE 0 END) asc"
        ),
        vec!["2", "4", "3", "1"],
        "the CASE predicate moves the one secret-carrying row (id 1) last"
    );

    // Every arm is a leg Go's parser drops and the pre-fix read handed straight to the
    // renderer. All three EXECUTE pre-fix — `build_sort_order` splits on spaces OUTSIDE
    // parentheses (`sql_base_repository.go:210-226`, `:230-245`, ported as
    // `sql_restful::split_sort_fields`), so even the CASE expression reaches SQLite as a
    // single field. The measured signal is the row the leaked leg pushes to the END;
    // ties among equal values are SQLite's own ordering, so only the distinguishing
    // position is asserted.
    let hostile = [
        // A no-space expression on a whitelisted column: `build_sort_order` takes its
        // first space-token as the column, so this renders `total_songs%2 asc`.
        ("total_songs%2 asc", None),
        // A mixed comma list — `sanitize_sort` validates the WHOLE snake-cased string
        // (`sql_restful.go:70-77`), yet each part maps on its own pre-fix.
        ("total_songs%2, id", None),
        // The brief's CASE shape over a column that must not be orderable.
        (
            "(CASE WHEN path LIKE '%secret-a%' THEN 1 ELSE 0 END)",
            Some("1"),
        ),
    ];
    for (sort, secret_last) in hostile {
        // PRE-FIX behavior, measured through the raw renderer the executed read used to
        // forward its options to (library_repo.rs:991 pre-change: `get_all_query_sql(options)`).
        let raw = QueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..QueryOptions::default()
        };
        let (raw_sql, _) = nd_db::library_repo::get_all_query_sql(Some(&raw))
            .expect("the renderer materializes whatever it is handed");
        assert!(
            raw_sql.contains("ORDER BY"),
            "the unsanitized sort must reach ORDER BY: {raw_sql}"
        );
        let pre_fix = lease(&store, |conn| {
            rusqlite::Connection::prepare(conn, &raw_sql).map(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, i64>("id"))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect::<Vec<i64>>()
            })
        });
        let leaked =
            pre_fix.unwrap_or_else(|e| panic!("pre-fix statement must execute: {raw_sql}: {e}"));
        assert_ne!(
            leaked,
            library_ids(&baseline),
            "pre-fix the leaked leg left the order untouched, so the stability result below \
             would be vacuous: {raw_sql}"
        );
        if let Some(secret) = secret_last {
            assert_eq!(
                leaked.last().map(|id| id.to_string()).as_deref(),
                Some(secret),
                "pre-fix the secret-carrying row is pushed to the end, which is the side \
                 channel this gate closes: {raw_sql}"
            );
        }

        // POST-FIX: through the executed repository read, the sort is dropped.
        let rows = lease(&store, |conn| {
            LibraryRepo::new(conn).get_all(Some(&RestQueryOptions {
                sort: sort.to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }))
        })
        .unwrap_or_else(|e| panic!("hostile sort {sort:?} must be dropped, not errored: {e}"));
        assert_eq!(
            library_ids(&rows),
            library_ids(&baseline),
            "sort {sort:?} changed row order"
        );
        assert_eq!(rows.len(), 4, "no row may be dropped or added");
        let parsed = nd_db::library_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        }));
        assert_eq!(parsed.sort, "", "sort {sort:?} must be dropped");
        let gated = nd_db::library_repo::get_all_query_sql(Some(&parsed))
            .expect("the gated statement materializes");
        assert!(!gated.0.contains("ORDER BY"), "{raw_sql}");
        assert!(!gated.0.to_lowercase().contains("case"), "{}", gated.0);
        assert!(
            !gated.0.contains("secret-a"),
            "the hostile expression must not survive in the text: {}",
            gated.0
        );
        assert!(parsed.filters.is_none(), "no filter may be smuggled");
    }
    std::fs::remove_file(path).ok();
}

/// Fix (c): dropping the hostile leg must not break legitimate sorting, and the
/// whitelisted sort must still order correctly through the executed read.
#[test]
fn library_whitelisted_sort_orders_and_the_count_is_invariant() {
    let (store, path) = fresh_store("nd-db-gate-library-");
    seed_libraries_for_sort_gate(&store);

    for (sort, order, expected) in [
        ("totalSongs", "asc", vec![2, 4, 1, 3]),
        ("totalSongs", "desc", vec![3, 1, 4, 2]),
        ("name", "asc", vec![1, 2, 4, 3]),
    ] {
        let rows = lease(&store, |conn| {
            LibraryRepo::new(conn).get_all(Some(&RestQueryOptions {
                sort: sort.to_string(),
                order: order.to_string(),
                ..RestQueryOptions::default()
            }))
        })
        .unwrap_or_else(|e| panic!("whitelisted sort {sort:?}/{order:?} failed: {e}"));
        assert_eq!(library_ids(&rows), expected, "sort {sort:?} {order:?}");
    }

    // Count under the same options: `count` drops the page
    // (`sql_base_repository.go:568`) and the aggregate is order-independent, so a caller
    // sort — hostile or whitelisted — cannot change the value.
    let plain =
        lease(&store, |conn| LibraryRepo::new(conn).count(None)).expect("count, no options");
    assert_eq!(plain, 4);
    for (sort, order) in [
        ("total_songs%2", "asc"),
        (
            "(CASE WHEN path LIKE '%secret-a%' THEN 1 ELSE 0 END)",
            "asc",
        ),
        ("totalSongs", "desc"),
        ("name", "asc"),
    ] {
        let count = lease(&store, |conn| {
            LibraryRepo::new(conn).count(Some(&RestQueryOptions {
                sort: sort.to_string(),
                order: order.to_string(),
                max: 1,
                offset: 3,
                ..RestQueryOptions::default()
            }))
        })
        .unwrap_or_else(|e| panic!("count under sort {sort:?} must not fail: {e}"));
        assert_eq!(count, plain, "sort {sort:?} changed the count");
    }

    // A filter does change the count, which is why filters stay in the signature.
    let filtered = lease(&store, |conn| {
        LibraryRepo::new(conn).count(Some(&RestQueryOptions {
            sort: "total_songs%2".to_string(),
            order: "asc".to_string(),
            filters: [("name".to_string(), "Gamma".to_string())]
                .into_iter()
                .collect(),
            ..RestQueryOptions::default()
        }))
    })
    .expect("filtered count");
    assert_eq!(filtered, 1);

    // The count statement's ORDER BY is the parser's output, so the hostile leg is gone
    // from the text too; the whitelisted one is kept ahead of the reset `library.id` leg
    // (Go's `count` appends rather than replaces — `sql_base_repository.go:569`).
    let (sql, _) = nd_db::library_repo::count_all_query_sql(Some(
        &nd_db::library_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: "totalSongs".to_string(),
            order: "desc".to_string(),
            ..RestQueryOptions::default()
        })),
    ))
    .expect("count materializes");
    assert!(
        sql.ends_with("ORDER BY total_songs desc, library.id"),
        "{sql}"
    );
    let (sql, _) = nd_db::library_repo::count_all_query_sql(Some(
        &nd_db::library_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: "total_songs%2".to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        })),
    ))
    .expect("count materializes");
    assert!(sql.ends_with("ORDER BY library.id"), "{sql}");
    std::fs::remove_file(path).ok();
}

// ----------------------------------------------------------------------------- tag

fn seed_tag(store: &SQLStore, name: &str, value: &str, library_id: i64, albums: i64, songs: i64) {
    let id = nd_id::new_tag_id(name, value);
    insert(
        store,
        "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?)",
        &[
            Value::Text(id.clone()),
            Value::Text(name.to_string()),
            Value::Text(value.to_string()),
        ],
    );
    if library_id > 0 {
        insert(
            store,
            "INSERT INTO library_tag (tag_id, library_id, album_count, media_file_count) \
             VALUES (?, ?, ?, ?)",
            &[
                Value::Text(id),
                Value::Integer(library_id),
                Value::Integer(albums),
                Value::Integer(songs),
            ],
        );
    }
}

fn seed_user_with_library(store: &SQLStore, id: &str, library_id: i64) {
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Value::Text(id.to_string()),
            Value::Text(id.to_string()),
            Value::Text(id.to_string()),
            Value::Text(format!("{id}@x.y")),
            Value::Text("CIPHER".to_string()),
            Value::Integer(1),
            Value::Text(T0.to_string()),
            Value::Text(T1.to_string()),
        ],
    );
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[Value::Text(id.to_string()), Value::Integer(library_id)],
    );
}

/// Dictionary rows in library 1 whose `tag_value`, `album_count` and
/// `media_file_count` orders are mutually different, so a leaked leg is visible.
fn seed_tags_for_sort_gate(store: &SQLStore) {
    // `user_library.library_id` has a FK to `library`, so the library row must exist
    // first (`nd-dbschema/src/schema_ddl.rs`, library_tag/user_library DDL).
    seed_library(store, 1, "Alpha", "/music-A", 0);
    seed_user_with_library(store, ADM, 1);
    // Dictionary (rowid) order Rock, Emo, Pop; media_file_count asc gives Pop, Rock,
    // Emo; album_count desc gives Pop, Rock, Emo; and a `%oc%` CASE gives Emo, Pop,
    // Rock — every one of them differs from the rowid order, so a leaked leg is
    // visible rather than coincidental.
    seed_tag(store, "genre", "Rock", 1, 2, 5);
    seed_tag(store, "genre", "Emo", 1, 0, 9);
    seed_tag(store, "genre", "Pop", 1, 5, 1);
}

fn tag_values(tags: &[nd_db::tag_repo::Tag]) -> Vec<&str> {
    tags.iter().map(|t| t.tag_value.as_str()).collect()
}

#[test]
fn tag_hostile_sorts_are_dropped_and_the_aggregate_order_is_stable() {
    let (store, path) = fresh_store("nd-db-gate-tag-");
    seed_tags_for_sort_gate(&store);

    let baseline = lease(&store, |conn| {
        TagRepo::new(conn).get_all("genre", Some(ADM), None)
    })
    .expect("unsorted aggregate read");
    // No ORDER BY leg, but the aggregate is NOT in insertion order: `GROUP BY tag.id, …`
    // is satisfied by the primary-key index, so SQLite emits the groups in `tag.id`
    // order. Measured, and the baseline every arm below is compared against.
    assert_eq!(tag_values(&baseline), vec!["Emo", "Pop", "Rock"]);

    // Non-vacuity, measured against the real tables.
    assert_eq!(
        raw_order(
            &store,
            "SELECT tag.tag_value FROM tag LEFT JOIN library_tag on library_tag.tag_id = tag.id \
             ORDER BY library_tag.media_file_count asc"
        ),
        vec!["Pop", "Rock", "Emo"],
        "the media_file_count leg really reorders the dictionary"
    );
    assert_eq!(
        raw_order(
            &store,
            "SELECT tag.tag_value FROM tag LEFT JOIN library_tag on library_tag.tag_id = tag.id \
             ORDER BY library_tag.album_count desc"
        ),
        vec!["Pop", "Rock", "Emo"],
        "the album_count desc leg really reorders the dictionary"
    );

    let hostile = [
        "library_tag.media_file_count asc",
        "library_tag.album_count desc",
        // `desc` so the matching tag surfaces FIRST: the baseline above has it last, so
        // the pre-fix order really differs from the stable post-fix order.
        "(CASE WHEN tag_value LIKE '%oc%' THEN 1 ELSE 0 END) desc",
    ];
    let baseline_values = tag_values(&baseline);
    for sort in hostile {
        let raw = QueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..QueryOptions::default()
        };
        let (raw_sql, raw_args) =
            nd_db::tag_repo::get_all_sql("genre", Some(ADM), Some(&raw), None).expect("renders");
        let _ = &raw_sql;
        assert!(
            raw_sql.contains("ORDER BY"),
            "the unsanitized sort must reach ORDER BY: {raw_sql}"
        );
        let pre_fix = lease(&store, |conn| {
            rusqlite::Connection::prepare(conn, &raw_sql).map(|mut stmt| {
                stmt.query_map(rusqlite::params_from_iter(raw_args.iter()), |row| {
                    row.get::<_, String>("tag_value")
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect::<Vec<String>>()
            })
        });
        let leaked = pre_fix.unwrap_or_else(|e| panic!("pre-fix must execute: {raw_sql}: {e}"));
        assert_ne!(
            leaked, baseline_values,
            "pre-fix the leaked leg left the aggregate order untouched, so the stability \
             result below would be vacuous: {raw_sql}"
        );
        if sort.contains("CASE") {
            assert_eq!(
                leaked.first().map(String::as_str),
                Some("Rock"),
                "pre-fix the `%oc%` predicate floats the matching tag to the front — the \
                 value oracle this gate closes: {raw_sql}"
            );
        }

        let tags = lease(&store, |conn| {
            TagRepo::new(conn).get_all(
                "genre",
                Some(ADM),
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: "asc".to_string(),
                    ..RestQueryOptions::default()
                }),
            )
        })
        .unwrap_or_else(|e| panic!("hostile sort {sort:?} must be dropped, not errored: {e}"));
        assert_eq!(
            tag_values(&tags),
            baseline_values,
            "sort {sort:?} changed the aggregate order"
        );
        let parsed = nd_db::tag_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        }));
        assert_eq!(parsed.sort, "", "sort {sort:?} must be dropped");
        let gated = nd_db::tag_repo::get_all_sql("genre", Some(ADM), Some(&parsed), None)
            .expect("gated statement materializes");
        assert!(
            !gated.0.contains("ORDER BY"),
            "no ORDER BY leg may survive: {}",
            gated.0
        );
        assert!(
            !gated.0.contains("secret") && !gated.0.contains("%oc%"),
            "{}",
            gated.0
        );
        assert!(parsed.filters.is_none(), "no filter may be smuggled");
    }

    // The whitelisted/mapped sort still orders through the executed read:
    // `setSortMappings({"name": "tag_value"})` (`sql_tags.go:159-161`).
    let tags = lease(&store, |conn| {
        TagRepo::new(conn).get_all(
            "genre",
            Some(ADM),
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            }),
        )
    })
    .expect("mapped sort read");
    assert_eq!(
        tag_values(&tags),
        vec!["Rock", "Pop", "Emo"],
        "tag_value desc"
    );
    let (sql, _) = nd_db::tag_repo::get_all_sql(
        "genre",
        Some(ADM),
        Some(&nd_db::tag_repo::parse_rest_options(Some(
            &RestQueryOptions {
                sort: "name".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            },
        ))),
        None,
    )
    .expect("materializes");
    assert!(
        sql.ends_with("GROUP BY tag.id, tag.tag_name, tag.tag_value ORDER BY tag_value desc"),
        "{sql}"
    );

    // Count invariant under the same options: Go's `Count` builds its own aggregate
    // (`sql_tags.go:203`) and never calls `applyOptions`.
    let plain = lease(&store, |conn| TagRepo::new(conn).count(Some(ADM), None)).expect("count");
    assert_eq!(plain, 3);
    for sort in hostile {
        let count = lease(&store, |conn| {
            TagRepo::new(conn).count(
                Some(ADM),
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: "desc".to_string(),
                    max: 1,
                    offset: 2,
                    ..RestQueryOptions::default()
                }),
            )
        })
        .unwrap_or_else(|e| panic!("count under sort {sort:?} must not fail: {e}"));
        assert_eq!(count, plain, "sort {sort:?} changed the count");
    }
    let (sql, _) = nd_db::tag_repo::count_all_sql(
        Some(ADM),
        Some(&nd_db::tag_repo::parse_rest_options(Some(
            &RestQueryOptions {
                sort: "library_tag.media_file_count asc".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            },
        ))),
    )
    .expect("count materializes");
    assert!(sql.ends_with("ORDER BY tag.id"), "{sql}");

    // Ownership/visibility logic is untouched by option parsing: the INNER join still
    // removes a tag the user's libraries cannot see.
    // `library_tag.library_id` has a FK to `library`, so library 2 exists first.
    seed_library(&store, 2, "Beta", "/music-B", 0);
    seed_tag(&store, "genre", "OnlyLib2", 2, 9, 99);
    let scoped = lease(&store, |conn| {
        TagRepo::new(conn).get_all(
            "genre",
            Some(ADM),
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
        )
    })
    .expect("scoped read");
    assert_eq!(tag_values(&scoped), vec!["Emo", "Pop", "Rock"]);
    let count = lease(&store, |conn| TagRepo::new(conn).count(Some(ADM), None)).expect("count");
    assert_eq!(
        count, 3,
        "the inaccessible library's tag stays out of the count"
    );
    std::fs::remove_file(path).ok();
}

// -------------------------------------------------------------------------- player

const KEY_A: &str = "nds_0123456789abcdefghijkl";
const KEY_B: &str = "nds_ABCDEFGHIJKLMNOPQRSTUV";

fn insert_user(store: &SQLStore, id: &str, name: &str) {
    insert(
        store,
        "INSERT INTO user(id, user_name, name, email, password, is_admin, created_at, \
         updated_at) VALUES(?,?,?,?,?,?,?,?)",
        &[
            Value::Text(id.to_string()),
            Value::Text(name.to_string()),
            Value::Text(name.to_string()),
            Value::Text(format!("{name}@example.com")),
            Value::Text("encrypted".to_string()),
            Value::Integer(0),
            Value::Text(T0.to_string()),
            Value::Text(T1.to_string()),
        ],
    );
}

fn insert_player(
    store: &SQLStore,
    id: &str,
    user_id: &str,
    client: &str,
    name: &str,
    max_bit_rate: i64,
    api_key_hash: Option<&str>,
) {
    insert(
        store,
        &format!(
            "INSERT INTO {PLAYER_TABLE}(id, name, user_agent, user_id, client, ip, \
             last_seen, max_bit_rate, transcoding_id, report_real_path, scrobble_enabled, \
             api_key_hash) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)"
        ),
        &[
            Value::Text(id.to_string()),
            Value::Text(name.to_string()),
            Value::Text("agent".to_string()),
            Value::Text(user_id.to_string()),
            Value::Text(client.to_string()),
            Value::Text("127.0.0.1".to_string()),
            Value::Text(T0.to_string()),
            Value::Integer(max_bit_rate),
            Value::Text(String::new()),
            Value::Integer(0),
            Value::Integer(1),
            match api_key_hash {
                Some(h) => Value::Text(h.to_string()),
                None => Value::Null,
            },
        ],
    );
}

fn owner(id: &str) -> LoggedUser {
    LoggedUser {
        id: id.to_string(),
        is_admin: false,
    }
}

fn admin_user() -> LoggedUser {
    LoggedUser {
        id: "u1".to_string(),
        is_admin: true,
    }
}

fn player_ids(list: &[nd_db::player_repo::PlayerRecord]) -> Vec<&str> {
    list.iter().map(|p| p.id.as_str()).collect()
}

/// Three players whose rowid order, `max_bit_rate` order and `api_key_hash` order are all
/// different, so a leaked `api_key_hash` leg is a measurable side channel.
fn seed_players_for_sort_gate(store: &SQLStore) {
    insert_user(store, "u1", "jdoe");
    insert_user(store, "u2", "other");
    insert_player(store, "p1", "u1", "subsonic", "a", 320, Some(KEY_A));
    insert_player(store, "p2", "u2", "dsub", "b", 64, None);
    insert_player(store, "p3", "u1", "NavidromeUI", "c", 128, Some(KEY_B));
}

#[test]
fn player_hostile_sorts_cannot_oracle_the_api_key_hash() {
    let (store, path) = fresh_store("nd-db-gate-player-");
    seed_players_for_sort_gate(&store);

    // Baseline: options == None — no ORDER BY, so SQLite's rowid order.
    let baseline = lease(&store, |conn| {
        PlayerRepo::new(conn).read_all(None, &admin_user())
    })
    .expect("unsorted read_all");
    assert_eq!(player_ids(&baseline), vec!["p1", "p2", "p3"]);

    // Non-vacuity, measured: `api_key_hash` really reorders the rows, so the stability
    // below is a real result and the pre-fix leak was a bit-at-a-time oracle on which
    // players hold a key.
    assert_eq!(
        raw_order(
            &store,
            "SELECT id FROM player ORDER BY api_key_hash IS NOT NULL desc, id asc"
        ),
        vec!["p1", "p3", "p2"],
        "the api_key_hash predicate would flip order if it reached ORDER BY"
    );

    let hostile = [
        // `build_sort_order` takes the first space-token as the column and the second as
        // the direction (`sql_base_repository.go:213-224`), so this executes pre-fix even
        // though `api_key_hash` is not a whitelisted field.
        "api_key_hash is not null",
        // A mixed comma list — dropped as a whole by `sanitize_sort` — whose first part is
        // the secret column.
        "api_key_hash, max_bit_rate",
        // The CASE shape, kept as ONE field by the paren-aware splitter
        // (`sql_base_repository.go:230-245`).
        "(CASE WHEN api_key_hash IS NOT NULL THEN 1 ELSE 0 END)",
    ];
    for sort in hostile {
        // PRE-FIX: the executed read forwarded a caller-built `Options` verbatim to
        // `read_all_sql`, so drive that renderer with the unsanitized sort.
        let raw = Options {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..Options::default()
        };
        let (raw_sql, raw_args) = nd_db::player_repo::read_all_sql(Some(&raw), &admin_user());
        assert!(
            raw_sql.contains("ORDER BY"),
            "the unsanitized sort must reach ORDER BY: {raw_sql}"
        );
        let pre_fix = lease(&store, |conn| {
            rusqlite::Connection::prepare(conn, &raw_sql).map(|mut stmt| {
                stmt.query_map(rusqlite::params_from_iter(raw_args.iter()), |row| {
                    row.get::<_, String>("id")
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect::<Vec<String>>()
            })
        });
        let leaked = pre_fix.unwrap_or_else(|e| panic!("pre-fix must execute: {raw_sql}: {e}"));
        assert_ne!(
            leaked,
            player_ids(&baseline),
            "pre-fix the leaked api_key_hash leg left the row order untouched, so the oracle \
             below would be vacuous: {raw_sql}"
        );
        // `api_key_hash` is NULL for exactly one player (p2) and SQLite sorts NULL first
        // ascending, last descending — so whichever direction `build_sort_order` derived,
        // p2 leaves its rowid slot and its position IS the key-possession bit.
        let p2_at = leaked
            .iter()
            .position(|id| id == "p2")
            .unwrap_or(usize::MAX);
        assert_ne!(
            p2_at, 1,
            "pre-fix the keyless player kept its rowid slot, so the possession oracle would \
             be vacuous: {raw_sql}"
        );

        // POST-FIX through the executed read, for a NON-admin too.
        for user in [admin_user(), owner("u2")] {
            let rows = lease(&store, |conn| {
                PlayerRepo::new(conn).read_all(
                    Some(&RestQueryOptions {
                        sort: sort.to_string(),
                        order: "asc".to_string(),
                        ..RestQueryOptions::default()
                    }),
                    &user,
                )
            })
            .unwrap_or_else(|e| panic!("hostile sort {sort:?} must be dropped: {e}"));
            if user.is_admin {
                assert_eq!(
                    player_ids(&rows),
                    player_ids(&baseline),
                    "sort {sort:?} changed the admin's row order"
                );
            } else {
                assert_eq!(player_ids(&rows), vec!["p2"], "ownership is unchanged");
            }
        }
        let parsed = Options::from_rest(Some(&RestQueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        }));
        assert_eq!(parsed.sort, "", "sort {sort:?} must be dropped");
        let gated = nd_db::player_repo::read_all_sql(Some(&parsed), &admin_user());
        // `api_key_hash` legitimately appears in the SELECT projection
        // (`player_repository.go:49`), so the check is on the ORDER BY leg: none.
        assert!(!gated.0.contains("ORDER BY"), "{}", gated.0);
        assert!(parsed.filters.is_none(), "no filter may be smuggled");
    }
    std::fs::remove_file(path).ok();
}

#[test]
fn player_mapped_and_whitelisted_sorts_order_and_count_is_invariant() {
    let (store, path) = fresh_store("nd-db-gate-player-");
    seed_players_for_sort_gate(&store);

    // `user_name` → `username` mapping (`player_repository.go:29-31`), as the camelCase
    // API spelling.
    let rows = lease(&store, |conn| {
        PlayerRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "userName".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            }),
            &admin_user(),
        )
    })
    .expect("mapped sort");
    assert_eq!(player_ids(&rows), vec!["p2", "p1", "p3"], "username desc");

    // A whitelisted field (`MaxBitRate`) orders numerically.
    for (sort, order, expected) in [
        ("maxBitRate", "asc", vec!["p2", "p3", "p1"]),
        ("maxBitRate", "desc", vec!["p1", "p3", "p2"]),
        ("client", "asc", vec!["p3", "p2", "p1"]),
    ] {
        let rows = lease(&store, |conn| {
            PlayerRepo::new(conn).read_all(
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: order.to_string(),
                    ..RestQueryOptions::default()
                }),
                &admin_user(),
            )
        })
        .unwrap_or_else(|e| panic!("whitelisted sort {sort:?}/{order:?} failed: {e}"));
        assert_eq!(player_ids(&rows), expected, "sort {sort:?} {order:?}");
    }

    // The page leg survives the gate unchanged.
    let page = lease(&store, |conn| {
        PlayerRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "maxBitRate".to_string(),
                order: "asc".to_string(),
                max: 1,
                offset: 1,
                ..RestQueryOptions::default()
            }),
            &admin_user(),
        )
    })
    .expect("paged read");
    assert_eq!(player_ids(&page), vec!["p3"]);

    // Count: unchanged by sort and page, changed by the `hasapikey` filter.
    let plain = lease(&store, |conn| {
        PlayerRepo::new(conn).count(None, &admin_user())
    })
    .expect("count");
    assert_eq!(plain, 3);
    for sort in ["api_key_hash is not null", "maxBitRate", "userName"] {
        let count = lease(&store, |conn| {
            PlayerRepo::new(conn).count(
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: "desc".to_string(),
                    max: 1,
                    offset: 2,
                    ..RestQueryOptions::default()
                }),
                &admin_user(),
            )
        })
        .unwrap_or_else(|e| panic!("count under sort {sort:?} must not fail: {e}"));
        assert_eq!(count, plain, "sort {sort:?} changed the count");
    }
    let keyed = lease(&store, |conn| {
        PlayerRepo::new(conn).count(
            Some(&RestQueryOptions {
                sort: "api_key_hash is not null".to_string(),
                order: "asc".to_string(),
                filters: [("hasapikey".to_string(), "true".to_string())]
                    .into_iter()
                    .collect(),
                ..RestQueryOptions::default()
            }),
            &admin_user(),
        )
    })
    .expect("keyed count");
    assert_eq!(keyed, 2, "the hasapikey filter still applies");

    // Ownership is unchanged on the count too: `newRestSelect`'s restriction
    // (`player_repository.go:72-75`).
    let scoped = lease(&store, |conn| {
        PlayerRepo::new(conn).count(None, &owner("u2"))
    })
    .expect("scoped count");
    assert_eq!(scoped, 1);
    std::fs::remove_file(path).ok();
}

// --------------------------------------------------------------------------- radio

/// Four radios whose rowid order differs from a `stream_url`-derived order, so the
/// hostile predicate is demonstrably non-vacuous even though this repository's renderer
/// emits no ORDER BY leg (documented residual, `hydrate-contract.md` §S-D3f).
fn seed_radios_for_sort_gate(store: &SQLStore) {
    for (id, url) in [
        ("ra-1", "https://x/secret-a"),
        ("ra-2", "https://x/public"),
        ("ra-3", "https://x/secret-c"),
        ("ra-4", "https://x/public"),
    ] {
        insert(
            store,
            "INSERT INTO radio (id, name, stream_url, home_page_url, created_at, updated_at, \
             uploaded_image) VALUES (?, ?, ?, '', ?, ?, '')",
            &[
                Value::Text(id.to_string()),
                Value::Text(id.to_string()),
                Value::Text(url.to_string()),
                Value::Text(T0.to_string()),
                Value::Text(T1.to_string()),
            ],
        );
    }
}

fn radio_ids(
    outcome: &nd_db::artwork_hydration::ReadOutcome<radio_repo::RadioRecord>,
) -> Vec<&str> {
    outcome.items.iter().map(|r| r.id.as_str()).collect()
}

#[test]
fn radio_hostile_sorts_and_filters_are_dropped_before_the_statement() {
    let (store, path) = fresh_store("nd-db-gate-radio-");
    seed_radios_for_sort_gate(&store);

    let baseline = lease(&store, |conn| RadioRepo::new(conn).get_all(None)).expect("GetAll");
    assert_eq!(radio_ids(&baseline), vec!["ra-1", "ra-2", "ra-3", "ra-4"]);

    // Non-vacuity, measured against the real table: the predicate really reorders rows
    // when it reaches ORDER BY.
    assert_eq!(
        raw_order(
            &store,
            "SELECT id FROM radio ORDER BY (CASE WHEN stream_url LIKE '%secret-a%' THEN 1 \
             ELSE 0 END) asc"
        ),
        vec!["ra-2", "ra-3", "ra-4", "ra-1"],
        "the CASE predicate moves the secret-carrying radio last"
    );

    let hostile = [
        "(CASE WHEN stream_url LIKE '%secret-a%' THEN 1 ELSE 0 END)",
        "stream_url%2 asc",
        "id, stream_url%2",
    ];
    for sort in hostile {
        // PRE-FIX: `get_all` forwarded a caller-built QueryOptions to `get_all_sql`, so
        // the sort reached the statement text verbatim (and a raw clause could reach
        // WHERE).
        let raw = QueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..QueryOptions::default()
        };
        let (raw_sql, _) = radio_repo::get_all_sql(Some(&raw)).expect("materializes");
        // This renderer has NO sort leg, so the sort text never appears — the leak that
        // mattered here is the unsanitized FILTER channel, pinned below. The gate still
        // drops the sort before the renderer sees it.
        assert!(
            !raw_sql.contains(sort),
            "radio's renderer drops no leg because it renders none: {raw_sql}"
        );

        let outcome = lease(&store, |conn| {
            RadioRepo::new(conn).get_all(Some(&RestQueryOptions {
                sort: sort.to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }))
        })
        .unwrap_or_else(|e| panic!("hostile sort {sort:?} must be dropped, not errored: {e}"));
        assert_eq!(
            radio_ids(&outcome),
            radio_ids(&baseline),
            "sort {sort:?} changed row order"
        );
        assert_eq!(outcome.artwork_error, None, "hydration still ran cleanly");

        let parsed = radio_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        }));
        assert_eq!(
            parsed.sort, "",
            "sort {sort:?} must be dropped by sanitize_sort"
        );
        let gated = radio_repo::get_all_sql(Some(&parsed)).expect("materializes");
        assert!(!gated.0.contains("ORDER BY"), "{}", gated.0);
        assert!(!gated.0.to_lowercase().contains("case"), "{}", gated.0);
        assert!(!gated.0.contains("secret-a"), "{}", gated.0);
        assert!(parsed.filters.is_none(), "no filter may be smuggled");
    }

    // The channel this change really closes on radio: a caller-built filter reached
    // WHERE verbatim pre-fix. A non-whitelisted REST filter is now dropped outright
    // (`sql_restful.go:38-42`), exactly like Go.
    let smuggled = QueryOptions {
        filters: Some(vec![Filter::Raw {
            sql: "stream_url LIKE '%secret-a%'".to_string(),
            raw_args: Vec::new(),
        }]),
        ..QueryOptions::default()
    };
    let (raw_sql, _) = radio_repo::get_all_sql(Some(&smuggled)).expect("materializes");
    assert!(
        raw_sql.contains("stream_url LIKE '%secret-a%'"),
        "the raw clause reaches WHERE verbatim pre-fix: {raw_sql}"
    );
    let pre_fix = lease(&store, |conn| {
        nd_db::execution::query_all(conn, &(raw_sql.clone(), Vec::new()), |row| {
            row.get::<_, String>("id")
        })
    })
    .expect("the pre-fix statement executes");
    assert_eq!(
        pre_fix,
        vec!["ra-1"],
        "the smuggled clause narrowed the read to the one radio whose stream URL carries the \
         secret — a row-set oracle on a column that is not filterable"
    );

    // A filter key that is neither a `model.Radio` field nor a filter mapping. In Go it
    // is dropped by the `parseRestFilters` whitelist arm (`sql_restful.go:39-42`); a
    // caller who skipped the parser could instead hand the renderer a `Filter::Raw` over
    // ANY column — the smuggled clause measured above. `stream_url` itself IS whitelisted
    // (`registerModelWhiteList(&model.Radio{})`, `sql_restful.go:142-163`), which is why
    // the smuggle above used it and this drop-arm uses a key no column backs.
    let gated = lease(&store, |conn| {
        RadioRepo::new(conn).get_all(Some(&RestQueryOptions {
            filters: [("api_key_hash".to_string(), "%secret-a%".to_string())]
                .into_iter()
                .collect(),
            ..RestQueryOptions::default()
        }))
    })
    .expect("non-whitelisted filter read");
    assert_eq!(
        radio_ids(&gated),
        vec!["ra-1", "ra-2", "ra-3", "ra-4"],
        "api_key_hash is not a Radio field, so the filter is dropped and all rows come back"
    );
    assert!(
        radio_repo::parse_rest_options(Some(&RestQueryOptions {
            filters: [("api_key_hash".to_string(), "%secret-a%".to_string())]
                .into_iter()
                .collect(),
            ..RestQueryOptions::default()
        }))
        .filters
        .is_none(),
        "the un-whitelisted filter must leave no clause at all — pre-fix a raw clause over \
         a secret column reached WHERE and returned only ra-1"
    );

    // The registered `name` mapping still filters (`radio_repository.go:22-24`).
    let named = lease(&store, |conn| {
        RadioRepo::new(conn).get_all(Some(&RestQueryOptions {
            filters: [("name".to_string(), "ra-2".to_string())]
                .into_iter()
                .collect(),
            ..RestQueryOptions::default()
        }))
    })
    .expect("mapped filter read");
    assert_eq!(radio_ids(&named), vec!["ra-2"]);

    // Count invariant under the same hostile options; the `name` filter still narrows it.
    let plain = lease(&store, |conn| RadioRepo::new(conn).count(None)).expect("count");
    assert_eq!(plain, 4);
    for sort in hostile {
        let count = lease(&store, |conn| {
            RadioRepo::new(conn).count(Some(&RestQueryOptions {
                sort: sort.to_string(),
                order: "desc".to_string(),
                max: 1,
                offset: 3,
                ..RestQueryOptions::default()
            }))
        })
        .unwrap_or_else(|e| panic!("count under sort {sort:?} must not fail: {e}"));
        assert_eq!(count, plain, "sort {sort:?} changed the count");
    }
    let named_count = lease(&store, |conn| {
        RadioRepo::new(conn).count(Some(&RestQueryOptions {
            sort: "(CASE WHEN stream_url LIKE '%secret-a%' THEN 1 ELSE 0 END)".to_string(),
            filters: [("name".to_string(), "ra-2".to_string())]
                .into_iter()
                .collect(),
            ..RestQueryOptions::default()
        }))
    })
    .expect("filtered count");
    assert_eq!(named_count, 1, "the name filter still applies to the count");
    std::fs::remove_file(path).ok();
}
