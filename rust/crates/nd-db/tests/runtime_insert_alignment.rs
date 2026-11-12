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
#[test]
fn album_insert_stores_each_value_in_its_own_column() {
    let (store, path) = store();
    seed_library(&store);
    let columns = ["name".to_string(), "album_artist".to_string()];
    let put = album_repo::put_sql(
        "al-1",
        &columns,
        vec![Value::from("Abbey Road"), Value::from("The Beatles")],
        "{}",
        vec![],
    );

    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(
        text(&store, "SELECT id FROM album"),
        "al-1",
        "id must land in id, not in a set column"
    );
    assert_eq!(text(&store, "SELECT name FROM album"), "Abbey Road");
    assert_eq!(
        text(&store, "SELECT album_artist FROM album"),
        "The Beatles"
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

#[test]
fn mediafile_insert_stores_each_value_in_its_own_column() {
    let (store, path) = store();
    seed_library(&store);
    let columns = [
        "title".to_string(),
        "album".to_string(),
        "full_text".to_string(),
    ];
    let put = mediafile_repo::put_sql(
        "mf-1",
        &columns,
        vec![
            Value::from("Song"),
            Value::from("Abbey Road"),
            Value::from(" abbey road song"),
        ],
        "{}",
        vec![],
    );

    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(text(&store, "SELECT id FROM media_file"), "mf-1");
    assert_eq!(text(&store, "SELECT title FROM media_file"), "Song");
    assert_eq!(text(&store, "SELECT album FROM media_file"), "Abbey Road");
    assert_eq!(
        text(&store, "SELECT full_text FROM media_file"),
        " abbey road song",
        "the FTS feeder must land in full_text, not in id or title"
    );
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
    assert_eq!(text(&store, "SELECT name FROM album"), "al-1");
    drop(store);
    std::fs::remove_file(path).ok();
}
