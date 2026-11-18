//! S-D3f follow-on (decision D-PLAYER-EDIT): the transcoding repository's READ
//! side EXECUTED against a real SQLite DB — fresh schema via `nd_dbschema`, one
//! isolated temp file per test, the harness shape copied from
//! `tests/runtime_player_repo.rs:33-46` and the lease/insert idioms from
//! `tests/runtime_hydrate_playlist_radio_artwork.rs:94-126`.
//!
//! Go truth under test: `persistence/transcoding_repository.go` `Get` (`:23-28`),
//! `FindByFormat` (`:34-39`), `Read` (`:53-62`), `ReadAll` (`:64-77`), `CountAll`
//! (`:30-31`) and `Count` (`:49-51`), plus the base layer they reach through:
//! `queryOne`'s `sql.ErrNoRows` → `model.ErrNotFound` mapping
//! (`sql_base_repository.go:375-388`), `queryAll`'s empty-slice-not-NotFound
//! behavior (`:441-457`, `dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), `count`'s
//! reset `ORDER BY transcoding.id` (`:565-575`), `applyOptions`' sort leg
//! (`:149-162`) and `parseRestOptions`' whitelist (`sql_restful.go:54-67`), which
//! `ReadAll` runs INSIDE the method (`transcoding_repository.go:65`) — so
//! [`TranscodingRepo::read_all`] takes the REST shape and sanitizes it itself.
//!
//! The claims this file pins, each executed:
//! (1) `Get`/`FindByFormat` return the REAL command — masking there would break
//!     `core/players.go:87-91` and `core/stream/decider.go:315,330`; Go pins the
//!     same for a regular user (`transcoding_repository_test.go:111-129`);
//! (2) `Read`/`ReadAll` blank `command` for a non-admin and keep every row
//!     (`:58-59`, `:71-74`); the admin keeps it;
//! (3) a missing id is `NotFound` with the text `data not found`
//!     (`model/errors.go:10` → `rest.ErrNotFound`, `deluan/rest@v1.0.1/errors.go:14`);
//! (4) an empty table reads as an empty `Vec`, never NotFound, and counts 0;
//! (5) `options.sort` reaches a real ORDER BY (asc and desc over seeded rows), a
//!     non-whitelisted sort is dropped, and a page limits/offsets;
//! (6) `count` equals the row count and is invariant under sort and page, while
//!     filters survive it;
//! (7) a NULL `default_bit_rate` is a SCAN error — not NotFound, not 0 — because
//!     Go scans it into a non-pointer `int` (`model/transcoding.go:14`);
//! (8) the DDL's `unique (name)`/`unique (target_format)` (`schema_ddl.rs:56`)
//!     surface as SQLite constraint errors, not NotFound;
//! (9) the offset-without-limit page EXECUTES (`LIMIT -1 OFFSET n`), the builder's
//!     own cited deviation (`sqlrepo.rs:313-319`) where Go's bare `OFFSET` is a
//!     SQLite parse error;
//! (10) a whitelisted-but-absent annotation sort column (`starred`) errors in
//!     SQLite exactly as it does in Go — reported, not silently dropped;
//! (11) the write legs' non-admin permission behavior is unchanged;
//! (12) a HOSTILE sort expression referencing `command` is dropped by the option
//!     parsing `read_all` does itself, so it cannot flip a non-admin's row order — the
//!     masked-field side channel this slice closes — while a whitelisted sort still
//!     orders correctly.

use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_restful::{eq_filter, Filter, QueryOptions, RestQueryOptions};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::transcoding_repo::{
    count_sql, find_by_format_sql, get_sql, read_all_sql, Transcoding, TranscodingError,
    TranscodingRepo, ERR_PERMISSION_DENIED, TABLE,
};
use nd_dbschema::schema::apply_fresh_schema;

/// One isolated on-disk DB per invocation: a unique temp path (lands in /tmp via
/// `nd_utils::files::temp_file_name`), freshly migrated, pooled so
/// `SEEDEDRAND`/`NATSORT` are registered per connection.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-transcoding-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one repository call on a lease; the store's own boundary error is
/// unwrapped, the repository's `TranscodingError` handed back untouched.
fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, TranscodingError>,
) -> Result<T, TranscodingError> {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Insert a row straight to SQL: `Put` is out of this slice's scope (it mints no
/// id — see `put_sql`), and the NULL-bit-rate case needs a raw NULL anyway.
fn insert_transcoding(
    store: &SQLStore,
    id: &str,
    name: &str,
    target_format: &str,
    command: &str,
    default_bit_rate: Option<i64>,
) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {TABLE}(id, name, target_format, command, default_bit_rate) \
                     VALUES(?,?,?,?,?)"
                ),
                rusqlite::params![id, name, target_format, command, default_bit_rate],
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("transcoding insert failed: {e}"));
}

/// Tear down the temp DB once the store released its pool.
fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn get_and_find_by_format_sql_texts_are_unchanged() {
    // Arrange/Act: the two Eq selects.
    // Assert: identical text and binds to the pre-execution golden, so making the
    // reads EXECUTE changed no statement.
    assert_eq!(
        get_sql("t1"),
        (
            "SELECT * FROM transcoding WHERE id = ?".to_string(),
            vec![Value::Text("t1".to_string())]
        )
    );
    assert_eq!(
        find_by_format_sql("mp3"),
        (
            "SELECT * FROM transcoding WHERE target_format = ?".to_string(),
            vec![Value::Text("mp3".to_string())]
        )
    );
    // Assert: neither carries an ORDER BY or a page leg (Go's `:24`, `:35`).
    assert!(!get_sql("t1").0.contains("ORDER BY"));
    assert!(!find_by_format_sql("mp3").0.contains("LIMIT"));
}

#[test]
fn count_sql_resets_the_order_and_drops_the_page_but_keeps_filters() {
    // Arrange: `count` (`sql_base_repository.go:565-575`) with a filter, a page and
    // a caller sort.
    let options = QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        max: 5,
        offset: 3,
        filters: Some(vec![eq_filter("target_format", "mp3")]),
        ..QueryOptions::default()
    };
    // Act:
    let (sql, args) = count_sql(Some(&options));
    // Assert: the reset `ORDER BY transcoding.id` is the only order leg and the
    // page is gone; the filter bind is the sole argument.
    assert_eq!(
        sql,
        "SELECT count(distinct transcoding.id) as count FROM transcoding \
         WHERE target_format = ? ORDER BY transcoding.id"
    );
    assert_eq!(args, vec![Value::Text("mp3".to_string())]);
}

#[test]
fn read_all_sql_renders_filters_then_order_by_then_page() {
    // Arrange: filters, a sort and a page.
    let options = QueryOptions {
        sort: "targetFormat".to_string(),
        order: "desc".to_string(),
        max: 2,
        offset: 1,
        filters: Some(vec![eq_filter("name", "mp3 audio")]),
        ..QueryOptions::default()
    };
    // Act:
    let (sql, args) = read_all_sql(Some(&options));
    // Assert: squirrel's by-kind order (WHERE → ORDER BY → LIMIT/OFFSET,
    // `sqlrepo.rs:343-372`), the ORDER BY leg now present.
    assert_eq!(
        sql,
        "SELECT * FROM transcoding WHERE name = ? ORDER BY target_format desc LIMIT 2 OFFSET 1"
    );
    assert_eq!(args, vec![Value::Text("mp3 audio".to_string())]);
}

#[test]
fn executed_round_trip_reads_every_field_including_the_command() {
    let (store, path) = fresh_store();
    insert_transcoding(
        &store,
        "t1",
        "mp3 audio",
        "mp3",
        "ffmpeg -i %s -b:a 128k",
        Some(128),
    );
    // Act: Get, unmasked by design.
    let got = on(&store, |conn| TranscodingRepo::new(conn).get("t1"));
    // Assert:
    assert_eq!(
        got.expect("the row reads"),
        Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "mp3".to_string(),
            command: "ffmpeg -i %s -b:a 128k".to_string(),
            default_bit_rate: 128,
        }
    );
    // Act: FindByFormat reads the same row through the format.
    let by_format = on(&store, |conn| {
        TranscodingRepo::new(conn).find_by_format("mp3")
    });
    // Assert:
    assert_eq!(
        by_format.expect("the format reads"),
        Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "mp3".to_string(),
            command: "ffmpeg -i %s -b:a 128k".to_string(),
            default_bit_rate: 128,
        }
    );
    cleanup(store, path);
}

#[test]
fn read_masks_a_non_admin_and_keeps_the_admin_command() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "t1", "opus", "opus", "ffmpeg -secret %s", Some(64));
    // Act: Read as a non-admin (`:56-58`).
    let masked = on(&store, |conn| TranscodingRepo::new(conn).read("t1", false));
    // Assert: the row comes back with every OTHER field intact and a blank command.
    let masked = masked.expect("a non-admin read still returns the row");
    assert_eq!(masked.command, "");
    assert_eq!(masked.id, "t1");
    assert_eq!(masked.name, "opus");
    assert_eq!(masked.target_format, "opus");
    assert_eq!(masked.default_bit_rate, 64);
    // Act/Assert: the admin keeps it.
    assert_eq!(
        on(&store, |conn| TranscodingRepo::new(conn).read("t1", true))
            .expect("admin read")
            .command,
        "ffmpeg -secret %s"
    );
    cleanup(store, path);
}

#[test]
fn get_and_find_by_format_never_mask_even_for_a_non_admin() {
    let (store, path) = fresh_store();
    insert_transcoding(
        &store,
        "streamreg",
        "temp",
        "test_format",
        "ffmpeg -secret",
        Some(64),
    );
    // Act: both reads — the ones players.go and decider.go consume. There is no
    // user parameter at all, which IS the parity: Go's Get/FindByFormat never
    // consult loggedUser (`:23-28`, `:34-39`), pinned by
    // `transcoding_repository_test.go:111-129`.
    let get = on(&store, |conn| TranscodingRepo::new(conn).get("streamreg"));
    let find = on(&store, |conn| {
        TranscodingRepo::new(conn).find_by_format("test_format")
    });
    // Assert:
    assert_eq!(get.expect("get").command, "ffmpeg -secret");
    assert_eq!(find.expect("find_by_format").command, "ffmpeg -secret");
    cleanup(store, path);
}

#[test]
fn a_missing_id_is_not_found_with_gos_message() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "t1", "mp3", "mp3", "c", Some(192));
    // Act: Get / FindByFormat / Read of ids and formats that match nothing.
    let miss = on(&store, |conn| TranscodingRepo::new(conn).get("nope"));
    let miss_format = on(&store, |conn| {
        TranscodingRepo::new(conn).find_by_format("flac")
    });
    let miss_read = on(&store, |conn| {
        TranscodingRepo::new(conn).read("nope", false)
    });
    // Assert: queryOne's ErrNoRows arm maps to model.ErrNotFound, whose text is
    // Go's "data not found"; the non-admin Read misses BEFORE any masking step
    // (`transcoding_repository.go:55-57`).
    for result in [miss, miss_format, miss_read] {
        let err = result.expect_err("a miss is an error");
        assert!(matches!(err, TranscodingError::NotFound), "{err:?}");
        assert_eq!(err.to_string(), "data not found");
    }
    cleanup(store, path);
}

#[test]
fn an_empty_table_reads_an_empty_vec_and_counts_zero() {
    let (store, path) = fresh_store();
    // Act: ReadAll and CountAll over a table with no rows.
    let rows = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(None, true)
    });
    let count = on(&store, |conn| TranscodingRepo::new(conn).count_all(None));
    // Assert: `queryAll`'s dbx `All` makes an empty non-nil slice
    // (`dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), so its ErrNoRows arm is dead
    // code — this is Ok(vec![]), never NotFound. `count` reads res.Count = 0.
    assert_eq!(rows.expect("empty read_all is Ok"), vec![]);
    assert_eq!(count.expect("empty count is Ok"), 0);
    // Act: also with options (filters/page/sort applied on top of nothing), in the REST
    // shape `read_all` parses itself.
    let options = RestQueryOptions {
        sort: "name".to_string(),
        order: "asc".to_string(),
        max: 10,
        offset: 1,
        filters: std::collections::BTreeMap::from([("name".to_string(), "x".to_string())]),
    };
    // Assert:
    assert_eq!(
        on(&store, |conn| TranscodingRepo::new(conn)
            .read_all(Some(&options), false))
        .expect("still Ok"),
        vec![]
    );
    cleanup(store, path);
}

#[test]
fn read_all_sorts_asc_and_desc_over_seeded_rows() {
    let (store, path) = fresh_store();
    // Seed three rows whose INSERT order is deliberately NOT the name order.
    insert_transcoding(&store, "3", "gamma", "flac", "c3", Some(320));
    insert_transcoding(&store, "1", "alpha", "mp3", "c1", Some(128));
    insert_transcoding(&store, "2", "beta", "opus", "c2", Some(64));
    // Act: `sort: "name"` under the admin (masking is irrelevant to the order). The
    // sort arrives as the REST shape, which `read_all` sanitizes itself — the
    // camelCase form below is the API-facing spelling Go's REST layer actually receives.
    let asc = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    let desc = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert: applyOptions' ORDER BY leg really runs (it used to be dropped).
    let names = |list: &Vec<Transcoding>| list.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&asc.expect("asc")), ["alpha", "beta", "gamma"]);
    assert_eq!(names(&desc.expect("desc")), ["gamma", "beta", "alpha"]);
    // Act: a sort on defaultBitRate resolves through the snake_case fallback.
    let by_rate = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "defaultBitRate".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert:
    assert_eq!(
        by_rate
            .expect("bit-rate sort")
            .iter()
            .map(|t| t.default_bit_rate)
            .collect::<Vec<_>>(),
        [64, 128, 320]
    );
    cleanup(store, path);
}

#[test]
fn read_all_pages_with_limit_and_offset() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "alpha", "mp3", "c1", Some(128));
    insert_transcoding(&store, "2", "beta", "opus", "c2", Some(64));
    insert_transcoding(&store, "3", "gamma", "flac", "c3", Some(320));
    // Act: page 1 of size 2 over the name order.
    let page = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "asc".to_string(),
                max: 2,
                offset: 1,
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert:
    let names = page
        .expect("page")
        .iter()
        .map(|t| t.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(names, ["beta", "gamma"]);
    // Act: the offset-without-limit page. Go's bare `OFFSET n` is a SQLite PARSE
    // error; the port emits `LIMIT -1 OFFSET n` (sqlrepo.rs:313-319, a cited
    // deviation) and therefore EXECUTES, skipping rows.
    let offset_only = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "name".to_string(),
                order: "asc".to_string(),
                offset: 2,
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert:
    assert_eq!(offset_only.expect("divergent page executes").len(), 1);
    cleanup(store, path);
}

#[test]
fn read_all_masks_every_row_and_drops_no_rows() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "alpha", "mp3", "secret1", Some(128));
    insert_transcoding(&store, "2", "beta", "opus", "secret2", Some(64));
    // Act: non-admin ReadAll (`:71-74`).
    let rows = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(None, false)
    });
    // Assert:
    let rows = rows.expect("non-admin read_all");
    assert_eq!(rows.len(), 2, "masking must not drop a row");
    for row in &rows {
        assert_eq!(row.command, "");
        assert!(!row.id.is_empty());
        assert!(row.default_bit_rate > 0);
    }
    // Act/Assert: the admin keeps both commands.
    let admin = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(None, true)
    })
    .expect("admin read_all");
    assert_eq!(
        admin.iter().map(|t| t.command.clone()).collect::<Vec<_>>(),
        ["secret1", "secret2"]
    );
    cleanup(store, path);
}

#[test]
fn count_matches_the_rows_and_is_invariant_under_sort_and_page() {
    let (store, path) = fresh_store();
    // Distinct formats: the DDL's unique (target_format) (schema_ddl.rs:56)
    // forbids two mp3 rows, which is also what makes FindByFormat single-row.
    insert_transcoding(&store, "1", "alpha", "mp3", "c1", Some(128));
    insert_transcoding(&store, "2", "beta", "mp4", "c2", Some(64));
    insert_transcoding(&store, "3", "gamma", "opus", "c3", Some(320));
    let plain = on(&store, |conn| TranscodingRepo::new(conn).count_all(None)).expect("count");
    // Assert: the aggregate counts every row.
    assert_eq!(plain, 3);
    // Act: the same count with a page and a caller sort.
    let paged = on(&store, |conn| {
        TranscodingRepo::new(conn).count_all(Some(&QueryOptions {
            sort: "name".to_string(),
            order: "desc".to_string(),
            max: 1,
            offset: 2,
            ..QueryOptions::default()
        }))
    })
    .expect("paged count");
    // Assert: `count` removes offset/limit and replaces the order (`:566-569`).
    assert_eq!(paged, 3);
    // Act: filters survive it, an Eq filter on target_format.
    let filtered = on(&store, |conn| {
        TranscodingRepo::new(conn).count_all(Some(&QueryOptions {
            filters: Some(vec![eq_filter("target_format", "mp3")]),
            ..QueryOptions::default()
        }))
    })
    .expect("filtered count");
    // Assert:
    assert_eq!(filtered, 1);
    // Act: the REST-shaped Count (`:49-51`) with a whitelisted sort, a page and a
    // whitelisted filter key.
    let rest = RestQueryOptions {
        sort: "Name".to_string(),
        order: "desc".to_string(),
        max: 1,
        offset: 1,
        filters: std::collections::BTreeMap::from([(
            "target_format".to_string(),
            "mp".to_string(),
        )]),
    };
    let rest_count =
        on(&store, |conn| TranscodingRepo::new(conn).count(Some(&rest))).expect("rest count");
    // Assert: the "starts with" arm (`sql_restful.go:48-49`) binds `mp%` and matches
    // mp3 AND mp4, and the sort/page legs change nothing (`count` drops them,
    // `sql_base_repository.go:568-569`).
    assert_eq!(rest_count, 2);
    // Act: a filter matching nothing counts 0, not NotFound.
    let none = on(&store, |conn| {
        TranscodingRepo::new(conn).count_all(Some(&QueryOptions {
            filters: Some(vec![eq_filter("target_format", "alac")]),
            ..QueryOptions::default()
        }))
    })
    .expect("zero count is Ok");
    // Assert:
    assert_eq!(none, 0);
    cleanup(store, path);
}

#[test]
fn a_null_default_bit_rate_is_a_scan_error_not_not_found() {
    let (store, path) = fresh_store();
    // The fresh DDL declares `default_bit_rate int default 192` with NO not null
    // (schema_ddl.rs:56), so a NULL cell is reachable.
    insert_transcoding(&store, "n1", "nullrate", "nul", "c", None);
    // Act: Get over the NULL row.
    let got = on(&store, |conn| TranscodingRepo::new(conn).get("n1"));
    // Assert: Go scans into a non-pointer `int`, so nil is a SCAN error
    // (`database/sql/convert.go:497-508`, the `reflect.Int…` arm's `src == nil` case at
    // `:498-500`); the port's `get_i64` reports the same,
    // NOT NotFound and NOT a zero bit rate.
    let err = got.expect_err("a NULL int column fails the scan");
    assert!(matches!(err, TranscodingError::Sqlite(_)), "{err:?}");
    assert_ne!(err.to_string(), "data not found");
    // Act: ReadAll hits the same row.
    let all = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(None, true)
    });
    // Assert:
    assert!(
        matches!(
            all.expect_err("same row fails the list scan"),
            TranscodingError::Sqlite(_)
        ),
        "list scan must fail the same way"
    );
    cleanup(store, path);
}

#[test]
fn unique_conflicts_surface_as_sqlite_errors() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "dup-name", "mp3", "c1", Some(128));
    // Act: the DDL's `unique (name)` and `unique (target_format)`
    // (schema_ddl.rs:56) reject a second row.
    let name_clash = store.with_connection_sync(|conn| {
        conn.execute(
            &format!(
                "INSERT INTO {TABLE}(id, name, target_format, command, default_bit_rate) \
                     VALUES(?,?,?,?,?)"
            ),
            rusqlite::params!["2", "dup-name", "opus", "c2", 64],
        )
    });
    let format_clash = store.with_connection_sync(|conn| {
        conn.execute(
            &format!(
                "INSERT INTO {TABLE}(id, name, target_format, command, default_bit_rate) \
                     VALUES(?,?,?,?,?)"
            ),
            rusqlite::params!["3", "other", "mp3", "c3", 64],
        )
    });
    // Assert: constraint violations, not NotFound; this is also what makes
    // FindByFormat's single-row semantics hold.
    for result in [name_clash, format_clash] {
        let err = result.expect_err("the UNIQUE rejects the duplicate");
        let text = err.to_string();
        assert!(text.contains("UNIQUE constraint failed"), "{text}");
    }
    // Act: FindByFormat still resolves the one surviving mp3 row.
    let find = on(&store, |conn| {
        TranscodingRepo::new(conn).find_by_format("mp3")
    });
    // Assert:
    assert_eq!(find.expect("single row").id, "1");
    cleanup(store, path);
}

#[test]
fn a_whitelisted_but_absent_annotation_sort_errors_like_go() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "alpha", "mp3", "c", Some(128));
    // Act: `starred` IS in Go's whitelist (registerFieldWhiteList merges
    // model.Annotations, `sql_restful.go:159-162`) but `transcoding` has no such
    // column, so the ORDER BY reaches SQLite against a missing column.
    let sorted = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "starred".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert: Go errors identically (no silent sort-drop in the base layer); the
    // port surfaces the SQLite error rather than pretending the sort was ignored.
    let err = sorted.expect_err("the whitelisted-but-absent column errors");
    assert!(matches!(err, TranscodingError::Sqlite(_)), "{err:?}");
    assert!(err.to_string().contains("no such column"), "{err}");
    // Act: a NON-whitelisted sort is dropped by the `sanitizeSort` `read_all` runs
    // itself (Go logs `Ignoring sort not whitelisted`, `sql_restful.go:74-77`), so no
    // ORDER BY leg renders and the statement runs.
    let (sort, _order) = nd_db::transcoding_repo::sanitize_sort("bogus", "desc");
    assert_eq!(sort, "", "a non-whitelisted sort is dropped");
    let dropped = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "bogus".to_string(),
                order: "desc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    });
    // Assert:
    assert_eq!(dropped.expect("no sort leg renders").len(), 1);
    cleanup(store, path);
}

#[test]
fn parsed_rest_options_drive_the_executed_filter() {
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "mp3 audio", "mp3", "c1", Some(128));
    insert_transcoding(&store, "2", "opus audio", "opus", "c2", Some(64));
    // Assert: the sanitizing entry point is the one under test here — `read_all` parses
    // the REST shape itself (`transcoding_repository.go:65`), so this call and the
    // hand-parsed one below agree.
    let rest = RestQueryOptions {
        sort: "Name".to_string(),
        order: "asc".to_string(),
        max: 5,
        offset: 0,
        filters: std::collections::BTreeMap::from([(
            "target_format".to_string(),
            "mp".to_string(),
        )]),
    };
    let rows = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(Some(&rest), true)
    })
    .expect("filtered read_all");
    // Assert: the "starts with" filter matched only the mp3 row, ORDER BY name asc.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.first().expect("row").name, "mp3 audio");
    // Assert: the parsed shape `read_all` builds internally, checked separately so a
    // regression in `parse_rest_options` is visible rather than only implied by the
    // row set above.
    let parsed = nd_db::transcoding_repo::parse_rest_options(Some(&rest));
    assert_eq!(parsed.sort, "name");
    assert_eq!(parsed.order, "asc");
    // Assert: the Filter shape itself.
    assert_eq!(
        parsed.filters,
        Some(vec![Filter::Like {
            column: "target_format".to_string(),
            pattern: "mp%".to_string(),
        }])
    );
    cleanup(store, path);
}

#[test]
fn a_hostile_sort_cannot_flip_a_non_admins_row_order() {
    // The masked-field side channel this slice closes. `command` is whitelisted, but an
    // EXPRESSION referencing it is not, and an earlier `read_all` accepted an
    // already-built `QueryOptions` — so this expression reached ORDER BY verbatim and
    // flipped row order for a caller whose `command` had already been blanked. Go cannot
    // be reached that way because `ReadAll` calls `parseRestOptions` inside the method
    // (`transcoding_repository.go:65`); `read_all` now does the same.
    let (store, path) = fresh_store();
    // Seed so that INSERT order and any command-derived order differ: only row 1 matches
    // `%secret-a%`, so a surviving CASE leg would move it away from the front.
    insert_transcoding(
        &store,
        "1",
        "alpha",
        "mp3",
        "ffmpeg -secret-alpha",
        Some(128),
    );
    insert_transcoding(&store, "2", "beta", "opus", "ffmpeg -public", Some(64));
    insert_transcoding(
        &store,
        "3",
        "gamma",
        "flac",
        "ffmpeg -secret-gamma",
        Some(320),
    );
    // Baseline: no options at all — no ORDER BY, so SQLite returns rowid (INSERT) order.
    let baseline = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(None, false)
    })
    .expect("baseline read_all");
    fn masked(list: &[Transcoding]) -> Vec<(&str, &str)> {
        list.iter()
            .map(|t| (t.id.as_str(), t.command.as_str()))
            .collect()
    }
    assert_eq!(
        masked(&baseline),
        [("1", ""), ("2", ""), ("3", "")],
        "baseline is INSERT order with every command masked"
    );
    // Every spelling of the attack that references `command` inside an expression.
    let hostile = [
        "(CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END) asc",
        "CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END",
        "command like '%secret-a%' asc",
        "(command = 'ffmpeg -secret-alpha') desc",
        // A comma list is only kept when the WHOLE snake-cased string is whitelisted,
        // so a mixed `id, command` list is dropped too (`sql_restful.go:70-77`).
        "id, command",
    ];
    for sort in hostile {
        // Act: as a NON-admin, through the executed repository call.
        let rows = on(&store, |conn| {
            TranscodingRepo::new(conn).read_all(
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: "asc".to_string(),
                    ..RestQueryOptions::default()
                }),
                false,
            )
        })
        .unwrap_or_else(|e| panic!("hostile sort {sort:?} must be dropped, not errored: {e}"));
        // Assert: the order is UNCHANGED from the no-sort baseline, so the expression
        // never reached ORDER BY and cannot act as a bit-at-a-time oracle.
        assert_eq!(
            masked(&rows),
            masked(&baseline),
            "sort {sort:?} changed row order"
        );
        // Assert: nothing is leaked — every command is blanked and the row set is intact.
        assert_eq!(rows.len(), 3, "no row may be dropped or added");
        for row in &rows {
            assert_eq!(row.command, "", "masking must still run");
        }
        // Assert: the sanitizer drops it outright, and the rendered statement carries no
        // ORDER BY leg and no fragment of the expression.
        let parsed = nd_db::transcoding_repo::parse_rest_options(Some(&RestQueryOptions {
            sort: sort.to_string(),
            order: "asc".to_string(),
            ..RestQueryOptions::default()
        }));
        assert_eq!(
            parsed.sort, "",
            "sort {sort:?} must be dropped by sanitize_sort"
        );
        let rendered = nd_db::transcoding_repo::read_all_sql(Some(&parsed)).0;
        assert!(!rendered.contains("ORDER BY"), "{rendered}");
        assert!(!rendered.to_lowercase().contains("case"), "{rendered}");
        assert!(!rendered.contains("secret-a"), "{rendered}");
        assert!(
            parsed.filters.is_none(),
            "the hostile sort must not smuggle a filter either: {:?}",
            parsed.filters
        );
    }
    // Act: the same hostile sort under an ADMIN. The drop is a whitelist property, not a
    // masking property, so it happens regardless of `is_admin` — which is also why the
    // order is unaffected even though the ORDER BY would have seen the real column.
    let admin = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "(CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END) asc".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
            true,
        )
    })
    .expect("admin read_all");
    // Assert:
    assert_eq!(
        admin.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["1", "2", "3"],
        "the admin's order is unchanged too"
    );
    assert_eq!(
        admin.iter().map(|t| t.command.as_str()).collect::<Vec<_>>(),
        [
            "ffmpeg -secret-alpha",
            "ffmpeg -public",
            "ffmpeg -secret-gamma"
        ],
        "the admin still keeps every command"
    );
    cleanup(store, path);
}

#[test]
fn a_whitelisted_sort_still_orders_correctly_for_a_non_admin() {
    // Fix (c): dropping the hostile leg must not break legitimate sorting.
    let (store, path) = fresh_store();
    insert_transcoding(&store, "3", "gamma", "flac", "c3", Some(320));
    insert_transcoding(&store, "1", "alpha", "mp3", "c1", Some(128));
    insert_transcoding(&store, "2", "beta", "opus", "c2", Some(64));
    fn ids(list: &[Transcoding]) -> Vec<&str> {
        list.iter().map(|t| t.id.as_str()).collect()
    }
    // Act/Assert: `name` asc and desc, as a NON-admin.
    for (sort, order, expected) in [
        ("name", "asc", ["1", "2", "3"]),
        ("name", "desc", ["3", "2", "1"]),
        // The camelCase API spelling resolves through `toSnakeCase` over the empty
        // SORT_MAPPINGS, i.e. the fallback rather than a player-style mapping.
        ("targetFormat", "asc", ["3", "1", "2"]),
        ("targetFormat", "desc", ["2", "1", "3"]),
        ("defaultBitRate", "asc", ["2", "1", "3"]),
    ] {
        let rows = on(&store, |conn| {
            TranscodingRepo::new(conn).read_all(
                Some(&RestQueryOptions {
                    sort: sort.to_string(),
                    order: order.to_string(),
                    ..RestQueryOptions::default()
                }),
                false,
            )
        })
        .unwrap_or_else(|e| panic!("whitelisted sort {sort} {order} failed: {e}"));
        assert_eq!(ids(&rows), expected, "sort {sort} {order}");
        for row in &rows {
            assert_eq!(row.command, "", "masking still applies to a sorted read");
        }
    }
    cleanup(store, path);
}

#[test]
fn a_bare_command_sort_orders_rows_for_a_non_admin_as_go_does() {
    // INHERITED Go behavior, pinned so it is not mistaken for a fix here: `command` is a
    // whitelisted field (`sql_restful.go:154-158`), so `sanitizeSort` KEEPS a bare
    // `sort=command` (`:70-77`) and the ORDER BY is evaluated against the real column —
    // masking happens after the query (`transcoding_repository.go:71-74`), in Go too.
    // A caller can therefore observe command ordering without seeing the text, in both
    // languages. Reported, not diverged from: diverging would mean rejecting a sort Go
    // accepts, and the whitelist is Go's only sort boundary.
    let (store, path) = fresh_store();
    insert_transcoding(&store, "1", "alpha", "mp3", "zzz", Some(128));
    insert_transcoding(&store, "2", "beta", "opus", "aaa", Some(64));
    insert_transcoding(&store, "3", "gamma", "flac", "mmm", Some(320));
    // Act:
    let rows = on(&store, |conn| {
        TranscodingRepo::new(conn).read_all(
            Some(&RestQueryOptions {
                sort: "command".to_string(),
                order: "asc".to_string(),
                ..RestQueryOptions::default()
            }),
            false,
        )
    })
    .expect("a bare command sort is accepted, as in Go");
    // Assert: ordered by the REAL command (aaa, mmm, zzz) while every command stays blank.
    assert_eq!(
        rows.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["2", "3", "1"]
    );
    for row in &rows {
        assert_eq!(row.command, "");
    }
    cleanup(store, path);
}

#[test]
fn the_write_legs_still_refuse_a_non_admin() {
    // Arrange: the mutating renderers this slice must not change.
    let t = Transcoding {
        id: "t1".to_string(),
        name: "mp3 audio".to_string(),
        target_format: "mp3".to_string(),
        command: "ffmpeg -i %s".to_string(),
        default_bit_rate: 160,
    };
    // Act/Assert: non-admin put/delete are refused before any statement is built
    // (`:43`, `:98`), unchanged by the executed-read work.
    assert_eq!(
        nd_db::transcoding_repo::put_sql(false, &t, &[]),
        Err(ERR_PERMISSION_DENIED)
    );
    assert_eq!(
        nd_db::transcoding_repo::delete_sql(false, &["t1"]),
        Err(ERR_PERMISSION_DENIED)
    );
    assert!(nd_db::transcoding_repo::put_sql(true, &t, &[]).is_ok());
    assert!(nd_db::transcoding_repo::delete_sql(true, &["t1"]).is_ok());
}
