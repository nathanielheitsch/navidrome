//! S-D2b record-driven writes for `folder` and `playlist`, EXECUTED against
//! a real SQLite DB.
//!
//! These are the per-entity executed write→read tests the contract
//! (`docs/rust-port/entity-write-contract.md` §7) assigns to the
//! folder/playlist sub-slices: a complete [`FolderRecord`] /
//! [`PlaylistRecord`] with a **distinct value per column** (so a value
//! rotation or field swap fails a per-column read-back), the INSERT leg
//! executed, every column read back in its own column, and the UPDATE
//! leg proving `created_at` survives while `updated_at` re-stamps.
//!
//! Playlist additionally pins the `dbPlaylist.PostMapArgs` arms
//! (`playlist_repository.go:41-51`): a smart record stores `rules` as
//! JSON and the evaluation-owned counters (`song_count`/`duration`/
//! `size`) are **not** in the UPDATE set — a previously stored counter
//! survives a smart save; a non-smart record stores no `rules` column
//! (NULL) and the counters are present. A camelCase partial
//! `cols_to_update` (`["Sync"]`, Go `Update`'s shape via
//! `toSnakeCase`) touches only `sync` + `updated_at`.
//!
//! Harness shape is `tests/runtime_insert_alignment.rs`: one isolated
//! on-disk temp DB per test, `apply_fresh_schema` through `nd_dbschema`,
//! `Pool` so `SEEDEDRAND`/`NATSORT` are registered per connection,
//! `_foreign_keys=on` (Go's `db/db.go:55`), with the same FK seeds
//! (library before folder, user before playlist).

use chrono::{DateTime, TimeZone, Utc};

use nd_db::folder_repo::{self, FolderRecord};
use nd_db::playlist_repo::{self, PlaylistRecord};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

fn value_ref(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

/// One isolated on-disk DB per invocation, freshly migrated.
fn store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-records-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    {
        let conn = pool.checkout().unwrap();
        apply_fresh_schema(conn.conn()).unwrap();
    }
    (SQLStore::new(pool), path)
}

fn insert(store: &SQLStore, statement: &(String, Vec<Value>)) -> usize {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                &statement.0,
                rusqlite::params_from_iter(statement.1.iter().map(value_ref)),
            )
        })
        .unwrap_or_else(|e| panic!("{} --{:?}--: {e}", statement.0, statement.1))
}

fn row_text(store: &SQLStore, sql: &str) -> String {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |r| r.get::<_, String>(0)))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn row_i64(store: &SQLStore, sql: &str) -> i64 {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |r| r.get::<_, i64>(0)))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn row_real(store: &SQLStore, sql: &str) -> f64 {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |r| r.get::<_, f64>(0)))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn row_opt_text(store: &SQLStore, sql: &str) -> Option<String> {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0)))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// `album`, `folder` and `media_file` reference `library`, so the row must
/// exist before any insert with `_foreign_keys=on` (reused verbatim from
/// `tests/runtime_insert_alignment.rs`).
fn seed_library(store: &SQLStore) {
    insert(
        store,
        &(
            "INSERT INTO library (id, name, path) VALUES (?, ?, ?)".to_string(),
            vec![Value::from(1), Value::from("Main"), Value::from("/music")],
        ),
    );
}

/// `playlist.owner_id` is NOT NULL without a default and references `user`.
fn seed_user(store: &SQLStore) {
    insert(
        store,
        &(
            "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            vec![
                Value::from("u-1"),
                Value::from("admin"),
                Value::from("Admin"),
                Value::from("a@b.c"),
                Value::from("x"),
                Value::Bool(true),
                Value::from("2024-01-01 00:00:00+00:00"),
                Value::from("2024-01-01 00:00:00+00:00"),
            ],
        ),
    );
}

fn dt(hms: &str) -> DateTime<Utc> {
    let parts: Vec<u32> = hms.split(':').map(|v| v.parse().unwrap()).collect();
    Utc.with_ymd_and_hms(2024, 7, 1, parts[0], parts[1], parts[2])
        .unwrap()
}

// ---------------------------------------------------------------------------
// Folder record write→read.
// ---------------------------------------------------------------------------

/// A folder record with a value in every one of the 13 persisted columns,
/// each distinct and typed per column — a rotation detector.
fn folder_record() -> FolderRecord {
    FolderRecord {
        library_id: 1,
        path: "music/rock".to_string(),
        name: "Rock".to_string(),
        parent_id: "p-parent1".to_string(),
        num_audio_files: 31,
        num_playlists: 5,
        image_files: Some(vec!["cover.jpg".to_string(), "back.jpg".to_string()]),
        images_updated_at: dt("10:11:12"),
        hash: "H-folder1".to_string(),
        missing: true,
        updated_at: dt("08:09:10"),
        created_at: Some(dt("01:02:03")),
    }
}

/// `put_sql` a full [`FolderRecord`]; every distinct value must land in
/// its own column. `None` image_files stores the literal `"[]"`; a
/// subsequent update re-stamps `updated_at` and leaves `created_at`
/// untouched.
#[test]
fn folder_record_insert_update_readback() {
    let (store, path) = store();
    seed_library(&store);

    let f = folder_record();
    let now = dt("14:15:16");
    let put = folder_repo::put_sql("f-1", &f, now, &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);

    // Rotation detector: every column read back in its own column.
    assert_eq!(row_text(&store, "SELECT id FROM folder"), "f-1");
    assert_eq!(row_i64(&store, "SELECT library_id FROM folder"), 1);
    assert_eq!(row_text(&store, "SELECT path FROM folder"), "music/rock");
    assert_eq!(row_text(&store, "SELECT name FROM folder"), "Rock");
    assert_eq!(
        row_text(&store, "SELECT parent_id FROM folder"),
        "p-parent1"
    );
    assert_eq!(row_i64(&store, "SELECT num_audio_files FROM folder"), 31);
    assert_eq!(row_i64(&store, "SELECT num_playlists FROM folder"), 5);
    assert_eq!(
        row_text(&store, "SELECT image_files FROM folder"),
        r#"["cover.jpg","back.jpg"]"#,
        "Some(image_files) must store the JSON array"
    );
    assert_eq!(
        row_text(&store, "SELECT images_updated_at FROM folder"),
        "2024-07-01 10:11:12+00:00"
    );
    assert_eq!(row_text(&store, "SELECT hash FROM folder"), "H-folder1");
    assert_eq!(
        row_i64(&store, "SELECT missing FROM folder"),
        1,
        "missing = true"
    );
    // updated_at was re-stamped to `now`; created_at kept the record's.
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM folder"),
        "2024-07-01 14:15:16+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM folder"),
        "2024-07-01 01:02:03+00:00"
    );

    // Update path: new distinct values + a new `now`. created_at must
    // survive (selectUpdateColumns drops it), updated_at must re-stamp.
    let mut f2 = folder_record();
    f2.name = "Rock (remixed)".to_string();
    f2.parent_id = "p-parent2".to_string();
    f2.num_audio_files = 32;
    f2.num_playlists = 6;
    f2.image_files = None; // PostMapArgs' "[]" arm
    f2.created_at = Some(dt("23:59:59")); // a different stamp — must NOT land
    let now2 = dt("16:17:18");
    let up = folder_repo::put_sql("f-1", &f2, now2, &[]).expect("renders");
    assert!(
        !up.update.0.contains("created_at = ?"),
        "update must not SET created_at"
    );
    assert_eq!(insert(&store, &up.update), 1);

    assert_eq!(
        row_text(&store, "SELECT name FROM folder"),
        "Rock (remixed)",
        "update changed name"
    );
    assert_eq!(
        row_text(&store, "SELECT parent_id FROM folder"),
        "p-parent2"
    );
    assert_eq!(row_i64(&store, "SELECT num_audio_files FROM folder"), 32);
    assert_eq!(row_i64(&store, "SELECT num_playlists FROM folder"), 6);
    assert_eq!(
        row_text(&store, "SELECT image_files FROM folder"),
        "[]",
        "None image_files must store the literal [] (never null)"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM folder"),
        "2024-07-01 16:17:18+00:00",
        "updated_at re-stamped by the update"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM folder"),
        "2024-07-01 01:02:03+00:00",
        "created_at must survive the update"
    );

    drop(store);
    std::fs::remove_file(path).ok();
}

/// Non-vacuity arm: a rotated folder INSERT (args shifted by one, as the
/// pre-`8c89aeb9` parallel-list renderers did) produces a visibly wrong
/// row that the per-column read-backs above would reject — proving the
/// assertions are sensitive to column alignment.
#[test]
fn folder_record_rotated_insert_is_detected() {
    let (store, path) = store();
    seed_library(&store);

    let f = folder_record();
    let put = folder_repo::put_sql("f-1", &f, dt("14:15:16"), &[]).expect("renders");
    // Rotate the args right by one (drop the last, prepend it) — exactly
    // the id-first/last misalignment class.
    let (sql, args) = put.insert;
    let mut rotated = args[1..].to_vec();
    rotated.push(args[0].clone());
    assert_eq!(
        insert(&store, &(sql.clone(), rotated)),
        1,
        "SQLite accepts the rotation; it mis-stores"
    );
    // The corruption the record-driven renderer prevents: values now sit
    // one column early — id holds `updated_at`'s text, not "f-1".
    assert_ne!(
        row_text(&store, "SELECT id FROM folder"),
        "f-1",
        "the rotation must be visible in the stored row"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Playlist record write→read.
// ---------------------------------------------------------------------------

fn non_smart_record() -> PlaylistRecord {
    PlaylistRecord {
        name: "Mix".to_string(),
        comment: "c-mix".to_string(),
        duration: 47.5,
        size: 987654,
        song_count: 33,
        owner_id: "u-1".to_string(),
        public: true,
        path: "p-mix".to_string(),
        sync: true,
        uploaded_image: "up-mix".to_string(),
        external_image_url: "ext-mix".to_string(),
        created_at: Some(dt("01:02:03")),
        updated_at: dt("08:09:10"),
        imported_hash: "ih-mix".to_string(),
        rules: None,
        evaluated_at: None,
    }
}

/// A smart record: typed criteria (rules stored as `marshal_criteria`
/// JSON) + nonzero counters that must be DROPPED from the update set by
/// the PostMapArgs smart arm.
fn smart_record() -> PlaylistRecord {
    let mut fields = nd_model::Fields::new();
    fields.insert(
        "artist".to_string(),
        serde_json::Value::String("Frahm".to_string()),
    );
    PlaylistRecord {
        name: "SmartMix".to_string(),
        comment: "c-smart".to_string(),
        duration: 55.5,
        size: 444444,
        song_count: 21,
        owner_id: "u-1".to_string(),
        public: false,
        path: "p-smart".to_string(),
        sync: false,
        uploaded_image: "up-smart".to_string(),
        external_image_url: "ext-smart".to_string(),
        created_at: Some(dt("01:02:04")),
        updated_at: dt("08:09:11"),
        imported_hash: "ih-smart".to_string(),
        rules: Some(nd_model::Criteria {
            expression: nd_model::Expression::Is(fields),
            ..nd_model::Criteria::default()
        }),
        evaluated_at: Some(dt("06:07:08")),
    }
}

/// Smart playlist: `rules` stored as JSON, and the evaluation-owned
/// counters are NOT in the UPDATE set — a previously stored counter
/// survives a smart save.
#[test]
fn playlist_smart_rules_stored_and_counters_unwritten() {
    let (store, path) = store();
    seed_user(&store);

    let p = smart_record();
    let put = playlist_repo::put_sql("pl-1", &p, dt("14:15:16"), &[]).expect("renders");

    // Go applies PostMapArgs to the shared `toSQLArgs` map
    // (`persistence/helpers.go:20-42`) and BOTH legs read it — so a FRESH
    // smart insert stores the DDL-zero counters (the delete runs before the
    // INSERT, not only for the UPDATE); `refreshCounters` owns the real
    // values afterwards.
    assert_eq!(insert(&store, &put.insert), 1);
    assert_eq!(
        row_i64(&store, "SELECT song_count FROM playlist"),
        0,
        "smart: song_count deleted from the map before the insert"
    );

    // Every distinct value in its own column.
    assert_eq!(row_text(&store, "SELECT id FROM playlist"), "pl-1");
    assert_eq!(row_text(&store, "SELECT name FROM playlist"), "SmartMix");
    assert_eq!(row_text(&store, "SELECT comment FROM playlist"), "c-smart");
    assert_eq!(
        row_real(&store, "SELECT duration FROM playlist"),
        0.0,
        "smart: duration deleted from the map before the insert"
    );
    assert_eq!(
        row_i64(&store, "SELECT size FROM playlist"),
        0,
        "smart: size deleted from the map before the insert"
    );
    assert_eq!(row_text(&store, "SELECT owner_id FROM playlist"), "u-1");
    assert_eq!(
        row_i64(&store, "SELECT public FROM playlist"),
        0,
        "public = false"
    );
    assert_eq!(row_text(&store, "SELECT path FROM playlist"), "p-smart");
    assert_eq!(
        row_i64(&store, "SELECT sync FROM playlist"),
        0,
        "sync = false"
    );
    assert_eq!(
        row_text(&store, "SELECT uploaded_image FROM playlist"),
        "up-smart"
    );
    assert_eq!(
        row_text(&store, "SELECT external_image_url FROM playlist"),
        "ext-smart"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM playlist"),
        "2024-07-01 01:02:04+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM playlist"),
        "2024-07-01 14:15:16+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT imported_hash FROM playlist"),
        "ih-smart"
    );
    assert_eq!(
        row_text(&store, "SELECT evaluated_at FROM playlist"),
        "2024-07-01 06:07:08+00:00"
    );
    // rules stored as the marshal_criteria JSON.
    let expected_rules = nd_model::criteria::marshal_criteria(p.rules.as_ref().expect("smart"));
    assert_eq!(
        row_text(&store, "SELECT rules FROM playlist"),
        expected_rules,
        "rules must be the Go MarshalJSON shape"
    );

    // The UPDATE leg must not touch the evaluation-owned counters.
    let up_sql = &put.update.0;
    for c in playlist_repo::SMART_OWNED_COUNTER_COLS {
        assert!(
            !up_sql.contains(&format!("{c} = ?")),
            "smart update must not SET {c}: {up_sql}"
        );
    }

    // Now simulate a previously-stored counter and run the update: the
    // stored counter must survive the smart save.
    insert(
        &store,
        &(
            "UPDATE playlist SET song_count = ?, duration = ?, size = ? WHERE id = ?".to_string(),
            vec![
                Value::Integer(777),
                Value::Real(1234.5),
                Value::Integer(88888),
                Value::Text("pl-1".to_string()),
            ],
        ),
    );
    assert_eq!(insert(&store, &put.update), 1, "update matched the row");

    assert_eq!(
        row_i64(&store, "SELECT song_count FROM playlist"),
        777,
        "previously stored counter must survive a smart save"
    );
    assert_eq!(
        row_real(&store, "SELECT duration FROM playlist"),
        1234.5,
        "previously stored duration must survive a smart save"
    );
    assert_eq!(
        row_i64(&store, "SELECT size FROM playlist"),
        88888,
        "previously stored size must survive a smart save"
    );
    // Non-counter columns DID change on the update.
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM playlist"),
        "2024-07-01 14:15:16+00:00"
    );
    // created_at untouched.
    assert_eq!(
        row_text(&store, "SELECT created_at FROM playlist"),
        "2024-07-01 01:02:04+00:00",
        "created_at must survive the update"
    );

    drop(store);
    std::fs::remove_file(path).ok();
}

/// Non-smart playlist: no `rules` column in the row (NULL), the counters
/// present as written.
#[test]
fn playlist_non_smart_rules_absent_counters_written() {
    let (store, path) = store();
    seed_user(&store);

    let p = non_smart_record();
    let put = playlist_repo::put_sql("pl-2", &p, dt("14:15:16"), &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(row_text(&store, "SELECT id FROM playlist"), "pl-2");
    assert_eq!(row_text(&store, "SELECT name FROM playlist"), "Mix");
    assert_eq!(
        row_i64(&store, "SELECT song_count FROM playlist"),
        33,
        "counters present as written"
    );
    assert_eq!(row_real(&store, "SELECT duration FROM playlist"), 47.5);
    assert_eq!(row_i64(&store, "SELECT size FROM playlist"), 987654);
    assert_eq!(
        row_opt_text(&store, "SELECT rules FROM playlist"),
        None,
        "non-smart: rules must be NULL (the column is not in the row)"
    );
    assert_eq!(
        row_opt_text(&store, "SELECT evaluated_at FROM playlist"),
        None,
        "evaluated_at None binds NULL"
    );
    assert_eq!(
        row_i64(&store, "SELECT public FROM playlist"),
        1,
        "public = true"
    );
    assert_eq!(
        row_i64(&store, "SELECT sync FROM playlist"),
        1,
        "sync = true"
    );

    // The update for a non-smart record writes the counters.
    let up_sql = &put.update.0;
    for c in playlist_repo::SMART_OWNED_COUNTER_COLS {
        assert!(
            up_sql.contains(&format!("{c} = ?")),
            "non-smart update must SET {c}: {up_sql}"
        );
    }
    assert_eq!(insert(&store, &put.update), 1);
    assert_eq!(
        row_text(&store, "SELECT created_at FROM playlist"),
        "2024-07-01 01:02:03+00:00",
        "created_at must survive the update"
    );

    drop(store);
    std::fs::remove_file(path).ok();
}

/// Go's `Update` shape (`playlist_repository.go:431-437`): a caller-driven
/// partial update with camelCase cols (`["Sync"]`) — after the seam's
/// `toSnakeCase` and Go's `updatedAt` append, only `sync` + `updated_at`
/// change; name/comment/public/rules/counters are untouched.
#[test]
fn playlist_partial_update_camelcase_touches_only_sync_and_updated_at() {
    let (store, path) = store();
    seed_user(&store);

    let p = non_smart_record();
    let put = playlist_repo::put_sql("pl-3", &p, dt("14:15:16"), &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);

    // A changed record that flips sync (and would change everything else
    // if the restriction were not applied).
    let mut p2 = non_smart_record();
    p2.sync = false;
    p2.name = "CHANGED".to_string();
    p2.comment = "CHANGED".to_string();
    p2.public = false;
    p2.song_count = 999;
    p2.duration = 1.0;
    p2.size = 1;
    p2.rules = None;
    // Go's Update appends "updatedAt" to the caller's cols.
    let up = playlist_repo::put_sql("pl-3", &p2, dt("16:17:18"), &["Sync", "UpdatedAt"])
        .expect("renders");
    assert_eq!(
        up.update.0, "UPDATE playlist SET sync = ?, updated_at = ? WHERE id = ?",
        "camelCase cols normalize; only sync + updated_at are SET"
    );
    assert_eq!(insert(&store, &up.update), 1);

    assert_eq!(
        row_i64(&store, "SELECT sync FROM playlist"),
        0,
        "sync flipped"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM playlist"),
        "2024-07-01 16:17:18+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT name FROM playlist"),
        "Mix",
        "name untouched"
    );
    assert_eq!(
        row_text(&store, "SELECT comment FROM playlist"),
        "c-mix",
        "comment untouched"
    );
    assert_eq!(
        row_i64(&store, "SELECT public FROM playlist"),
        1,
        "public untouched"
    );
    assert_eq!(
        row_i64(&store, "SELECT song_count FROM playlist"),
        33,
        "counter untouched"
    );
    assert_eq!(
        row_real(&store, "SELECT duration FROM playlist"),
        47.5,
        "duration untouched"
    );
    assert_eq!(
        row_i64(&store, "SELECT size FROM playlist"),
        987654,
        "size untouched"
    );
    assert_eq!(
        row_opt_text(&store, "SELECT rules FROM playlist"),
        None,
        "rules untouched (still NULL)"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM playlist"),
        "2024-07-01 01:02:03+00:00",
        "created_at untouched on update"
    );

    drop(store);
    std::fs::remove_file(path).ok();
}
