//! INSERT column/value alignment, EXECUTED against a real SQLite DB.
//!
//! This port had a silent data-corruption bug in the three caller-column `Put`
//! renderers: the column list ended with `id`
//! (`columns.chain(["id"])`) while the VALUES tuple began with it
//! (`vec![Value::from(id)]` then `values`). For any insert with more than one
//! column every value landed in the wrong column — verified against SQLite
//! before the fix, where `INSERT INTO album (name, album_artist, id) VALUES
//! ('alb-1', 'Song Title', 'Jane Doe')` stored `id = 'Jane Doe'`.
//!
//! Go cannot have this bug: squirrel's `SetMap` sorts the column names and
//! emits the values positionally against that same order
//! (`squirrel@v1.5.4/insert.go:271-283`), and Go's `put` appends `id` to the
//! values map (`sql_base_repository.go:637-641`). The invariant is therefore
//! "columns and args are emitted together", not "id binds first".
//!
//! `playlist_repo` had a second, separate defect: its insert leg rendered
//! `INSERT INTO playlist SET …`, which is MySQL syntax SQLite rejects outright
//! (`near "SET": syntax error`). That is asserted here too.
//!
//! Renderer tests alone could not catch any of this — they only compared the
//! strings the renderer produced, including the wrong order. These tests run
//! the statements and read the stored row back.
//!
//! Harness shape is `tests/runtime_player_repo.rs:34-46`: one isolated on-disk
//! temp DB per test, `apply_fresh_schema` through `nd_dbschema`, and `Pool` so
//! `SEEDEDRAND`/`NATSORT` are registered per connection. `_foreign_keys=on` is
//! Go's `db/db.go:55` DSN parameter.

use nd_db::album_repo;
use nd_db::folder_repo::{self, FolderRecord};
use nd_db::mediafile_repo;
use nd_db::playlist_repo::{self, PlaylistRecord};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

/// `rusqlite::types::Value` for a [`Value`] — the mapping the repositories do
/// internally (`share_repo::bind_values`), reproduced so the test drives only
/// the public statements.
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
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-insert-", ".db"));
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

fn text(store: &SQLStore, sql: &str) -> String {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |row| row.get::<_, String>(0)))
        .unwrap()
}
/// A single integer column as an i64 (the `text` helper is TEXT-only).
fn number(store: &SQLStore, sql: &str) -> i64 {
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |row| row.get::<_, i64>(0)))
        .unwrap()
}
/// `album`, `folder` and `media_file` all reference `library` (and album/
/// media_file default `library_id = 1`), so the library row has to exist
/// before any of them can be inserted with `_foreign_keys=on`.
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

/// The exact shape of the bug: distinct values per column, so a rotation is
/// visible. `id` is deliberately a value no other column could plausibly hold.
/// A rotation-detector album record: every column carries a distinct value
/// typed for its own column, so any value landing in the wrong column is
/// visible in the per-column read-backs below (S-D2b).
fn album_record() -> nd_db::album_repo::AlbumRecord {
    use nd_db::album_repo::AlbumRecord;
    use nd_db::sql_participations::{Participant, Participants, Role};
    use nd_db::sql_tags::DbTags;
    use std::collections::BTreeMap;
    let mut discs = BTreeMap::new();
    discs.insert(1, "disc-one".to_string());
    discs.insert(2, "disc-two".to_string());
    discs.insert(10, "disc-ten".to_string());
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Rock".to_string()]);
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    tags.insert("catalognumber".to_string(), vec!["CAT-1".to_string()]);
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-1".to_string(),
            name: "Nils Frahm".to_string(),
            sub_role: "Vocals".to_string(),
        }],
    );
    // Go AllNames input: name + SortArtistName, never SubRole.
    let participant_names = vec![nd_db::album_repo::NamedParticipant {
        id: "ar-1".to_string(),
        name: "Nils Frahm".to_string(),
        sort_name: "Frahm, Nils".to_string(),
        sub_role: "Vocals".to_string(),
    }];
    AlbumRecord {
        library_id: 1,
        name: "Abbey Road".to_string(),
        embed_art_path: "/music/cover.jpg".to_string(),
        album_artist: "Nils Frahm".to_string(),
        album_artist_id: "aa-id-1".to_string(),
        min_year: 1969,
        max_year: 1970,
        date: "1969-09-26".to_string(),
        min_original_year: 1968,
        max_original_year: 1968,
        original_date: "1968-01-02".to_string(),
        release_date: "1970-02-01".to_string(),
        compilation: true,
        comment: "c-album".to_string(),
        song_count: 17,
        duration: 1234.5,
        size: 87654321,
        discs: Some(discs),
        sort_album_name: "s-aby".to_string(),
        sort_album_artist_name: "s-bea".to_string(),
        order_album_name: "o-aby".to_string(),
        order_album_artist_name: "o-bea".to_string(),
        catalog_num: "CAT-1".to_string(),
        mbz_album_id: "mbz-al".to_string(),
        mbz_album_artist_id: "mbz-aa".to_string(),
        mbz_album_type: "mbz-atype".to_string(),
        mbz_album_comment: "mbz-ac".to_string(),
        mbz_release_group_id: "mbz-rg".to_string(),
        folder_ids: Some(vec!["fold-1".to_string()]),
        explicit_status: "E".to_string(),
        rg_album_gain: Some(-8.5),
        rg_album_peak: Some(0.99),
        description: "desc-al".to_string(),
        small_image_url: "http://img/s".to_string(),
        medium_image_url: "http://img/m".to_string(),
        large_image_url: "http://img/l".to_string(),
        external_url: "http://ext/al".to_string(),
        external_info_updated_at: Some(stamp()),
        genre: "Rock".to_string(),
        tags,
        participants,
        participant_names,
        missing: true,
        imported_at: stamp(),
        created_at: Some(stamp()),
        updated_at: Some(stamp()),
    }
}
/// `album` + a seeded artist/tag: distinct value per column, every column
/// read back in its own column (the record's 45 columns + `id`), the
/// DDL-orphan `releases` never written (stays at its default 0), and the
/// PostMapArgs FTS/junction columns landing consistently with the record.
/// A restricted `UpdateExternalInfo` (the six external columns) then proves
/// name/title/song_count/created_at are untouched by the UPDATE leg.
#[test]
fn album_insert_stores_each_value_in_its_own_column() {
    let (store, path) = store();
    seed_library(&store);
    insert(
        &store,
        &(
            "INSERT INTO artist (id, name, order_artist_name) VALUES (?, ?, ?)".to_string(),
            vec![
                Value::from("ar-1"),
                Value::from("Nils Frahm"),
                Value::from(""),
            ],
        ),
    );
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
                Value::from(genre_id),
                Value::from("Rock"),
                Value::from("rock-al"),
            ],
        ),
    );
    let rec = album_record();
    let put = album_repo::put_sql("al-1", &rec, stamp(), &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);
    // --- rotation detector: every record column in its own column ---
    assert_eq!(text(&store, "SELECT id FROM album"), "al-1");
    assert_eq!(
        text(&store, "SELECT CAST(name AS TEXT) FROM album"),
        "Abbey Road"
    );
    assert_eq!(
        text(&store, "SELECT CAST(embed_art_path AS TEXT) FROM album"),
        "/music/cover.jpg"
    );
    assert_eq!(
        text(&store, "SELECT CAST(album_artist AS TEXT) FROM album"),
        "Nils Frahm"
    );
    assert_eq!(
        text(&store, "SELECT CAST(album_artist_id AS TEXT) FROM album"),
        "aa-id-1"
    );
    assert_eq!(number(&store, "SELECT min_year FROM album"), 1969);
    assert_eq!(number(&store, "SELECT max_year FROM album"), 1970);
    assert_eq!(
        text(&store, "SELECT CAST(date AS TEXT) FROM album"),
        "1969-09-26"
    );
    assert_eq!(number(&store, "SELECT min_original_year FROM album"), 1968);
    assert_eq!(number(&store, "SELECT max_original_year FROM album"), 1968);
    assert_eq!(
        text(&store, "SELECT CAST(original_date AS TEXT) FROM album"),
        "1968-01-02"
    );
    assert_eq!(
        text(&store, "SELECT CAST(release_date AS TEXT) FROM album"),
        "1970-02-01"
    );
    assert_eq!(number(&store, "SELECT compilation FROM album"), 1);
    assert_eq!(
        text(&store, "SELECT CAST(comment AS TEXT) FROM album"),
        "c-album"
    );
    assert_eq!(number(&store, "SELECT song_count FROM album"), 17);
    assert_eq!(
        text(&store, "SELECT CAST(duration AS TEXT) FROM album"),
        "1234.5"
    );
    assert_eq!(number(&store, "SELECT size FROM album"), 87654321);
    assert_eq!(
        text(&store, "SELECT CAST(discs AS TEXT) FROM album"),
        r#"{"1":"disc-one","10":"disc-ten","2":"disc-two"}"#
    );
    assert_eq!(
        text(&store, "SELECT CAST(sort_album_name AS TEXT) FROM album"),
        "s-aby"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(sort_album_artist_name AS TEXT) FROM album"
        ),
        "s-bea"
    );
    assert_eq!(
        text(&store, "SELECT CAST(order_album_name AS TEXT) FROM album"),
        "o-aby"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(order_album_artist_name AS TEXT) FROM album"
        ),
        "o-bea"
    );
    assert_eq!(
        text(&store, "SELECT CAST(catalog_num AS TEXT) FROM album"),
        "CAT-1"
    );
    assert_eq!(
        text(&store, "SELECT CAST(mbz_album_id AS TEXT) FROM album"),
        "mbz-al"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_album_artist_id AS TEXT) FROM album"
        ),
        "mbz-aa"
    );
    assert_eq!(
        text(&store, "SELECT CAST(mbz_album_type AS TEXT) FROM album"),
        "mbz-atype"
    );
    assert_eq!(
        text(&store, "SELECT CAST(mbz_album_comment AS TEXT) FROM album"),
        "mbz-ac"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_release_group_id AS TEXT) FROM album"
        ),
        "mbz-rg"
    );
    assert_eq!(
        text(&store, "SELECT CAST(folder_ids AS TEXT) FROM album"),
        r#"["fold-1"]"#
    );
    assert_eq!(
        text(&store, "SELECT CAST(explicit_status AS TEXT) FROM album"),
        "E"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rg_album_gain AS TEXT) FROM album"),
        "-8.5"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rg_album_peak AS TEXT) FROM album"),
        "0.99"
    );
    assert_eq!(
        text(&store, "SELECT CAST(description AS TEXT) FROM album"),
        "desc-al"
    );
    assert_eq!(
        text(&store, "SELECT CAST(small_image_url AS TEXT) FROM album"),
        "http://img/s"
    );
    assert_eq!(
        text(&store, "SELECT CAST(medium_image_url AS TEXT) FROM album"),
        "http://img/m"
    );
    assert_eq!(
        text(&store, "SELECT CAST(large_image_url AS TEXT) FROM album"),
        "http://img/l"
    );
    assert_eq!(
        text(&store, "SELECT CAST(external_url AS TEXT) FROM album"),
        "http://ext/al"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(external_info_updated_at AS TEXT) FROM album"
        ),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(
        text(&store, "SELECT CAST(genre AS TEXT) FROM album"),
        "Rock"
    );
    assert_eq!(number(&store, "SELECT missing FROM album"), 1);
    assert_eq!(
        text(&store, "SELECT CAST(imported_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(
        text(&store, "SELECT CAST(created_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(
        text(&store, "SELECT CAST(updated_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00"
    );
    // --- PostMapArgs columns, consistent with the record's typed data ---
    let tag_refs = |name: &str| -> Vec<&str> {
        rec.tags
            .get(name)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    };
    let fts = nd_db::sql_feeders::album_fts_columns(
        &rec.name,
        &rec.sort_album_name,
        &rec.album_artist,
        &nd_db::album_repo::participant_names(&rec.participant_names),
        &["disc-ten", "disc-one", "disc-two"],
        tag_refs("albumversion").as_slice(),
        tag_refs("catalognumber").as_slice(),
    );
    assert!(!text(&store, "SELECT CAST(full_text AS TEXT) FROM album")
        .trim()
        .is_empty());
    assert_eq!(
        text(&store, "SELECT CAST(full_text AS TEXT) FROM album"),
        fts.full_text
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(search_participants AS TEXT) FROM album"
        ),
        fts.search_participants
    );
    assert_eq!(
        text(&store, "SELECT CAST(search_normalized AS TEXT) FROM album"),
        fts.search_normalized
    );
    // tags JSON carries every tag; execute the junction maintenance (Go's Put
    // runs it in the same transaction) and check the seeded ids landed.
    assert!(text(&store, "SELECT CAST(tags AS TEXT) FROM album").contains(r#""Rock""#));
    // participants JSON keeps the {id,name,subRole} shape — sub_role stays,
    // no sortName key is ever serialized (Go parity, sql_participations.go:13-17).
    let participants_json = text(&store, "SELECT CAST(participants AS TEXT) FROM album");
    assert!(participants_json.contains(r#""Nils Frahm""#));
    assert!(participants_json.contains(r#""Vocals""#));
    assert!(!participants_json.contains("sortName"));
    // FTS: the sort name lands in both columns; the sub-role never does.
    // `search_participants` is raw (sorted, deduped, joined): the exact
    // sort name string is present. `full_text` is sanitized (lowercased,
    // commas stripped, words sorted): the sort name contributes its words.
    let full_text = text(&store, "SELECT CAST(full_text AS TEXT) FROM album");
    let search_participants = text(
        &store,
        "SELECT CAST(search_participants AS TEXT) FROM album",
    );
    assert!(search_participants.contains("Frahm, Nils"));
    assert!(search_participants.contains("Nils Frahm"));
    assert!(!search_participants.contains("Vocals"));
    assert!(full_text.contains("frahm") && full_text.contains("nils"));
    assert!(!full_text.contains("vocals"));
    // Fresh insert: the junction tables are empty, so the deletes match 0 rows
    // and the inserts place the single seeded id each.
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
        number(
            &store,
            "SELECT count(*) FROM album_artists WHERE album_id = 'al-1'"
        ),
        1i64
    );
    assert_eq!(number(&store, "SELECT count(*) FROM album_artists aa WHERE album_id = 'al-1' AND aa.role = 'albumartist' AND EXISTS (SELECT 1 FROM artist a WHERE a.id = aa.artist_id AND a.name = 'Nils Frahm')"), 1i64);
    assert_eq!(
        number(
            &store,
            "SELECT count(*) FROM album_tags WHERE album_id = 'al-1'"
        ),
        1i64
    );
    // --- DDL orphan: `releases` is never written; the default 0 holds ---
    assert!(!put.insert.0.contains("releases"));
    assert_eq!(number(&store, "SELECT releases FROM album"), 0);
    // --- restricted external-info update: only the six columns move ---
    let mut rec2 = rec.clone();
    rec2.description = "desc-al-2".to_string();
    rec2.small_image_url = "http://img/s2".to_string();
    rec2.medium_image_url = "http://img/m2".to_string();
    rec2.large_image_url = "http://img/l2".to_string();
    rec2.external_url = "http://ext/al2".to_string();
    rec2.name = "MUST NOT CHANGE".to_string();
    rec2.embed_art_path = "MUST NOT CHANGE".to_string();
    rec2.song_count = 99;
    rec2.created_at = Some(stamp());
    let ext = album_repo::update_external_info_sql("al-1", &rec2, stamp()).expect("renders");
    // Go Put/UpdateExternalInfo never touch updated_at
    // (album_repository.go:208-219/223-225). Parse the SET list so
    // `external_info_updated_at` cannot satisfy a substring check.
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
    assert_eq!(
        text(&store, "SELECT CAST(updated_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00",
        "updated_at passes through the record — Go Put never stamps it"
    );
    assert_eq!(
        text(&store, "SELECT CAST(imported_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00",
        "imported_at is stamped to the put's `now` at Put entry"
    );
    assert_eq!(
        text(&store, "SELECT CAST(description AS TEXT) FROM album"),
        "desc-al-2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(small_image_url AS TEXT) FROM album"),
        "http://img/s2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(medium_image_url AS TEXT) FROM album"),
        "http://img/m2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(large_image_url AS TEXT) FROM album"),
        "http://img/l2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(external_url AS TEXT) FROM album"),
        "http://ext/al2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(name AS TEXT) FROM album"),
        "Abbey Road"
    );
    assert_eq!(number(&store, "SELECT song_count FROM album"), 17);
    assert_eq!(
        text(&store, "SELECT CAST(created_at AS TEXT) FROM album"),
        "2024-01-01 00:00:00+00:00"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

/// Superseded in scope by `runtime_marshal_records.rs::folder_record_insert_update_readback`,
/// but the rotation regression itself stays pinned here: the INSERT leg renders a
/// column list plus a positional VALUES tuple with `id` appended to both, and each
/// value lands in its own column. A [`FolderRecord`] carries a distinct value per
/// column, so a rotation is detectable without hand-building a column list the
/// record API no longer accepts.
#[test]
fn folder_insert_stores_each_value_in_its_own_column() {
    let (store, path) = store();
    seed_library(&store);
    // folder.library_id is NOT NULL without a default in the fresh DDL.
    let record = FolderRecord {
        // Read-path identity (never in folder_row): the row's own id.
        id: "f-1".to_string(),
        library_id: 1,
        path: "music/rock".to_string(),
        name: "Rock".to_string(),
        parent_id: String::new(),
        num_audio_files: 3,
        num_playlists: 0,
        image_files: None,
        images_updated_at: stamp(),
        hash: "H-1".to_string(),
        missing: false,
        updated_at: stamp(),
        created_at: Some(stamp()),
        // Hydration-owned join-only field: absent from folder_row.
        library_path: String::new(),
    };
    let put = folder_repo::put_sql("f-1", &record, stamp(), &[]).expect("renders");

    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(text(&store, "SELECT id FROM folder"), "f-1");
    assert_eq!(text(&store, "SELECT name FROM folder"), "Rock");
    assert_eq!(
        text(&store, "SELECT CAST(num_audio_files AS TEXT) FROM folder"),
        "3"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

/// An explicit timestamp bind: the DDL's localtime defaults are never relied on
/// (next-work.md's 2026-11-11 decision).
fn stamp() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// A rotation-detector media file record: every column carries a distinct
/// value typed for its own column (the 63 non-id persisted columns).
fn mediafile_record() -> nd_db::mediafile_repo::MediaFileRecord {
    use nd_db::mediafile_repo::MediaFileRecord;
    use nd_db::sql_participations::{Participant, Participants, Role};
    use nd_db::sql_tags::DbTags;
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
    // Go AllNames input: name + SortArtistName, never SubRole.
    let participant_names = vec![nd_db::album_repo::NamedParticipant {
        id: "ar-1".to_string(),
        name: "Song Artist".to_string(),
        sort_name: "Artist, Song".to_string(),
        sub_role: "Vocals".to_string(),
    }];
    MediaFileRecord {
        album: "Abbey Road".to_string(),
        album_artist: "The Beatles".to_string(),
        album_artist_id: "aa-id-2".to_string(),
        album_id: "al-m".to_string(),
        artist: "Song Artist".to_string(),
        artist_id: "ar-m".to_string(),
        birth_time: stamp(),
        bit_depth: Some(24),
        bit_rate: 1411,
        bpm: Some(120),
        catalog_num: "CAT-M".to_string(),
        channels: 2,
        codec: "flac".to_string(),
        comment: "c-mediafile".to_string(),
        compilation: false,
        created_at: Some(stamp()),
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
        mbz_recording_id: "mbz-r".to_string(),
        mbz_release_track_id: "mbz-rt".to_string(),
        mbz_album_id: "mbz-al".to_string(),
        mbz_release_group_id: "mbz-rg".to_string(),
        mbz_artist_id: "mbz-ar".to_string(),
        mbz_album_artist_id: "mbz-aa".to_string(),
        mbz_album_type: "mbz-at".to_string(),
        mbz_album_comment: "mbz-ac".to_string(),
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
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        rg_track_gain: Some(-9.5),
        rg_track_peak: Some(0.55),
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
        updated_at: Some(stamp()),
    }
}

/// `media_file` + a seeded artist/tag: distinct value per column, every
/// column read back in its own column (the record's 63 columns + `id`),
/// the 4 RG columns NULL/Some arms, `birth_time` INSERT-only, `full_title"
/// subtitle behavior pinned (`append_subtitle` true/false), participants/
/// tags JSON + junctions incl. sub_role, an empty-participants update
/// proving delete-with-no-insert, and a camelCase restricted update
/// (`["ProbeData"]`) touching only probe_data.
#[test]
fn mediafile_insert_stores_each_value_in_its_own_column() {
    let (store, path) = store();
    seed_library(&store);
    insert(
        &store,
        &(
            "INSERT INTO artist (id, name, order_artist_name) VALUES (?, ?, ?)".to_string(),
            vec![
                Value::from("ar-1"),
                Value::from("Song Artist"),
                Value::from(""),
            ],
        ),
    );
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
    let rec = mediafile_record();
    let put = mediafile_repo::put_sql("mf-1", &rec, stamp(), false, &[]).expect("renders");
    assert_eq!(insert(&store, &put.insert), 1);
    // --- rotation detector: every record column in its own column ---
    assert_eq!(text(&store, "SELECT id FROM media_file"), "mf-1");
    assert_eq!(
        text(&store, "SELECT CAST(album AS TEXT) FROM media_file"),
        "Abbey Road"
    );
    assert_eq!(
        text(&store, "SELECT CAST(album_artist AS TEXT) FROM media_file"),
        "The Beatles"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(album_artist_id AS TEXT) FROM media_file"
        ),
        "aa-id-2"
    );
    assert_eq!(
        text(&store, "SELECT CAST(album_id AS TEXT) FROM media_file"),
        "al-m"
    );
    assert_eq!(
        text(&store, "SELECT CAST(artist AS TEXT) FROM media_file"),
        "Song Artist"
    );
    assert_eq!(
        text(&store, "SELECT CAST(artist_id AS TEXT) FROM media_file"),
        "ar-m"
    );
    assert_eq!(
        text(&store, "SELECT CAST(birth_time AS TEXT) FROM media_file"),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(number(&store, "SELECT bit_depth FROM media_file"), 24);
    assert_eq!(number(&store, "SELECT bit_rate FROM media_file"), 1411);
    assert_eq!(number(&store, "SELECT bpm FROM media_file"), 120);
    assert_eq!(
        text(&store, "SELECT CAST(catalog_num AS TEXT) FROM media_file"),
        "CAT-M"
    );
    assert_eq!(number(&store, "SELECT channels FROM media_file"), 2);
    assert_eq!(
        text(&store, "SELECT CAST(codec AS TEXT) FROM media_file"),
        "flac"
    );
    assert_eq!(
        text(&store, "SELECT CAST(comment AS TEXT) FROM media_file"),
        "c-mediafile"
    );
    assert_eq!(number(&store, "SELECT compilation FROM media_file"), 0);
    assert_eq!(
        text(&store, "SELECT CAST(created_at AS TEXT) FROM media_file"),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(
        text(&store, "SELECT CAST(date AS TEXT) FROM media_file"),
        "1969-09-26"
    );
    assert_eq!(number(&store, "SELECT disc_number FROM media_file"), 1);
    assert_eq!(
        text(&store, "SELECT CAST(disc_subtitle AS TEXT) FROM media_file"),
        "d-sub"
    );
    assert_eq!(
        text(&store, "SELECT CAST(duration AS TEXT) FROM media_file"),
        "456.75"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(explicit_status AS TEXT) FROM media_file"
        ),
        "E"
    );
    assert_eq!(
        text(&store, "SELECT CAST(folder_id AS TEXT) FROM media_file"),
        "fold-m"
    );
    assert_eq!(
        text(&store, "SELECT CAST(genre AS TEXT) FROM media_file"),
        "Rock"
    );
    assert_eq!(number(&store, "SELECT has_cover_art FROM media_file"), 1);
    assert_eq!(number(&store, "SELECT library_id FROM media_file"), 1);
    assert_eq!(
        text(&store, "SELECT CAST(lyrics AS TEXT) FROM media_file"),
        "lyr"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_recording_id AS TEXT) FROM media_file"
        ),
        "mbz-r"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_release_track_id AS TEXT) FROM media_file"
        ),
        "mbz-rt"
    );
    assert_eq!(
        text(&store, "SELECT CAST(mbz_album_id AS TEXT) FROM media_file"),
        "mbz-al"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_release_group_id AS TEXT) FROM media_file"
        ),
        "mbz-rg"
    );
    assert_eq!(
        text(&store, "SELECT CAST(mbz_artist_id AS TEXT) FROM media_file"),
        "mbz-ar"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_album_artist_id AS TEXT) FROM media_file"
        ),
        "mbz-aa"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_album_type AS TEXT) FROM media_file"
        ),
        "mbz-at"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(mbz_album_comment AS TEXT) FROM media_file"
        ),
        "mbz-ac"
    );
    assert_eq!(number(&store, "SELECT missing FROM media_file"), 0);
    assert_eq!(
        text(
            &store,
            "SELECT CAST(order_album_artist_name AS TEXT) FROM media_file"
        ),
        "oaa"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(order_album_name AS TEXT) FROM media_file"
        ),
        "oal"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(order_artist_name AS TEXT) FROM media_file"
        ),
        "oar"
    );
    assert_eq!(
        text(&store, "SELECT CAST(order_title AS TEXT) FROM media_file"),
        "oti"
    );
    assert_eq!(
        text(&store, "SELECT CAST(original_date AS TEXT) FROM media_file"),
        "1968-01-02"
    );
    assert_eq!(number(&store, "SELECT original_year FROM media_file"), 1968);
    assert_eq!(
        text(&store, "SELECT CAST(path AS TEXT) FROM media_file"),
        "/music/a.mp3"
    );
    assert_eq!(
        text(&store, "SELECT CAST(pid AS TEXT) FROM media_file"),
        "pid-m"
    );
    assert_eq!(
        text(&store, "SELECT CAST(probe_data AS TEXT) FROM media_file"),
        r#"{"codec":"flac"}"#
    );
    assert_eq!(
        text(&store, "SELECT CAST(release_date AS TEXT) FROM media_file"),
        "1970-02-01"
    );
    assert_eq!(number(&store, "SELECT release_year FROM media_file"), 1970);
    assert_eq!(
        text(&store, "SELECT CAST(rg_album_gain AS TEXT) FROM media_file"),
        "-8.25"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rg_album_peak AS TEXT) FROM media_file"),
        "0.75"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rg_track_gain AS TEXT) FROM media_file"),
        "-9.5"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rg_track_peak AS TEXT) FROM media_file"),
        "0.55"
    );
    assert_eq!(number(&store, "SELECT sample_rate FROM media_file"), 44100);
    assert_eq!(number(&store, "SELECT size FROM media_file"), 12345678);
    assert_eq!(
        text(
            &store,
            "SELECT CAST(sort_album_artist_name AS TEXT) FROM media_file"
        ),
        "saa"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(sort_album_name AS TEXT) FROM media_file"
        ),
        "sal"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(sort_artist_name AS TEXT) FROM media_file"
        ),
        "sar"
    );
    assert_eq!(
        text(&store, "SELECT CAST(sort_title AS TEXT) FROM media_file"),
        "sti"
    );
    assert_eq!(
        text(&store, "SELECT CAST(suffix AS TEXT) FROM media_file"),
        ".mp3"
    );
    assert_eq!(
        text(&store, "SELECT CAST(title AS TEXT) FROM media_file"),
        "Something"
    );
    assert_eq!(number(&store, "SELECT track_number FROM media_file"), 17);
    assert_eq!(number(&store, "SELECT year FROM media_file"), 1969);
    assert_eq!(
        text(&store, "SELECT CAST(updated_at AS TEXT) FROM media_file"),
        "2024-01-01 00:00:00+00:00"
    );
    // --- PostMapArgs columns, consistent with the record's typed data ---
    // append_subtitle=false: full_title is the bare title (config default); it
    // is the FIRST word of full_text, so the stored column pins the behavior.
    let fts = nd_db::sql_feeders::mediafile_fts_columns(
        &rec.title,
        &rec.album,
        &rec.artist,
        &rec.album_artist,
        &rec.sort_title,
        &rec.sort_album_name,
        &rec.sort_artist_name,
        &rec.sort_album_artist_name,
        &rec.disc_subtitle,
        &nd_db::album_repo::participant_names(&rec.participant_names),
    );
    assert_eq!(
        text(&store, "SELECT CAST(full_text AS TEXT) FROM media_file"),
        fts.full_text,
        "the stored full_text must be the append_subtitle=false feed"
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(search_participants AS TEXT) FROM media_file"
        ),
        fts.search_participants
    );
    assert_eq!(
        text(
            &store,
            "SELECT CAST(search_normalized AS TEXT) FROM media_file"
        ),
        fts.search_normalized
    );
    assert!(
        !text(&store, "SELECT CAST(full_text AS TEXT) FROM media_file")
            .trim()
            .is_empty()
    );
    // tags JSON carries the tag; participants JSON carries the name + sub_role.
    assert!(text(&store, "SELECT CAST(tags AS TEXT) FROM media_file").contains(r#""Rock""#));
    assert!(
        text(&store, "SELECT CAST(participants AS TEXT) FROM media_file").contains(r#""Vocals""#)
    );
    // Execute the junction maintenance (Go's Put runs it in the same
    // transaction); the junction rows incl. sub_role land, joined to the
    // seeded artist. Fresh insert: the deletes match 0 rows, the inserts place
    // the single seeded id each.
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
        number(
            &store,
            "SELECT count(*) FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        1i64
    );
    assert_eq!(
        text(
            &store,
            "SELECT role FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        "artist"
    );
    assert_eq!(
        text(
            &store,
            "SELECT sub_role FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        "Vocals"
    );
    assert_eq!(number(&store, "SELECT count(*) FROM media_file_artists ma JOIN artist a ON a.id = ma.artist_id WHERE ma.media_file_id = 'mf-1' AND a.name = 'Song Artist'"), 1i64);
    assert_eq!(
        number(
            &store,
            "SELECT count(*) FROM media_file_tags WHERE media_file_id = 'mf-1'"
        ),
        1i64
    );
    assert_eq!(
        text(
            &store,
            "SELECT tag_id FROM media_file_tags WHERE media_file_id = 'mf-1'"
        ),
        genre_id
    );
    // --- full_title subtitle behavior pinned via the helper (append on) ---
    assert_eq!(mediafile_repo::full_title(&rec, false), "Something");
    assert_eq!(mediafile_repo::full_title(&rec, true), "Something (Live)");
    // --- UPDATE leg: birth_time and created_at are never SET; a later
    // update with empty participants is delete-with-no-insert; a
    // camelCase-restricted update touches only probe_data ---
    assert!(!put.update.0.contains("birth_time = ?"));
    assert!(!put.update.0.contains("created_at = ?"));
    let mut rec2 = rec.clone();
    rec2.participants.clear();
    let up = mediafile_repo::put_sql("mf-1", &rec2, stamp(), false, &[]).expect("renders");
    assert_eq!(insert(&store, &up.update), 1);
    assert!(
        up.participants_insert.is_none(),
        "no participants -> no insert"
    );
    assert_eq!(insert(&store, &up.participants_delete), 1);
    assert_eq!(
        number(&store, "SELECT count(*) FROM media_file_artists"),
        0i64
    );
    assert_eq!(
        text(&store, "SELECT CAST(birth_time AS TEXT) FROM media_file"),
        "2024-01-01 00:00:00+00:00",
        "birth_time survives the update"
    );
    assert_eq!(
        text(&store, "SELECT CAST(created_at AS TEXT) FROM media_file"),
        "2024-01-01 00:00:00+00:00",
        "created_at survives the update"
    );
    let restricted =
        mediafile_repo::put_sql("mf-1", &rec, stamp(), false, &["ProbeData"]).expect("renders");
    assert_eq!(
        restricted.update.0, "UPDATE media_file SET probe_data = ? WHERE id = ?",
        "the camelCase-restricted update must touch only probe_data"
    );
    assert_eq!(insert(&store, &restricted.update), 1);
    drop(store);
    std::fs::remove_file(path).ok();
}

/// `playlist_repo`'s insert leg used MySQL's `INSERT … SET`, which SQLite
/// refuses to parse. It is now a column list + VALUES tuple with `id` appended,
/// matching Go's `put` (`sql_base_repository.go:637-641`).
#[test]
fn playlist_insert_is_sqlite_syntax_and_stores_each_value() {
    let (store, path) = store();
    seed_user(&store);
    // Non-smart record: PostMapArgs deletes `rules`, so that column is absent
    // from the INSERT — exactly the value that previously rotated into the
    // `created_at` position when a caller supplied a rules column.
    let record = PlaylistRecord {
        // Read-path identity (never in playlist_row): the row's own id.
        id: "pl-1".to_string(),
        name: "Mix".to_string(),
        comment: String::new(),
        duration: 0.0,
        size: 0,
        song_count: 0,
        owner_id: "u-1".to_string(),
        public: false,
        path: String::new(),
        sync: false,
        uploaded_image: String::new(),
        external_image_url: String::new(),
        created_at: Some(stamp()),
        updated_at: stamp(),
        imported_hash: String::new(),
        rules: None,
        evaluated_at: None,
        // Hydration-owned derived fields: absent from playlist_row.
        annotation: nd_model::annotation::Annotation::default(),
        // Read-path-only (`structs:"-"`): absent from playlist_row.
        owner_name: "jdoe".to_string(),
    };
    let insert_stmt = playlist_repo::put_sql("pl-1", &record, stamp(), &[])
        .expect("renders")
        .insert;

    assert!(
        !insert_stmt.0.contains(" SET "),
        "SQLite has no INSERT … SET: {}",
        insert_stmt.0
    );
    assert_eq!(insert(&store, &insert_stmt), 1);

    assert_eq!(text(&store, "SELECT id FROM playlist"), "pl-1");
    assert_eq!(text(&store, "SELECT name FROM playlist"), "Mix");
    assert_eq!(
        text(&store, "SELECT owner_id FROM playlist"),
        "u-1",
        "owner_id must not be rotated into id or name"
    );
    assert_eq!(
        text(&store, "SELECT created_at FROM playlist"),
        "2024-01-01 00:00:00+00:00"
    );
    assert_eq!(
        text(&store, "SELECT CAST(rules IS NULL AS TEXT) FROM playlist"),
        "1",
        "non-smart put deletes rules: the column is not written"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

/// Non-vacuity: the pre-fix argument order must produce a different, visibly
/// wrong row. This pins that the tests above are sensitive to the order rather
/// than passing for any ordering.
#[test]
fn binding_id_first_would_corrupt_the_row_the_tests_detect() {
    let (store, path) = store();
    seed_library(&store);
    // Exactly what the buggy renderer emitted: columns end with id, args start
    // with it. SQLite accepts it and mis-stores everything.
    let corrupted = (
        "INSERT INTO album (name, album_artist, id) VALUES (?, ?, ?)".to_string(),
        vec![
            Value::from("al-1"),
            Value::from("Abbey Road"),
            Value::from("The Beatles"),
        ],
    );
    assert_eq!(insert(&store, &corrupted), 1);

    // The corruption the fixed renderer prevents: id holds the artist name.
    assert_eq!(text(&store, "SELECT id FROM album"), "The Beatles");
    assert_eq!(text(&store, "SELECT CAST(name AS TEXT) FROM album"), "al-1");
    drop(store);
    std::fs::remove_file(path).ok();
}
