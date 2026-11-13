//! S-D3b golden — folder + playlist hydration EXECUTED against a real SQLite
//! DB: fresh schema via `nd_dbschema`, one isolated temp file per test
//! (harness shape copied from `tests/runtime_hydrate.rs`).
//!
//! Go truth under test: `dbFolder`/`PostScan`
//! (`persistence/folder_repository.go:25-46`) and `dbPlaylist`/`PostScan`
//! (`persistence/playlist_repository.go:24-34`), the two base selects
//! (`selectFolder` `:58-62`, `selectPlaylist` `:257-260` through
//! `withAnnotation` `sql_annotations.go:63-88`), promoted onto the S-D3 shared
//! seam (`nd-db/src/hydrate.rs`). `_foreign_keys=on` is Go's `db/db.go:55` DSN
//! parameter.
//!
//! The three claims this slice is really about, each asserted against a live
//! row rather than a rendered string: (1) the JSON shim decodes — with Go's
//! exact folder prefix and Go's UN-wrapped playlist error; (2) the join-only /
//! derived columns (`library_path`, the annotation columns) land on the record
//! and are NOT emitted by the write map, so the write SQL is unchanged; (3) a
//! light query that omits the joined columns decodes to zero values instead of
//! failing.

use nd_db::folder_repo::{self, FolderRecord, FolderRow};
use nd_db::hydrate::{self, HydrateError, RowGet};
use nd_db::playlist_repo::{self, PlaylistRecord, PlaylistRow};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
const LIB_PATH: &str = "/music";

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled so `SEEDEDRAND`/`NATSORT` are registered per connection
/// (`schema-contract.md` §4).
fn store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-hydrate-fp-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// `rusqlite::types::Value` for a [`Value`] (`runtime_insert_alignment.rs:38`).
fn bind(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

/// Run one read on a lease; lease/store errors panic, the mapper's error is
/// handed back untouched for assertion.
fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<T, rusqlite::Error> {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Execute a `(sql, args)` pair through the shared execution seam.
fn query_all<T>(
    store: &SQLStore,
    statement: &(String, Vec<Value>),
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, rusqlite::Error> {
    on(store, |conn| {
        nd_db::execution::query_all(conn, statement, map)
    })
}

/// Execute a raw statement with typed args.
fn insert(store: &SQLStore, sql: &str, args: &[rusqlite::types::Value]) -> usize {
    store
        .with_connection_sync(|conn| conn.execute(sql, rusqlite::params_from_iter(args.iter())))
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
}

/// Phase 1 + phase 2 of the seam, surfaced the way Go's `ScanStruct` error
/// leaves the read (a sqlite conversion failure carrying the Go message).
fn hydrate_folder_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FolderRecord> {
    hydrate::row_to_folder(row)
        .map_err(HydrateError::Sqlite)
        .and_then(FolderRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// The same two phases for `dbPlaylist`.
fn hydrate_playlist_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaylistRecord> {
    hydrate::row_to_playlist(row)
        .map_err(HydrateError::Sqlite)
        .and_then(PlaylistRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// `folder.library_id` is NOT NULL without a default; `name`/`path` are
/// `not null unique` in the fresh DDL (`schema_ddl.rs:59,61`).
fn seed_library(store: &SQLStore) -> i64 {
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Text("Main".to_string()),
            rusqlite::types::Value::Text(LIB_PATH.to_string()),
            rusqlite::types::Value::Text(String::new()),
        ],
    );
    1
}

/// `playlist.owner_id` is NOT NULL and references `user`.
fn seed_user(store: &SQLStore, id: &str, user_name: &str) {
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text(id.to_string()),
            rusqlite::types::Value::Text(user_name.to_string()),
            rusqlite::types::Value::Text(user_name.to_string()),
            rusqlite::types::Value::Text(format!("{user_name}@x.y")),
            rusqlite::types::Value::Text("CIPHER".to_string()),
            rusqlite::types::Value::Integer(0),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
        ],
    );
}

/// A folder written through the record-driven write path, so the `image_files`
/// cell holds exactly what `PostMapArgs` marshals.
fn seed_folder(store: &SQLStore, lib_id: i64, id: &str, image_files: Option<&[String]>) {
    let record = FolderRecord {
        id: id.to_string(),
        library_id: lib_id,
        path: "music/rock".to_string(),
        name: "Rock".to_string(),
        parent_id: "p-abc".to_string(),
        num_audio_files: 3,
        num_playlists: 2,
        image_files: image_files.map(<[String]>::to_vec),
        images_updated_at: stamp(T1),
        hash: "H-1".to_string(),
        missing: false,
        updated_at: stamp(T1),
        created_at: Some(stamp(T0)),
        // Join-only, hydration-owned.
        library_path: String::new(),
    };
    let put = folder_repo::put_sql(id, &record, stamp(T1), &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(store, &put.insert.0, &args);
}

fn stamp(text: &str) -> chrono::DateTime<chrono::Utc> {
    nd_db::time::parse(text).unwrap_or_else(|| panic!("{text} parses"))
}

/// Base-read hydrate of one folder through `select_folder` (`Get`).
fn hydrate_folder(store: &SQLStore, id: &str) -> Result<FolderRecord, rusqlite::Error> {
    Ok(
        query_all(store, &folder_repo::get_sql(id, None), hydrate_folder_row)?
            .pop()
            .expect("one folder row"),
    )
}

/// Base-read hydrate of one playlist through `selectPlaylist` for `user_id`.
fn hydrate_playlist(
    store: &SQLStore,
    id: &str,
    user_id: &str,
    user: &nd_db::playlist_repo::User,
) -> Result<PlaylistRecord, rusqlite::Error> {
    Ok(query_all(
        store,
        &playlist_repo::get_sql(id, user_id, user),
        hydrate_playlist_row,
    )?
    .pop()
    .expect("one playlist row"))
}

/// Go's base read carries `image_files` (decoded by the shim), `library_path`
/// from the INNER JOIN, the row's own `id`, and Go's zero `UpdateAt` —
/// `ScanStruct` maps by field name, so `UpdateAt` → `update_at` and the selected
/// `updated_at` column has no scan target (`dbx@v1.12.0/rows.go:60-85`). The
/// join-only column must NOT reach the write map, so the write legs stay
/// byte-identical.
#[test]
fn folder_base_read_hydrates_image_files_library_path_and_id() {
    // Arrange:
    let (store, path) = store();
    let lib = seed_library(&store);
    seed_folder(
        &store,
        lib,
        "f-1",
        Some(&["cover.jpg".to_string(), "back.jpg".to_string()]),
    );

    // Act:
    let got = hydrate_folder(&store, "f-1").expect("base read hydrates");

    // Assert: the shim decoded onto the record.
    assert_eq!(
        got.image_files,
        Some(vec!["cover.jpg".to_string(), "back.jpg".to_string()]),
        "image_files decodes through sql_json::unmarshal_image_files"
    );
    // Assert: Go's `UpdateAt` is NOT hydrated from the selected `updated_at`
    // (dbx snake-cases the FIELD name — `UpdateAt` → `update_at`, and no select
    // emits `update_at`), so it stays Go's zero time while the column itself
    // holds T1. Asserted against a field that IS hydrated — `images_updated_at`
    // — to prove the zero is a mapping gap, not a broken timestamp decoder.
    assert!(
        hydrate::is_go_zero(&got.updated_at),
        "UpdateAt maps to update_at, so the selected updated_at is discarded"
    );
    assert_eq!(
        hydrate::to_go_time_text(&got.updated_at),
        "0001-01-01T00:00:00Z",
        "Go's zero time, which is what Go's JSON renders for Folder.UpdateAt"
    );
    assert_eq!(
        nd_db::time::to_text(&got.images_updated_at),
        T1,
        "ImagesUpdatedAt maps to images_updated_at and IS hydrated"
    );
    assert_eq!(
        got.created_at.map(|d| nd_db::time::to_text(&d)),
        Some(T0.to_string())
    );
    // Assert: the row's own identity is preserved on the record…
    assert_eq!(got.id, "f-1");
    // Assert: the INNER-joined library path is on the record, and it is the
    // library's PATH — selectFolder emits only `library.path as library_path`.
    assert_eq!(got.library_path, LIB_PATH);
    // Assert: every persisted column read from its own column.
    assert_eq!(got.library_id, lib);
    assert_eq!(got.name, "Rock");
    assert_eq!(got.path, "music/rock");
    assert_eq!(got.parent_id, "p-abc");
    assert_eq!(got.num_audio_files, 3);
    assert_eq!(got.num_playlists, 2);
    assert!(!got.missing);
    assert_eq!(got.hash, "H-1");

    // Assert: the write map is unaffected — `library_path` (Go `structs:"-"`)
    // is not a key, so the legs are identical to a record whose join-only
    // field is empty.
    let row_map = folder_repo::folder_row(&got);
    let keys: Vec<&String> = row_map.map().keys().collect();
    assert_eq!(keys.len(), 12, "the 12 non-id persisted folder columns");
    for forbidden in ["library_path", "library_name", "id"] {
        assert!(
            !keys.iter().any(|k| k.as_str() == forbidden),
            "{forbidden} must not reach the write map: {keys:?}"
        );
    }
    assert_eq!(
        folder_repo::put_sql("f-1", &got, stamp(T1), &[]).expect("renders"),
        folder_repo::put_sql(
            "f-1",
            &FolderRecord {
                library_path: String::new(),
                ..got.clone()
            },
            stamp(T1),
            &[]
        )
        .expect("renders"),
        "the hydrated record re-renders the identical write legs"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The folder hook's Go-distinguishable modes, executed: the empty shim is
/// the `PostScan` skip branch (`ImageFiles` stays nil), a malformed payload is a
/// decode error carrying Go's EXACT prefix out of the read, and the DDL's
/// `'0000-00-00 00:00:00'` default SCANS as Go's zero `time.Time` — the pinned
/// driver swallows the parse failure (`go-sqlite3@v1.14.52/sqlite3.go:2832-2842`)
/// rather than failing the row.
#[test]
fn folder_empty_shim_skips_and_malformed_json_carries_gos_prefix() {
    // Arrange: two folders whose `image_files` cells bypass the write path.
    let (store, path) = store();
    let lib = seed_library(&store);
    for (id, image_files) in [("f-empty", ""), ("f-broken", r#"["a.jpg", oops]"#)] {
        insert(
            &store,
            "INSERT INTO folder (id, library_id, path, name, parent_id, num_audio_files, \
             num_playlists, image_files, images_updated_at, hash, missing, updated_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                rusqlite::types::Value::Text(id.to_string()),
                rusqlite::types::Value::Integer(lib),
                rusqlite::types::Value::Text("music/rock".to_string()),
                rusqlite::types::Value::Text("Rock".to_string()),
                rusqlite::types::Value::Text(String::new()),
                rusqlite::types::Value::Integer(1),
                rusqlite::types::Value::Integer(0),
                rusqlite::types::Value::Text(image_files.to_string()),
                rusqlite::types::Value::Text(T1.to_string()),
                rusqlite::types::Value::Text("H-1".to_string()),
                rusqlite::types::Value::Integer(0),
                rusqlite::types::Value::Text(T1.to_string()),
                rusqlite::types::Value::Text(T0.to_string()),
            ],
        );
    }

    // Act / Assert: the empty cell is Go's guard (`folder_repository.go:40`) —
    // no decode runs, the slice stays nil.
    let got = hydrate_folder(&store, "f-empty").expect("empty shim is not an error");
    assert_eq!(
        got.image_files, None,
        "the skip branch leaves ImageFiles nil"
    );

    // Act / Assert: the malformed cell is a scan error carrying Go's prefix.
    let err = hydrate_folder(&store, "f-broken").unwrap_err();
    let rusqlite::Error::FromSqlConversionFailure(_, _, msg) = err else {
        panic!("expected a conversion failure, got {err:?}");
    };
    assert!(
        msg.to_string()
            .starts_with(hydrate::FOLDER_IMAGE_FILES_PREFIX),
        "{msg}"
    );

    // Act / Assert: a default row whose non-pointer timestamps hold the DDL
    // sentinel reads FINE — the driver's parse-failure arm returns
    // time.Time{} with no error, so the row hydrates with Go's zero stamp and
    // the record spells that zero as `None` (Go's `IsZero()`).
    insert(
        &store,
        "INSERT INTO folder (id, library_id) VALUES (?, ?)",
        &[
            rusqlite::types::Value::Text("f-sentinel".to_string()),
            rusqlite::types::Value::Integer(lib),
        ],
    );
    let got = hydrate_folder(&store, "f-sentinel").expect("the sentinel scans as Go's zero time");
    assert!(
        hydrate::is_go_zero(&got.images_updated_at),
        "'0000-00-00 00:00:00' is not parseable, so the driver hands back time.Time{{}}"
    );
    assert_eq!(
        hydrate::to_go_time_text(&got.images_updated_at),
        "0001-01-01T00:00:00Z"
    );
    // The DDL defaults `created_at`/`updated_at` to
    // `datetime(current_timestamp, 'localtime')` — a NAIVE `2006-01-02 15:04:05`
    // the driver parses in UTC, so `created_at` is a real stamp and `id` still
    // comes from the row.
    assert_eq!(got.id, "f-sentinel");
    assert!(
        got.created_at.is_some(),
        "the naive default parses (driver format 3)"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The smart-playlist base read, executed: `rules` decoded through the criteria
/// codec, the evaluation-owned counters read from their base columns, and the
/// six annotation columns plus the base `average_rating` folded onto the record
/// — none of them reaching the write map.
#[test]
fn playlist_base_read_hydrates_rules_counters_and_annotation() {
    // Arrange:
    let (store, path) = store();
    seed_user(&store, "u-1", "admin");
    let rules = r#"{"any":[{"is":{"artist":"Frahm"}}]}"#;
    insert(
        &store,
        "INSERT INTO playlist (id, name, comment, duration, size, song_count, public, created_at, \
         updated_at, path, sync, rules, evaluated_at, owner_id, average_rating, imported_hash, \
         uploaded_image, external_image_url) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("pl-1".to_string()),
            rusqlite::types::Value::Text("SmartMix".to_string()),
            rusqlite::types::Value::Text("c".to_string()),
            rusqlite::types::Value::Real(55.5),
            rusqlite::types::Value::Integer(444444),
            rusqlite::types::Value::Integer(21),
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text("p-smart".to_string()),
            rusqlite::types::Value::Integer(0),
            rusqlite::types::Value::Text(rules.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text("u-1".to_string()),
            rusqlite::types::Value::Real(4.25),
            rusqlite::types::Value::Text("ih".to_string()),
            rusqlite::types::Value::Text(String::new()),
            rusqlite::types::Value::Text(String::new()),
        ],
    );
    // The annotation row the LEFT JOIN picks up for the caller.
    insert(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, play_date, rating, \
         starred, starred_at, rated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("u-1".to_string()),
            rusqlite::types::Value::Text("pl-1".to_string()),
            rusqlite::types::Value::Text("playlist".to_string()),
            rusqlite::types::Value::Integer(7),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Integer(4),
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text(T0.to_string()),
        ],
    );

    // Act: selectPlaylist for u-1, restricted to pl-1, through both phases.
    let got = hydrate_playlist(&store, "pl-1", "u-1", &Some("u-1".to_string()))
        .expect("base read hydrates");

    // Assert: rules decoded through the shared criteria codec, smart per Go.
    let parsed = hydrate::parse_playlist_rules(Some(rules)).expect("golden rules parse");
    assert_eq!(got.rules, parsed);
    assert!(playlist_repo::is_smart_playlist(&got.rules));
    // Assert: the counters are persisted columns and are read (PostMapArgs drops
    // them on WRITE; the read still sees the evaluation-owned values).
    assert_eq!(got.song_count, 21);
    assert_eq!(got.duration, 55.5);
    assert_eq!(got.size, 444444);
    assert_eq!(
        got.evaluated_at.map(|d| nd_db::time::to_text(&d)),
        Some(T1.to_string())
    );
    // Assert: the annotation join decoded, including the base-table average.
    // The three stamps are the MODEL's serialisable strings — Go's RFC3339Nano
    // wire form (`T`, UTC `Z`), NOT the SQLite storage spelling the columns hold.
    assert_eq!(got.annotation.play_count, 7);
    assert_eq!(got.annotation.rating, 4);
    assert!(got.annotation.starred);
    assert_eq!(
        got.annotation.play_date.as_deref(),
        Some("2024-02-02T00:00:00Z")
    );
    assert_eq!(
        got.annotation.starred_at.as_deref(),
        Some("2024-02-02T00:00:00Z")
    );
    assert_eq!(
        got.annotation.rated_at.as_deref(),
        Some("2024-01-01T00:00:00Z")
    );
    assert_eq!(got.annotation.average_rating, 4.25);
    // Assert: the row's own identity and the `user` join's `owner_name` are both
    // hydrated. `OwnerName` is `structs:"-"` for the WRITE map only; dbx's
    // `ScanStruct` maps by field name (`rows.go:60-85`), so the alias is scanned.
    assert_eq!(got.id, "pl-1");
    assert_eq!(got.owner_name, "admin");

    // Assert: the record is exactly what the row's own columns describe, plus
    // the two read-path-only fields the select contributes.
    let expected = PlaylistRecord {
        id: "pl-1".to_string(),
        name: "SmartMix".to_string(),
        comment: "c".to_string(),
        duration: 55.5,
        size: 444444,
        song_count: 21,
        owner_id: "u-1".to_string(),
        public: true,
        path: "p-smart".to_string(),
        sync: false,
        uploaded_image: String::new(),
        external_image_url: String::new(),
        created_at: Some(stamp(T0)),
        updated_at: stamp(T1),
        imported_hash: "ih".to_string(),
        rules: got.rules.clone(),
        evaluated_at: got.evaluated_at,
        annotation: got.annotation.clone(),
        owner_name: "admin".to_string(),
    };
    assert_eq!(got, expected);

    // Assert: no derived/joined column reaches the write map — the write SQL is
    // unchanged by hydration — and the smart PostMapArgs arm still applies.
    let row_map = playlist_repo::playlist_row(&got);
    let keys: Vec<&String> = row_map.map().keys().collect();
    for derived in [
        "starred",
        "starred_at",
        "rating",
        "rated_at",
        "play_date",
        "play_count",
        "average_rating",
        "owner_name",
        "annotation",
    ] {
        assert!(
            !keys.iter().any(|k| k.as_str() == derived),
            "{derived} must not be written: {keys:?}"
        );
    }
    assert!(
        keys.iter().any(|k| k.as_str() == "rules"),
        "smart arm stores rules"
    );
    for counter in playlist_repo::SMART_OWNED_COUNTER_COLS {
        assert!(
            !keys.iter().any(|k| k.as_str() == counter),
            "smart arm drops {counter}"
        );
    }

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The rules shim's three nil arms and its error arm, executed on live rows:
/// SQL NULL, the empty cell, and a literal JSON `null` all leave `Rules == None`
/// like Go; a payload Go rejects surfaces the criteria codec's message
/// UN-prefixed (`dbPlaylist.PostScan` has no `fmt.Errorf` wrapper).
#[test]
fn playlist_rules_nil_arms_and_malformed_criteria_are_unprefixed() {
    // Arrange: one playlist per rules shape.
    let (store, path) = store();
    seed_user(&store, "u-1", "admin");
    for (id, rules) in [
        ("pl-null", None),
        ("pl-empty", Some("")),
        ("pl-json-null", Some("null")),
        ("pl-broken", Some(r#"{"all":[]}"#)),
    ] {
        insert(
            &store,
            "INSERT INTO playlist (id, name, owner_id, created_at, updated_at, rules) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                rusqlite::types::Value::Text(id.to_string()),
                rusqlite::types::Value::Text("Mix".to_string()),
                rusqlite::types::Value::Text("u-1".to_string()),
                rusqlite::types::Value::Text(T0.to_string()),
                rusqlite::types::Value::Text(T1.to_string()),
                match rules {
                    Some(text) => rusqlite::types::Value::Text(text.to_string()),
                    None => rusqlite::types::Value::Null,
                },
            ],
        );
    }

    // Act / Assert: the three nil arms (NULL, "", JSON null).
    for id in ["pl-null", "pl-empty", "pl-json-null"] {
        let got =
            hydrate_playlist(&store, id, "u-1", &None).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(got.rules, None, "{id} leaves Rules nil like Go");
        assert!(!playlist_repo::is_smart_playlist(&got.rules));
    }

    // Act / Assert: the error arm carries the codec's message with NO prefix.
    let err = hydrate_playlist(&store, "pl-broken", "u-1", &None).unwrap_err();
    let rusqlite::Error::FromSqlConversionFailure(_, _, msg) = err else {
        panic!("expected a conversion failure, got {err:?}");
    };
    let text = msg.to_string();
    assert!(
        text.contains("missing rules"),
        "expected the criteria codec's own message, got {text}"
    );
    assert!(
        !text.contains("from db:") && !text.starts_with("parsing "),
        "Go wraps the playlist rules error with no prefix: {text}"
    );
    // Assert: the seam's own type carries that message verbatim (no invented
    // prefix added anywhere in the playlist path).
    assert_eq!(HydrateError::Decoded(text.clone()).to_string(), text);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The absent-column tolerance rule executed: a light projection emits none of
/// the annotation columns, so each joined column decodes to its zero value and
/// `has_column` reports the absence — no panic, no error. The LEFT JOIN's own
/// NULL arm (columns present, no annotation row) decodes to the same zeros.
#[test]
fn light_playlist_query_without_annotation_columns_decodes_zeros() {
    // Arrange: a playlist whose owner HAS an annotation row, read without the
    // join (the light-query shape).
    let (store, path) = store();
    seed_user(&store, "u-1", "admin");
    insert(
        &store,
        "INSERT INTO playlist (id, name, owner_id, created_at, updated_at, average_rating) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("pl-1".to_string()),
            rusqlite::types::Value::Text("Mix".to_string()),
            rusqlite::types::Value::Text("u-1".to_string()),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Real(3.5),
        ],
    );
    insert(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("u-1".to_string()),
            rusqlite::types::Value::Text("pl-1".to_string()),
            rusqlite::types::Value::Text("playlist".to_string()),
            rusqlite::types::Value::Integer(9),
            rusqlite::types::Value::Integer(5),
            rusqlite::types::Value::Integer(1),
        ],
    );

    let light = "SELECT playlist.* FROM playlist WHERE id = 'pl-1'".to_string();
    let mut rows = query_all(&store, &(light, Vec::new()), |row| {
        for absent in [
            "starred",
            "starred_at",
            "rating",
            "rated_at",
            "play_date",
            "play_count",
            "owner_name",
        ] {
            assert!(!row.has_column(absent), "{absent} absent on a light query");
        }
        assert!(
            row.has_column("average_rating"),
            "it is a base-table column, emitted by playlist.*"
        );
        hydrate_playlist_row(row)
    })
    .expect("light query hydrates");
    let got = rows.pop().expect("one playlist row");

    // Assert: the annotation is the zero value, NOT the joined row's values.
    // `average_rating` comes from the base table, so playlist.* still emits it;
    // the six JOINED columns are the ones that decode to zeros.
    assert_eq!(got.annotation.average_rating, 3.5);
    assert_eq!(got.annotation.play_count, 0);
    assert_eq!(got.annotation.rating, 0);
    assert!(!got.annotation.starred);
    assert_eq!(got.annotation.play_date, None);
    assert_eq!(got.annotation.rated_at, None);
    assert_eq!(got.annotation.starred_at, None);
    assert_eq!(got.name, "Mix");
    // The light projection has no `owner_name` alias, so Go's `ScanStruct`
    // leaves the field at its zero value (rows.go:81-86 discards the column);
    // the row's own `id` is still read from `playlist.*`.
    assert_eq!(got.owner_name, "", "absent alias leaves Go's zero value");
    assert_eq!(got.id, "pl-1");
    assert_eq!(
        got.song_count, 0,
        "the DDL default, read through the base column"
    );
    assert_eq!(
        got.created_at.map(|d| nd_db::time::to_text(&d)),
        Some(T0.to_string())
    );

    // Act / Assert: the LEFT JOIN's NULL arm — columns present, no annotation
    // row for this user — decodes to zeros/None, and the base average_rating
    // is still read.
    let joined = hydrate_playlist(&store, "pl-1", "u-2", &None).expect("join read hydrates");
    assert!(!joined.annotation.starred);
    assert_eq!(joined.annotation.play_count, 0);
    assert_eq!(joined.annotation.rating, 0);
    assert_eq!(
        joined.annotation.play_date, None,
        "pointer column NULL → None"
    );
    assert_eq!(joined.annotation.rated_at, None);
    assert_eq!(joined.annotation.starred_at, None);
    assert_eq!(
        joined.annotation.average_rating, 3.5,
        "base-table column, not the join"
    );
    // The `user` JOIN is present here, so `owner_name` is hydrated even though
    // no annotation row matched.
    assert_eq!(joined.owner_name, "admin");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Bulk identity: a multi-row read keeps each row's OWN `id` on its record —
/// the hazard a shared/reused record would create. Folders and playlists are
/// both read through the repo's bulk select (`get_all_sql`), and each record's
/// id must match its own row rather than the first row's or the caller's.
#[test]
fn bulk_reads_keep_each_rows_own_id() {
    // Arrange: three folders and three playlists, ids in a different order than
    // the read's ORDER BY so a mis-threaded id would show up.
    let (store, path) = store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    for (id, name) in [("f-3", "Three"), ("f-1", "One"), ("f-2", "Two")] {
        insert(
            &store,
            "INSERT INTO folder (id, library_id, path, name, images_updated_at, updated_at, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                rusqlite::types::Value::Text(id.to_string()),
                rusqlite::types::Value::Integer(lib),
                rusqlite::types::Value::Text(format!("music/{name}")),
                rusqlite::types::Value::Text(name.to_string()),
                rusqlite::types::Value::Text(T1.to_string()),
                rusqlite::types::Value::Text(T1.to_string()),
                rusqlite::types::Value::Text(T0.to_string()),
            ],
        );
    }
    for (id, name) in [("pl-3", "C"), ("pl-1", "A"), ("pl-2", "B")] {
        insert(
            &store,
            "INSERT INTO playlist (id, name, owner_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
            &[
                rusqlite::types::Value::Text(id.to_string()),
                rusqlite::types::Value::Text(name.to_string()),
                rusqlite::types::Value::Text("u-1".to_string()),
                rusqlite::types::Value::Text(T0.to_string()),
                rusqlite::types::Value::Text(T1.to_string()),
            ],
        );
    }

    // Act: the bulk folder read, ordered by name.
    let bulk = "SELECT folder.id, folder.library_id, folder.path, folder.name, \
                folder.missing, folder.parent_id, folder.num_audio_files, \
                folder.num_playlists, folder.image_files, folder.images_updated_at, \
                folder.hash, folder.updated_at, folder.created_at, library.path as library_path \
                FROM folder INNER JOIN library ON folder.library_id = library.id \
                ORDER BY folder.name";
    let folders = query_all(&store, &(bulk.to_string(), Vec::new()), hydrate_folder_row)
        .expect("bulk folder read");
    // Assert: ids follow the rows (name order: One, Three, Two), not the insert order.
    let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["f-1", "f-3", "f-2"]);
    for (folder, id) in folders.iter().zip(["f-1", "f-3", "f-2"]) {
        assert_eq!(folder.id, id);
        assert_eq!(folder.path, format!("music/{}", folder.name));
    }

    // Act: the bulk playlist read through the repo's own select.
    let playlists = query_all(
        &store,
        &playlist_repo::get_all_sql("u-1", &Some("u-1".to_string()), &["name".to_string()]),
        hydrate_playlist_row,
    )
    .expect("bulk playlist read");
    // Assert: three rows, each with its own id and its own owner_name.
    let ids: Vec<&str> = playlists.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["pl-1", "pl-2", "pl-3"]);
    for (playlist, id) in playlists.iter().zip(["pl-1", "pl-2", "pl-3"]) {
        assert_eq!(playlist.id, id);
        assert_eq!(playlist.owner_name, "admin");
    }

    drop(store);
    std::fs::remove_file(&path).unwrap();
}
