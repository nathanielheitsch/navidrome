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
        // Read-path identity (never in folder_row): the row's own id.
        id: "f-1".to_string(),
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
        // Hydration-owned join-only field: absent from folder_row.
        library_path: String::new(),
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
        // Read-path identity (never in playlist_row): the row's own id.
        id: "pl-1".to_string(),
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
        // Hydration-owned derived fields: absent from playlist_row.
        annotation: nd_model::annotation::Annotation::default(),
        // Read-path-only (`structs:"-"`): absent from playlist_row.
        owner_name: "jdoe".to_string(),
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
        // Read-path identity (never in playlist_row): the row's own id.
        id: "pl-2".to_string(),
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
        // Derived, hydration-owned: absent from playlist_row.
        annotation: nd_model::annotation::Annotation::default(),
        // Read-path-only (`structs:"-"`): absent from playlist_row.
        owner_name: "jdoe".to_string(),
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

// ---------------------------------------------------------------------------
// Album record write→read (S-D2b).
// ---------------------------------------------------------------------------

use nd_db::album_repo::{self, AlbumRecord};
use nd_db::mediafile_repo::{self, MediaFileRecord};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::DbTags;
use std::collections::BTreeMap;

fn album_record() -> AlbumRecord {
    let mut discs = BTreeMap::new();
    discs.insert(1, "disc-one".to_string());
    discs.insert(2, "disc-two".to_string());
    discs.insert(10, "disc-ten".to_string());
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Rock".to_string()]);
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-1".to_string(),
            name: "Nils Frahm".to_string(),
            sub_role: "Vocals".to_string(),
        }],
    );
    // Go AllNames input: name + non-empty SortArtistName, never SubRole.
    let participant_names = vec![nd_db::album_repo::NamedParticipant {
        id: "ar-1".to_string(),
        name: "Nils Frahm".to_string(),
        sort_name: "Frahm, Nils".to_string(),
        sub_role: "Vocals".to_string(),
    }];
    AlbumRecord {
        library_id: 1,
        name: "Abbey Road".to_string(),
        embed_art_path: "/cover.jpg".to_string(),
        album_artist: "Nils Frahm".to_string(),
        album_artist_id: "aa-1".to_string(),
        min_year: 1969,
        max_year: 1970,
        date: "1969-09-26".to_string(),
        min_original_year: 1968,
        max_original_year: 1968,
        original_date: "1968-01-02".to_string(),
        release_date: "1970-02-01".to_string(),
        compilation: true,
        comment: "c-al".to_string(),
        song_count: 17,
        duration: 1234.5,
        size: 87654321,
        discs: Some(discs),
        sort_album_name: "s-al".to_string(),
        sort_album_artist_name: "s-aa".to_string(),
        order_album_name: "o-al".to_string(),
        order_album_artist_name: "o-aa".to_string(),
        catalog_num: "CAT-1".to_string(),
        mbz_album_id: "m-al".to_string(),
        mbz_album_artist_id: "m-aa".to_string(),
        mbz_album_type: "m-atype".to_string(),
        mbz_album_comment: "m-ac".to_string(),
        mbz_release_group_id: "m-rg".to_string(),
        folder_ids: Some(vec!["fold-1".to_string()]),
        explicit_status: "E".to_string(),
        rg_album_gain: None,
        rg_album_peak: None,
        description: "d-al".to_string(),
        small_image_url: "http://s".to_string(),
        medium_image_url: "http://m".to_string(),
        large_image_url: "http://l".to_string(),
        external_url: "http://e".to_string(),
        external_info_updated_at: Some(dt("05:06:07")),
        genre: "Rock".to_string(),
        tags,
        participants,
        participant_names,
        missing: true,
        imported_at: dt("01:02:03"),
        created_at: Some(dt("02:03:04")),
        updated_at: Some(dt("03:04:05")),
    }
}

fn seed_artist(store: &SQLStore, id: &str, name: &str) {
    insert(
        store,
        &(
            "INSERT INTO artist (id, name, order_artist_name) VALUES (?, ?, ?)".to_string(),
            vec![Value::from(id), Value::from(name), Value::from("")],
        ),
    );
}

/// Album record: every distinct value in its own column, the RG shims bind
/// NULL when `None`, `discs` JSON is string-key ordered, `folder_ids` stores
/// the array, the FTS columns are consistent with the derived inputs, the
/// `releases` DDL orphan stays at its default 0, and the restricted
/// external-info update leaves name/title/song_count/created_at untouched.
#[test]
fn album_record_insert_update_readback() {
    let (store, path) = store();
    seed_library(&store);
    seed_artist(&store, "ar-1", "Nils Frahm");
    let genre_id = nd_db::sql_tags::indexed_tag_ids(nd_db::sql_tags::DbTags::from([(
        "genre".to_string(),
        vec!["Rock".to_string()],
    )]))[0]
        .clone();
    insert(
        &store,
        &(
            "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?)".to_string(),
            vec![
                Value::from(genre_id.as_str()),
                Value::from("Rock"),
                Value::from("rock-al"),
            ],
        ),
    );

    let a = album_record();
    let now = dt("14:15:16");
    let put = album_repo::put_sql("al-1", &a, now, &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);

    // Rotation detector.
    assert_eq!(row_text(&store, "SELECT id FROM album"), "al-1");
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Abbey Road");
    assert_eq!(
        row_text(&store, "SELECT embed_art_path FROM album"),
        "/cover.jpg"
    );
    assert_eq!(
        row_text(&store, "SELECT album_artist FROM album"),
        "Nils Frahm"
    );
    assert_eq!(
        row_text(&store, "SELECT album_artist_id FROM album"),
        "aa-1"
    );
    assert_eq!(row_i64(&store, "SELECT min_year FROM album"), 1969);
    assert_eq!(row_i64(&store, "SELECT max_year FROM album"), 1970);
    assert_eq!(row_text(&store, "SELECT date FROM album"), "1969-09-26");
    assert_eq!(row_i64(&store, "SELECT min_original_year FROM album"), 1968);
    assert_eq!(row_i64(&store, "SELECT max_original_year FROM album"), 1968);
    assert_eq!(
        row_text(&store, "SELECT original_date FROM album"),
        "1968-01-02"
    );
    assert_eq!(
        row_text(&store, "SELECT release_date FROM album"),
        "1970-02-01"
    );
    assert_eq!(row_i64(&store, "SELECT compilation FROM album"), 1);
    assert_eq!(row_text(&store, "SELECT comment FROM album"), "c-al");
    assert_eq!(row_i64(&store, "SELECT song_count FROM album"), 17);
    assert_eq!(row_real(&store, "SELECT duration FROM album"), 1234.5);
    assert_eq!(row_i64(&store, "SELECT size FROM album"), 87654321);
    assert_eq!(
        row_text(&store, "SELECT discs FROM album"),
        r#"{"1":"disc-one","10":"disc-ten","2":"disc-two"}"#,
        "string-key order must be 1, 10, 2"
    );
    assert_eq!(
        row_text(&store, "SELECT sort_album_name FROM album"),
        "s-al"
    );
    assert_eq!(
        row_text(&store, "SELECT sort_album_artist_name FROM album"),
        "s-aa"
    );
    assert_eq!(
        row_text(&store, "SELECT order_album_name FROM album"),
        "o-al"
    );
    assert_eq!(
        row_text(&store, "SELECT order_album_artist_name FROM album"),
        "o-aa"
    );
    assert_eq!(row_text(&store, "SELECT catalog_num FROM album"), "CAT-1");
    assert_eq!(row_text(&store, "SELECT mbz_album_id FROM album"), "m-al");
    assert_eq!(
        row_text(&store, "SELECT mbz_album_artist_id FROM album"),
        "m-aa"
    );
    assert_eq!(
        row_text(&store, "SELECT mbz_album_type FROM album"),
        "m-atype"
    );
    assert_eq!(
        row_text(&store, "SELECT mbz_album_comment FROM album"),
        "m-ac"
    );
    assert_eq!(
        row_text(&store, "SELECT mbz_release_group_id FROM album"),
        "m-rg"
    );
    assert_eq!(
        row_text(&store, "SELECT folder_ids FROM album"),
        r#"["fold-1"]"#
    );
    assert_eq!(row_text(&store, "SELECT explicit_status FROM album"), "E");
    // RG shims: None binds NULL (no reader column for the shim itself — the
    // real rg_album_gain/rg_album_peak columns read back NULL).
    assert_eq!(
        row_opt_text(&store, "SELECT rg_album_gain FROM album"),
        None
    );
    assert_eq!(
        row_opt_text(&store, "SELECT rg_album_peak FROM album"),
        None
    );
    assert_eq!(row_text(&store, "SELECT description FROM album"), "d-al");
    assert_eq!(
        row_text(&store, "SELECT small_image_url FROM album"),
        "http://s"
    );
    assert_eq!(
        row_text(&store, "SELECT medium_image_url FROM album"),
        "http://m"
    );
    assert_eq!(
        row_text(&store, "SELECT large_image_url FROM album"),
        "http://l"
    );
    assert_eq!(
        row_text(&store, "SELECT external_url FROM album"),
        "http://e"
    );
    assert_eq!(
        row_text(&store, "SELECT external_info_updated_at FROM album"),
        "2024-07-01 05:06:07+00:00"
    );
    assert_eq!(row_text(&store, "SELECT genre FROM album"), "Rock");
    assert_eq!(row_i64(&store, "SELECT missing FROM album"), 1);
    // Go Put fidelity (album_repository.go:208-219): only imported_at is
    // stamped to `now`; created_at and updated_at pass the record through
    // unchanged.
    assert_eq!(
        row_text(&store, "SELECT imported_at FROM album"),
        "2024-07-01 14:15:16+00:00",
        "imported_at stamped to now at Put entry"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-07-01 02:03:04+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM album"),
        "2024-07-01 03:04:05+00:00",
        "updated_at passes through — Go Put never stamps it"
    );

    // PostMapArgs consistency: FTS columns pinned against literals — the
    // sort name lands in both FTS participant columns, the sub-role never
    // does, and the persisted participants JSON keeps the {id,name,subRole}
    // shape (no sortName key).
    let names = album_repo::participant_names(&a.participant_names);
    assert_eq!(
        names,
        vec!["Nils Frahm".to_string(), "Frahm, Nils".to_string()]
    );
    let tag_refs = |name: &str| -> Vec<&str> {
        a.tags
            .get(name)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    };
    let fts = nd_db::sql_feeders::album_fts_columns(
        &a.name,
        &a.sort_album_name,
        &a.album_artist,
        &names,
        &["disc-ten", "disc-one", "disc-two"],
        tag_refs("albumversion").as_slice(),
        tag_refs("catalognumber").as_slice(),
    );
    // `search_participants` is raw (sorted/deduped join): the exact sort
    // name lands, the sub-role never does. `full_text` is sanitized
    // (lowercased, commas stripped, words sorted): the sort name
    // contributes its words.
    let full_text = row_text(&store, "SELECT full_text FROM album");
    assert!(!full_text.trim().is_empty());
    assert!(
        full_text.contains("frahm") && full_text.contains("nils"),
        "sort name words in full_text"
    );
    assert!(!full_text.contains("vocals"), "sub_role never reaches FTS");
    assert_eq!(
        row_text(&store, "SELECT search_participants FROM album"),
        "Frahm, Nils Nils Frahm",
    );
    assert!(!row_text(&store, "SELECT search_participants FROM album").contains("Vocals"));
    assert_eq!(
        row_text(&store, "SELECT search_normalized FROM album"),
        fts.search_normalized
    );
    let participants_json = row_text(&store, "SELECT participants FROM album");
    assert!(participants_json.contains(r#""Nils Frahm""#));
    assert!(participants_json.contains(r#""Vocals""#));
    assert!(!participants_json.contains("sortName"));
    // Junction maintenance (fresh insert: deletes match 0, inserts place 1).
    assert_eq!(insert(&store, &put.participants_delete), 0);
    assert_eq!(
        insert(
            &store,
            put.participants_insert
                .as_ref()
                .expect("participants insert")
        ),
        1
    );
    assert_eq!(insert(&store, &put.tags_delete), 0);
    let tags_insert = put.tags_insert.clone().expect("tags insert");
    assert_eq!(insert(&store, &tags_insert), 1);
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM album_artists WHERE album_id = 'al-1'"
        ),
        1
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM album_tags WHERE album_id = 'al-1'"
        ),
        1
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT tag_id FROM album_tags WHERE album_id = 'al-1'"
        ),
        genre_id
    );

    // DDL orphan: `releases` never written, stays at its default 0.
    assert!(!put.insert.0.contains("releases"));
    assert_eq!(row_i64(&store, "SELECT releases FROM album"), 0);

    // Restricted external-info update: the six external columns move;
    // name/title/song_count/created_at/updated_at do not (Go
    // UpdateExternalInfo, album_repository.go:223-225).
    let mut a2 = a.clone();
    a2.description = "d-al-2".to_string();
    a2.small_image_url = "http://s2".to_string();
    a2.medium_image_url = "http://m2".to_string();
    a2.large_image_url = "http://l2".to_string();
    a2.external_url = "http://e2".to_string();
    a2.name = "MUST NOT CHANGE".to_string();
    a2.embed_art_path = "MUST NOT CHANGE".to_string();
    a2.song_count = 99;
    let ext = album_repo::update_external_info_sql("al-1", &a2, now).expect("renders");
    // Parse the SET list so `external_info_updated_at` cannot satisfy a
    // substring check for `updated_at`.
    let set = ext
        .update
        .0
        .strip_prefix("UPDATE album SET ")
        .and_then(|s| s.strip_suffix(" WHERE id = ?"))
        .expect("set clause");
    let cols: Vec<&str> = set
        .split(", ")
        .map(|pair| pair.split(" = ?").next().unwrap())
        .collect();
    assert_eq!(
        cols,
        vec![
            "description",
            "external_info_updated_at",
            "external_url",
            "large_image_url",
            "medium_image_url",
            "small_image_url",
        ]
    );
    assert_eq!(insert(&store, &ext.update), 1);
    assert_eq!(row_text(&store, "SELECT description FROM album"), "d-al-2");
    assert_eq!(
        row_text(&store, "SELECT small_image_url FROM album"),
        "http://s2"
    );
    assert_eq!(
        row_text(&store, "SELECT medium_image_url FROM album"),
        "http://m2"
    );
    assert_eq!(
        row_text(&store, "SELECT large_image_url FROM album"),
        "http://l2"
    );
    assert_eq!(
        row_text(&store, "SELECT external_url FROM album"),
        "http://e2"
    );
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Abbey Road");
    assert_eq!(row_i64(&store, "SELECT song_count FROM album"), 17);
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-07-01 02:03:04+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM album"),
        "2024-07-01 03:04:05+00:00",
        "updated_at untouched by the restricted update"
    );

    // created_at: None arm — NULL on the INSERT leg, passed through on the
    // update leg (it is never in the UPDATE set).
    let mut a3 = a.clone();
    a3.created_at = None;
    let put3 = album_repo::put_sql("al-2", &a3, dt("18:19:20"), &[]).expect("renders");
    assert_eq!(insert(&store, &put3.insert), 1);
    assert_eq!(
        row_opt_text(&store, "SELECT created_at FROM album WHERE id = 'al-2'"),
        None
    );
    assert!(!put3.update.0.contains("created_at = ?"));
    drop(store);
    std::fs::remove_file(path).ok();
}

/// RG Some arms + a follow-up update with some RG set and folder_ids None:
/// `folder_ids` stores `null` (nil) vs the array above, and the RG reals
/// survive a later update round-trip.
#[test]
fn album_record_rg_some_and_null_folder_ids_roundtrip() {
    let (store, path) = store();
    seed_library(&store);
    let mut a = album_record();
    a.rg_album_gain = Some(-8.25);
    a.rg_album_peak = Some(0.75);
    let put = album_repo::put_sql("al-1", &a, dt("14:15:16"), &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);
    assert_eq!(row_real(&store, "SELECT rg_album_gain FROM album"), -8.25);
    assert_eq!(row_real(&store, "SELECT rg_album_peak FROM album"), 0.75);

    // Update with folder_ids None (Go nil → null) and RG unset (→ NULL).
    let mut a2 = a.clone();
    a2.folder_ids = None;
    a2.rg_album_gain = None;
    a2.rg_album_peak = None;
    let up = album_repo::put_sql("al-1", &a2, dt("16:17:18"), &[]).expect("renders");
    assert_eq!(insert(&store, &up.update), 1);
    // `folder_ids` is `NOT NULL` jsonb: Go nil → the JSON literal `null`.
    assert_eq!(row_text(&store, "SELECT folder_ids FROM album"), "null");
    assert_eq!(
        row_opt_text(&store, "SELECT rg_album_gain FROM album"),
        None
    );
    assert_eq!(
        row_opt_text(&store, "SELECT rg_album_peak FROM album"),
        None
    );
    // Go Put stamps only imported_at; updated_at passes through the record
    // even on an update leg.
    assert_eq!(
        row_text(&store, "SELECT imported_at FROM album"),
        "2024-07-01 16:17:18+00:00",
        "imported_at re-stamped to the update's `now`"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM album"),
        "2024-07-01 03:04:05+00:00",
        "updated_at passes through — Go Put never stamps it"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Mediafile record write→read (S-D2b).
// ---------------------------------------------------------------------------

fn mediafile_record() -> MediaFileRecord {
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Rock".to_string()]);
    tags.insert("subtitle".to_string(), vec!["Live".to_string()]);
    let mut participants = Participants::new();
    participants.insert(
        Role::from("artist"),
        vec![Participant {
            id: "ar-1".to_string(),
            name: "Song Artist".to_string(),
            sub_role: "Vocals".to_string(),
        }],
    );
    // Go AllNames input: name + non-empty SortArtistName, never SubRole.
    let participant_names = vec![nd_db::album_repo::NamedParticipant {
        id: "ar-1".to_string(),
        name: "Song Artist".to_string(),
        sort_name: "Artist, Song".to_string(),
        sub_role: "Vocals".to_string(),
    }];
    MediaFileRecord {
        album: "Abbey Road".to_string(),
        album_artist: "The Beatles".to_string(),
        album_artist_id: "aa-m".to_string(),
        album_id: "al-m".to_string(),
        artist: "Song Artist".to_string(),
        artist_id: "ar-m".to_string(),
        birth_time: dt("00:01:02"),
        bit_depth: Some(24),
        bit_rate: 1411,
        bpm: Some(120),
        catalog_num: "CAT-M".to_string(),
        channels: 2,
        codec: "flac".to_string(),
        comment: "c-mf".to_string(),
        compilation: false,
        created_at: Some(dt("02:03:04")),
        date: "1969-09-26".to_string(),
        disc_number: 1,
        disc_subtitle: "d-sub".to_string(),
        duration: 456.75,
        explicit_status: "E".to_string(),
        folder_id: "fold-m".to_string(),
        genre: "Rock".to_string(),
        has_cover_art: true,
        library_id: 1,
        lyrics: "lyr".to_string(),
        mbz_recording_id: "m-r".to_string(),
        mbz_release_track_id: "m-rt".to_string(),
        mbz_album_id: "m-al".to_string(),
        mbz_release_group_id: "m-rg".to_string(),
        mbz_artist_id: "m-ar".to_string(),
        mbz_album_artist_id: "m-aa".to_string(),
        mbz_album_type: "m-at".to_string(),
        mbz_album_comment: "m-ac".to_string(),
        missing: false,
        order_album_artist_name: "oaa".to_string(),
        order_album_name: "oal".to_string(),
        order_artist_name: "oar".to_string(),
        order_title: "oti".to_string(),
        original_date: "1968-01-02".to_string(),
        original_year: 1968,
        path: "/music/a.mp3".to_string(),
        pid: "pid-m".to_string(),
        probe_data: r#"{"codec":"flac"}"#.to_string(),
        release_date: "1970-02-01".to_string(),
        release_year: 1970,
        rg_album_gain: None,
        rg_album_peak: None,
        rg_track_gain: None,
        rg_track_peak: None,
        sample_rate: 44100,
        size: 12345678,
        sort_album_artist_name: "saa".to_string(),
        sort_album_name: "sal".to_string(),
        sort_artist_name: "sar".to_string(),
        sort_title: "sti".to_string(),
        suffix: ".mp3".to_string(),
        tags,
        title: "Something".to_string(),
        track_number: 17,
        year: 1969,
        participants,
        participant_names,
        updated_at: Some(dt("03:04:05")),
    }
}

/// Mediafile record: all 63 columns distinct, 4 RG arms (NULL here, Some in
/// the follow-up), `birth_time` INSERT-only, `created_at` untouched by a
/// later update, the `full_title` subtitle behavior pinned both ways, and an
/// empty-participants update proving delete-with-no-insert.
#[test]
fn mediafile_record_insert_update_readback() {
    let (store, path) = store();
    seed_library(&store);
    seed_artist(&store, "ar-1", "Song Artist");
    let genre_id = nd_db::sql_tags::indexed_tag_ids(nd_db::sql_tags::DbTags::from([(
        "genre".to_string(),
        vec!["Rock".to_string()],
    )]))[0]
        .clone();
    insert(
        &store,
        &(
            "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?)".to_string(),
            vec![
                Value::from(genre_id.as_str()),
                Value::from("Rock"),
                Value::from("rock-mf"),
            ],
        ),
    );

    let m = mediafile_record();
    let put = mediafile_repo::put_sql("mf-1", &m, dt("14:15:16"), false, &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(
        row_text(&store, "SELECT id FROM media_file WHERE id = 'mf-1'"),
        "mf-1"
    );
    assert_eq!(
        row_text(&store, "SELECT album FROM media_file WHERE id = 'mf-1'"),
        "Abbey Road"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT album_artist FROM media_file WHERE id = 'mf-1'"
        ),
        "The Beatles"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT album_artist_id FROM media_file WHERE id = 'mf-1'"
        ),
        "aa-m"
    );
    assert_eq!(
        row_text(&store, "SELECT album_id FROM media_file WHERE id = 'mf-1'"),
        "al-m"
    );
    assert_eq!(
        row_text(&store, "SELECT artist FROM media_file WHERE id = 'mf-1'"),
        "Song Artist"
    );
    assert_eq!(
        row_text(&store, "SELECT artist_id FROM media_file WHERE id = 'mf-1'"),
        "ar-m"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT birth_time FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 00:01:02+00:00",
        "birth_time present on INSERT"
    );
    assert_eq!(
        row_i64(&store, "SELECT bit_depth FROM media_file WHERE id = 'mf-1'"),
        24
    );
    assert_eq!(
        row_i64(&store, "SELECT bit_rate FROM media_file WHERE id = 'mf-1'"),
        1411
    );
    assert_eq!(
        row_i64(&store, "SELECT bpm FROM media_file WHERE id = 'mf-1'"),
        120
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT catalog_num FROM media_file WHERE id = 'mf-1'"
        ),
        "CAT-M"
    );
    assert_eq!(
        row_i64(&store, "SELECT channels FROM media_file WHERE id = 'mf-1'"),
        2
    );
    assert_eq!(
        row_text(&store, "SELECT codec FROM media_file WHERE id = 'mf-1'"),
        "flac"
    );
    assert_eq!(
        row_text(&store, "SELECT comment FROM media_file WHERE id = 'mf-1'"),
        "c-mf"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT compilation FROM media_file WHERE id = 'mf-1'"
        ),
        0
    );
    // Go Put fidelity (mediafile_repository.go:184-190): created_at is
    // stamped to `now` only when the record's is zero; here it is Some, so
    // it passes through unchanged.
    assert_eq!(
        row_text(
            &store,
            "SELECT created_at FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 02:03:04+00:00",
        "created_at Some passes through (not re-stamped)"
    );
    assert_eq!(
        row_text(&store, "SELECT date FROM media_file WHERE id = 'mf-1'"),
        "1969-09-26"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT disc_number FROM media_file WHERE id = 'mf-1'"
        ),
        1
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT disc_subtitle FROM media_file WHERE id = 'mf-1'"
        ),
        "d-sub"
    );
    assert_eq!(
        row_real(&store, "SELECT duration FROM media_file WHERE id = 'mf-1'"),
        456.75
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT explicit_status FROM media_file WHERE id = 'mf-1'"
        ),
        "E"
    );
    assert_eq!(
        row_text(&store, "SELECT folder_id FROM media_file WHERE id = 'mf-1'"),
        "fold-m"
    );
    assert_eq!(
        row_text(&store, "SELECT genre FROM media_file WHERE id = 'mf-1'"),
        "Rock"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT has_cover_art FROM media_file WHERE id = 'mf-1'"
        ),
        1
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT library_id FROM media_file WHERE id = 'mf-1'"
        ),
        1
    );
    assert_eq!(
        row_text(&store, "SELECT lyrics FROM media_file WHERE id = 'mf-1'"),
        "lyr"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_recording_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-r"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_release_track_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-rt"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_album_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-al"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_release_group_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-rg"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_artist_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-ar"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_album_artist_id FROM media_file WHERE id = 'mf-1'"
        ),
        "m-aa"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_album_type FROM media_file WHERE id = 'mf-1'"
        ),
        "m-at"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT mbz_album_comment FROM media_file WHERE id = 'mf-1'"
        ),
        "m-ac"
    );
    assert_eq!(
        row_i64(&store, "SELECT missing FROM media_file WHERE id = 'mf-1'"),
        0
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT order_album_artist_name FROM media_file WHERE id = 'mf-1'"
        ),
        "oaa"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT order_album_name FROM media_file WHERE id = 'mf-1'"
        ),
        "oal"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT order_artist_name FROM media_file WHERE id = 'mf-1'"
        ),
        "oar"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT order_title FROM media_file WHERE id = 'mf-1'"
        ),
        "oti"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT original_date FROM media_file WHERE id = 'mf-1'"
        ),
        "1968-01-02"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT original_year FROM media_file WHERE id = 'mf-1'"
        ),
        1968
    );
    assert_eq!(
        row_text(&store, "SELECT path FROM media_file WHERE id = 'mf-1'"),
        "/music/a.mp3"
    );
    assert_eq!(
        row_text(&store, "SELECT pid FROM media_file WHERE id = 'mf-1'"),
        "pid-m"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT probe_data FROM media_file WHERE id = 'mf-1'"
        ),
        r#"{"codec":"flac"}"#
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT release_date FROM media_file WHERE id = 'mf-1'"
        ),
        "1970-02-01"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT release_year FROM media_file WHERE id = 'mf-1'"
        ),
        1970
    );
    // 4 RG NULL arms.
    assert_eq!(
        row_opt_text(
            &store,
            "SELECT rg_album_gain FROM media_file WHERE id = 'mf-1'"
        ),
        None
    );
    assert_eq!(
        row_opt_text(
            &store,
            "SELECT rg_album_peak FROM media_file WHERE id = 'mf-1'"
        ),
        None
    );
    assert_eq!(
        row_opt_text(
            &store,
            "SELECT rg_track_gain FROM media_file WHERE id = 'mf-1'"
        ),
        None
    );
    assert_eq!(
        row_opt_text(
            &store,
            "SELECT rg_track_peak FROM media_file WHERE id = 'mf-1'"
        ),
        None
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT sample_rate FROM media_file WHERE id = 'mf-1'"
        ),
        44100
    );
    assert_eq!(
        row_i64(&store, "SELECT size FROM media_file WHERE id = 'mf-1'"),
        12345678
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sort_album_artist_name FROM media_file WHERE id = 'mf-1'"
        ),
        "saa"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sort_album_name FROM media_file WHERE id = 'mf-1'"
        ),
        "sal"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sort_artist_name FROM media_file WHERE id = 'mf-1'"
        ),
        "sar"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sort_title FROM media_file WHERE id = 'mf-1'"
        ),
        "sti"
    );
    assert_eq!(
        row_text(&store, "SELECT suffix FROM media_file WHERE id = 'mf-1'"),
        ".mp3"
    );
    assert_eq!(
        row_text(&store, "SELECT title FROM media_file WHERE id = 'mf-1'"),
        "Something"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT track_number FROM media_file WHERE id = 'mf-1'"
        ),
        17
    );
    assert_eq!(
        row_i64(&store, "SELECT year FROM media_file WHERE id = 'mf-1'"),
        1969
    );
    // updated_at passes through the record (Go Put never stamps it).
    assert_eq!(
        row_text(
            &store,
            "SELECT updated_at FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 03:04:05+00:00",
        "updated_at passes through — Go Put never stamps it"
    );

    // full_title pinned both ways (config switch is the caller's).
    assert_eq!(mediafile_repo::full_title(&m, false), "Something");
    assert_eq!(mediafile_repo::full_title(&m, true), "Something (Live)");
    // The stored full_text used append_subtitle=false (the put's argument).
    let fts = nd_db::sql_feeders::mediafile_fts_columns(
        &m.title,
        &m.album,
        &m.artist,
        &m.album_artist,
        &m.sort_title,
        &m.sort_album_name,
        &m.sort_artist_name,
        &m.sort_album_artist_name,
        &m.disc_subtitle,
        &album_repo::participant_names(&m.participant_names),
    );
    assert_eq!(
        row_text(&store, "SELECT full_text FROM media_file WHERE id = 'mf-1'"),
        fts.full_text
    );
    // Pinned literals rather than a recomputation of the same helper: the
    // sort name must reach both FTS columns and the sub-role must not
    // (Go `AllNames`, model/participants.go:174-185).
    assert_eq!(
        album_repo::participant_names(&m.participant_names),
        vec!["Song Artist".to_string(), "Artist, Song".to_string()]
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT search_participants FROM media_file WHERE id = 'mf-1'"
        ),
        "Artist, Song Song Artist",
        "sort name lands in search_participants"
    );
    assert!(
        !row_text(
            &store,
            "SELECT search_participants FROM media_file WHERE id = 'mf-1'"
        )
        .contains("Vocals"),
        "sub_role never reaches FTS"
    );
    assert!(
        !row_text(&store, "SELECT full_text FROM media_file WHERE id = 'mf-1'").contains("vocals"),
        "sub_role never reaches full_text"
    );
    assert!(
        row_text(
            &store,
            "SELECT participants FROM media_file WHERE id = 'mf-1'"
        )
        .contains(r#""Vocals""#),
        "the persisted {{id,name,subRole}} JSON is unaffected by the FTS input"
    );
    assert!(
        !row_text(
            &store,
            "SELECT participants FROM media_file WHERE id = 'mf-1'"
        )
        .contains("sortName"),
        "sort_name is input-only and must never be persisted"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT search_normalized FROM media_file WHERE id = 'mf-1'"
        ),
        fts.search_normalized
    );
    assert!(
        row_text(&store, "SELECT tags FROM media_file WHERE id = 'mf-1'").contains(r#""Rock""#)
    );
    assert!(row_text(
        &store,
        "SELECT participants FROM media_file WHERE id = 'mf-1'"
    )
    .contains(r#""Vocals""#));
    // Junction rows incl. sub_role, joined to the seeded artist.
    assert_eq!(insert(&store, &put.participants_delete), 0);
    assert_eq!(
        insert(
            &store,
            put.participants_insert
                .as_ref()
                .expect("participants insert")
        ),
        1
    );
    assert_eq!(insert(&store, &put.tags_delete), 0);
    let tags_insert = put.tags_insert.clone().expect("tags insert");
    assert_eq!(insert(&store, &tags_insert), 1);
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        1
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sub_role FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        "Vocals"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM media_file_tags WHERE media_file_id = 'mf-1'"
        ),
        1
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT tag_id FROM media_file_tags WHERE media_file_id = 'mf-1'"
        ),
        genre_id
    );

    // A later update: birth_time and created_at are absent from the SET; the
    // RG Some arms land; an empty-participants put deletes without
    // inserting; updated_at passes through unchanged.
    let mut m2 = m.clone();
    m2.rg_album_gain = Some(-8.25);
    m2.rg_album_peak = Some(0.75);
    m2.rg_track_gain = Some(-9.5);
    m2.rg_track_peak = Some(0.55);
    m2.participants.clear();
    let up = mediafile_repo::put_sql("mf-1", &m2, dt("16:17:18"), false, &[]).expect("renders");
    // The created_at: None arm (Go zero time.Time): Go Put stamps
    // CreatedAt = now when zero; on the INSERT leg that is visible. A fresh
    // row with a distinct path/id exercises it.
    let mut m3 = m.clone();
    m3.created_at = None;
    m3.path = "/music/c.mp3".to_string();
    let ins3 = mediafile_repo::put_sql("mf-2", &m3, dt("18:19:20"), false, &[]).expect("renders");
    assert_eq!(insert(&store, &ins3.insert), 1);
    assert_eq!(
        row_text(
            &store,
            "SELECT created_at FROM media_file WHERE id = 'mf-2'"
        ),
        "2024-07-01 18:19:20+00:00",
        "created_at None is stamped to the put's `now` on INSERT"
    );
    assert!(
        !up.update.0.contains("birth_time = ?"),
        "birth_time never updated"
    );
    assert!(
        !up.update.0.contains("created_at = ?"),
        "created_at never updated"
    );
    assert!(
        up.participants_insert.is_none(),
        "empty participants -> no insert"
    );
    assert_eq!(insert(&store, &up.update), 1);
    assert_eq!(insert(&store, &up.participants_delete), 1);
    assert_eq!(
        row_i64(&store, "SELECT count(*) FROM media_file_artists"),
        0
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT birth_time FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 00:01:02+00:00",
        "birth_time survives the update"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT created_at FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 02:03:04+00:00",
        "created_at survives the update"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT updated_at FROM media_file WHERE id = 'mf-1'"
        ),
        "2024-07-01 03:04:05+00:00",
        "updated_at passes through the UPDATE leg too (the put's `now` was 16:17:18)"
    );
    assert_eq!(
        row_real(
            &store,
            "SELECT rg_album_gain FROM media_file WHERE id = 'mf-1'"
        ),
        -8.25
    );
    assert_eq!(
        row_real(
            &store,
            "SELECT rg_album_peak FROM media_file WHERE id = 'mf-1'"
        ),
        0.75
    );
    assert_eq!(
        row_real(
            &store,
            "SELECT rg_track_gain FROM media_file WHERE id = 'mf-1'"
        ),
        -9.5
    );
    assert_eq!(
        row_real(
            &store,
            "SELECT rg_track_peak FROM media_file WHERE id = 'mf-1'"
        ),
        0.55
    );

    // A camelCase restricted update touches only probe_data.
    let restricted = mediafile_repo::put_sql("mf-1", &m, dt("16:17:18"), false, &["ProbeData"])
        .expect("renders");
    assert_eq!(
        restricted.update.0,
        "UPDATE media_file SET probe_data = ? WHERE id = ?"
    );
    assert_eq!(insert(&store, &restricted.update), 1);

    drop(store);
    std::fs::remove_file(path).ok();
}
