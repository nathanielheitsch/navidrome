//! S-D2a shared marshal: the sorted column/value unit + PostMapArgs,
//! EXECUTED against a real SQLite DB.
//!
//! Pinned the way `runtime_insert_alignment.rs` pins the pre-S-D2a bug class
//! — by running the statements and reading the stored row + junction tables
//! back, because renderer string comparisons cannot catch a field swap or an
//! order rotation. Every base column carries a **distinct value no other
//! column could plausibly hold**, so a rotated/rotated-adjacent insert or
//! update fails the read-back assertions.
//!
//! Oracles: the PostMapArgs transformations compose the landed,
//! independently Go-pinned primitives (cited in `src/marshal.rs`):
//! `sql_participations` + `tests/participants_json.rs`/`go_participants`,
//! `sql_feeders` + `tests/search_feeders.rs`/`go_feeders`, `sql_json` +
//! `tests/sql_json.rs`/`go_sql_json`, `sql_tags` (its unit tests pin the Go
//! `marshalTags` shape). No new oracle here: this file pins the *composition*
//! behaviour (ordering, filtering, overwrites) and the executed data flow.
//!
//! Harness shape is `tests/runtime_insert_alignment.rs`: one isolated on-disk
//! temp DB per test, `apply_fresh_schema` through `nd_dbschema`, `Pool` so
//! `SEEDEDRAND`/`NATSORT` are registered per connection, `_foreign_keys=on`
//! (Go's `db/db.go:55`).
//!
//! **S-D2a boundary:** the base `ColumnMap` is caller-supplied (the
//! `structs.Map(entity)` half is not yet a Rust record); S-D2b replaces it
//! with complete repo-local records. These tests intentionally exercise that
//! boundary — a caller-supplied base map with distinct values — and do NOT
//! claim caller-supplied columns are eliminated.

use std::collections::BTreeMap;

use nd_db::marshal::{
    album_post_map_args, folder_post_map_args, mediafile_post_map_args, AlbumDerived, ColumnMap,
    FolderDerived, MarshalError, MarshalledRow, MediaFileDerived,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags as sqltags;
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
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-marshal-", ".db"));
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
/// Execute a maintenance leg that is absent for empty participants/tags
/// (Go returns before building the insert — delete still ran).
fn exec_maybe(store: &SQLStore, statement: &Option<(String, Vec<Value>)>) -> Option<usize> {
    statement.as_ref().map(|s| insert(store, s))
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

/// `album`/`folder`/`media_file` reference `library` (and default
/// `library_id = 1`), so the row must exist before any insert with
/// `_foreign_keys=on`.
fn seed_library(store: &SQLStore) {
    insert(
        store,
        &(
            "INSERT INTO library (id, name, path) VALUES (?, ?, ?)".to_string(),
            vec![Value::from(1), Value::from("Main"), Value::from("/music")],
        ),
    );
}

/// `album_artists.artist_id` / `media_file_artists.artist_id` reference
/// `artist`: the participant junction INSERT rows need a real artist.
fn seed_artist(store: &SQLStore, id: &str, name: &str) {
    insert(
        store,
        &(
            "INSERT INTO artist (id, name, order_artist_name) VALUES (?, ?, ?)".to_string(),
            vec![Value::from(id), Value::from(name), Value::from(name)],
        ),
    );
}

/// `album_tags.tag_id` / `media_file_tags.tag_id` reference the `tag`
/// dictionary: the tag junction INSERT only creates rows for ids the
/// dictionary has (`sql_tags::insert_tags_sql`'s JOIN).
fn seed_tag(store: &SQLStore, tag_id: &str, name: &str, value: &str) {
    insert(
        store,
        &(
            "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?)".to_string(),
            vec![Value::from(tag_id), Value::from(name), Value::from(value)],
        ),
    );
}

// ---------------------------------------------------------------------------
// Core unit invariants, executed through the repo adapters where possible.
// ---------------------------------------------------------------------------

/// Scrambled base + derived keys must emerge sorted, values positional,
/// `id` appended last — and the executed row must agree (a rotation fails).
#[test]
fn core_scrambled_keys_emerge_sorted_with_positional_values() {
    let (store, path) = store();
    seed_library(&store);

    // Base deliberately built in scrambled order, every value distinct.
    let mut base = BTreeMap::new();
    base.insert("name".to_string(), Value::Text("Zulu Name".into()));
    base.insert(
        "created_at".to_string(),
        Value::Text("2024-05-05 05:05:05+00:00".into()),
    );
    base.insert(
        "sort_album_name".to_string(),
        Value::Text("Srt Zulu".into()),
    );
    base.insert(
        "album_artist".to_string(),
        Value::Text("Zulu Artist".into()),
    );
    base.insert("max_year".to_string(), Value::Integer(1987));

    let derived = AlbumDerived {
        name: "Zulu Name".into(),
        sort_album_name: "Srt Zulu".into(),
        album_artist: "Zulu Artist".into(),
        participant_names: vec!["Frahm Nils".into()],
        disc_values: vec!["Bonus Disc".into()],
        album_version: vec!["Remastered".into()],
        catalog_number: vec!["ND-1234".into()],
        tags: sqltags::DbTags::new(),
        participants: Participants::new(),
        folder_ids: Some(vec!["folder-a".into()]),
        discs: Some({
            let mut m = BTreeMap::new();
            m.insert(10, "Ten".into());
            m.insert(1, "One".into());
            m
        }),
    };
    let (map, _maint) = album_post_map_args(ColumnMap::new(base), derived);
    let row = MarshalledRow::new("album", "alb-scrambled", &map, &[]).unwrap();

    // The derived keys join the sort; `id` stays last, never sorted in.
    let cols = row.insert_columns.clone();
    let sorted = {
        let mut c: Vec<String> = cols.clone();
        c.pop();
        c.sort();
        c
    };
    assert_eq!(cols[..cols.len() - 1], sorted, "columns must be sorted");
    assert_eq!(cols.last().map(String::as_str), Some("id"));

    let put = nd_db::album_repo::put_marshalled_sql("alb-scrambled", &map, &_maint, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);

    // Read back: each distinct value in its own column. A value rotation
    // (e.g. id-first args) fails one of these.
    assert_eq!(row_text(&store, "SELECT id FROM album"), "alb-scrambled");
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Zulu Name");
    assert_eq!(
        row_text(&store, "SELECT sort_album_name FROM album"),
        "Srt Zulu"
    );
    assert_eq!(
        row_text(&store, "SELECT album_artist FROM album"),
        "Zulu Artist"
    );
    assert_eq!(
        row_text(&store, "SELECT CAST(max_year AS TEXT) FROM album"),
        "1987"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-05-05 05:05:05+00:00"
    );
    // Derived: the JSON columns stored exactly what the primitives produced.
    assert_eq!(
        row_text(&store, "SELECT discs FROM album"),
        r#"{"1":"One","10":"Ten"}"#,
        "Go's string-key sort: 1, 10"
    );
    assert_eq!(
        row_text(&store, "SELECT folder_ids FROM album"),
        r#"["folder-a"]"#
    );
    // full_text came from the feeder, not a base key. SanitizeStrings
    // lowercases, transliterates, and SORTS the words — so assert the words,
    // not a phrase.
    let full_text = row_text(&store, "SELECT full_text FROM album");
    assert!(full_text.starts_with(' '));
    assert!(full_text.contains("zulu"), "full_text: {full_text}");
    assert!(full_text.contains("name"), "full_text: {full_text}");
    assert!(full_text.contains("remastered"), "full_text: {full_text}");
    assert!(full_text.contains("bonus"), "full_text: {full_text}");
    drop(store);
    std::fs::remove_file(path).ok();
}

/// `id` in the base/final map must be a typed error, not a silent double-bind.
#[test]
fn core_id_in_map_is_a_typed_error() {
    let mut base = BTreeMap::new();
    base.insert("id".to_string(), Value::Text("sneaky".into()));
    base.insert("name".to_string(), Value::Text("n".into()));
    let err = MarshalledRow::new("album", "a-1", &ColumnMap::new(base), &[]).unwrap_err();
    assert_eq!(err, MarshalError::IdInMap);

    // The repo adapters surface the same error through the public seam.
    let (map, _m) = album_post_map_args(
        ColumnMap::new({
            let mut m = BTreeMap::new();
            m.insert("name".to_string(), Value::Text("n".into()));
            m
        }),
        bare_album_derived(),
    );
    let with_id = ColumnMap::new({
        let mut m = map.map().clone();
        m.insert("id".to_string(), Value::Text("sneaky".into()));
        m
    });
    let err = nd_db::album_repo::put_marshalled_sql("a-1", &with_id, &_m, &[]).unwrap_err();
    assert_eq!(err, MarshalError::IdInMap);
}

fn bare_album_derived() -> AlbumDerived {
    AlbumDerived {
        name: "n".into(),
        sort_album_name: "s".into(),
        album_artist: "aa".into(),
        participant_names: vec![],
        disc_values: vec![],
        album_version: vec![],
        catalog_number: vec![],
        tags: sqltags::DbTags::new(),
        participants: Participants::new(),
        folder_ids: None,
        discs: None,
    }
}

/// An update set left empty after the `created_at`/`birth_time` drop is an
/// error — never `UPDATE … SET WHERE id = ?` invalid SQL.
#[test]
fn core_empty_update_set_errors() {
    let mut base = BTreeMap::new();
    base.insert("created_at".to_string(), Value::Text("t0".into()));
    base.insert("birth_time".to_string(), Value::Text("t0".into()));

    // Direct core:
    let direct = ColumnMap::new(base.clone());
    let err = MarshalledRow::new("media_file", "mf-1", &direct, &[]).unwrap_err();
    assert_eq!(err, MarshalError::EmptyUpdateSet);

    // Through the mediafile adapter:
    let (map, _m) = mediafile_post_map_args(
        ColumnMap::new(base),
        MediaFileDerived {
            full_title: "t".into(),
            album: "a".into(),
            artist: "b".into(),
            album_artist: "c".into(),
            sort_title: "".into(),
            sort_album_name: "".into(),
            sort_artist_name: "".into(),
            sort_album_artist_name: "".into(),
            disc_subtitle: "".into(),
            participant_names: vec![],
            tags: sqltags::DbTags::new(),
            participants: Participants::new(),
        },
    );
    // PostMapArgs always adds derived keys, so the unrestricted adapter
    // update never comes up empty; the restriction to a dropped column is
    // the reachable error path.
    let err =
        nd_db::mediafile_repo::put_marshalled_sql("mf-1", &map, &_m, &["created_at"]).unwrap_err();
    assert_eq!(err, MarshalError::EmptyUpdateSet);
}

/// `created_at`/`birth_time` are in the INSERT set and absent from the UPDATE
/// set; an optional `cols_to_update` restricts the update only.
#[test]
fn core_update_drops_created_at_birth_time_and_cols_restrict_update_only() {
    let mut base = BTreeMap::new();
    base.insert("created_at".to_string(), Value::Text("t0".into()));
    base.insert("birth_time".to_string(), Value::Text("t0".into()));
    base.insert("title".to_string(), Value::Text("T".into()));
    base.insert("album".to_string(), Value::Text("A".into()));

    let (map, m) = mediafile_post_map_args(
        ColumnMap::new(base.clone()),
        MediaFileDerived {
            full_title: "T".into(),
            album: "A".into(),
            artist: "B".into(),
            album_artist: "C".into(),
            sort_title: "".into(),
            sort_album_name: "".into(),
            sort_artist_name: "".into(),
            sort_album_artist_name: "".into(),
            disc_subtitle: "".into(),
            participant_names: vec![],
            tags: sqltags::DbTags::new(),
            participants: Participants::new(),
        },
    );
    let full = MarshalledRow::new("media_file", "mf-1", &map, &[]).unwrap();
    assert!(full.insert_columns.contains(&"created_at".into()));
    assert!(full.insert_columns.contains(&"birth_time".into()));
    assert!(!full.update_columns.contains(&"created_at".into()));
    assert!(!full.update_columns.contains(&"birth_time".into()));

    // cols_to_update restricts the UPDATE; the INSERT keeps everything.
    let restricted = MarshalledRow::new("media_file", "mf-1", &map, &["title"]).unwrap();
    assert_eq!(restricted.update_columns, vec!["title".to_string()]);
    assert_eq!(restricted.insert_columns.len(), full.insert_columns.len());
    assert_eq!(restricted.insert_columns, full.insert_columns);

    // A restriction that leaves only the dropped columns errors.
    let err = MarshalledRow::new("media_file", "mf-1", &map, &["created_at"]).unwrap_err();
    assert_eq!(err, MarshalError::EmptyUpdateSet);

    // The adapter passes the restriction through.
    let put = nd_db::mediafile_repo::put_marshalled_sql("mf-1", &map, &m, &["title"]).unwrap();
    assert_eq!(put.update.0, "UPDATE media_file SET title = ? WHERE id = ?");
    let _ = base;
}

/// `Value` types and nullable timestamps survive the round trip: `Null`
/// binds SQL NULL, `Real` binds a float, and the stored typeof agrees.
#[test]
fn core_value_types_and_null_timestamps_survive() {
    let (store, path) = store();
    seed_library(&store);

    let mut base = BTreeMap::new();
    base.insert("name".to_string(), Value::Text("NullProbe".into()));
    base.insert("rg_album_gain".to_string(), Value::Real(1.25));
    base.insert("rg_album_peak".to_string(), Value::Null); // nullable *float64
    base.insert("created_at".to_string(), Value::Null); // Go: nil *time.Time

    let (map, m) = album_post_map_args(ColumnMap::new(base), bare_album_derived());
    let put = nd_db::album_repo::put_marshalled_sql("alb-null", &map, &m, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(
        row_text(&store, "SELECT typeof(rg_album_peak) FROM album"),
        "null"
    );
    assert_eq!(
        row_text(&store, "SELECT typeof(created_at) FROM album"),
        "null"
    );
    assert_eq!(
        row_text(&store, "SELECT CAST(rg_album_gain AS TEXT) FROM album"),
        "1.25"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Album write→read.
// ---------------------------------------------------------------------------

/// Full album write→read through the S-D2 adapter: INSERT, read every column
/// and both junction tables; then an UPDATE that changes `name` (and
/// participants/tags) while `created_at` stays untouched, and the derived
/// search columns + stale junction rows are rewritten (delete-then-insert,
/// Go order).
#[test]
fn album_write_read_and_update_keeps_created_at() {
    let (store, path) = store();
    seed_library(&store);
    seed_artist(&store, "ar-1", "Beatle J");
    seed_artist(&store, "ar-2", "Frahm Nils");
    let genre_id = sqltags::indexed_tag_ids(sqltags::DbTags::from([(
        "genre".to_string(),
        vec!["Prog".into()],
    )]))[0]
        .clone();
    seed_tag(&store, &genre_id, "genre", "Prog");

    // Distinct value per column: a swap or rotation fails the read-back.
    let mut base = BTreeMap::new();
    base.insert("name".to_string(), Value::Text("Zuma".into()));
    base.insert(
        "sort_album_name".to_string(),
        Value::Text("Zuma Sort".into()),
    );
    base.insert(
        "album_artist".to_string(),
        Value::Text("King Crimson".into()),
    );
    base.insert(
        "created_at".to_string(),
        Value::Text("2024-03-01 12:00:00+00:00".into()),
    );
    base.insert(
        "updated_at".to_string(),
        Value::Text("2024-03-02 13:00:00+00:00".into()),
    );
    base.insert("max_year".to_string(), Value::Integer(1969));
    base.insert(
        "imported_at".to_string(),
        Value::Text("2024-03-03 14:00:00+00:00".into()),
    );

    let participants = Participants::from([(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-1".into(),
            name: "Beatle J".into(),
            sub_role: "".into(),
        }],
    )]);
    let tags = sqltags::DbTags::from([
        ("genre".to_string(), vec!["Prog".into()]),
        ("albumversion".to_string(), vec!["2nd Edition".into()]),
    ]);

    let derived = AlbumDerived {
        name: "Zuma".into(),
        sort_album_name: "Zuma Sort".into(),
        album_artist: "King Crimson".into(),
        participant_names: vec!["Beatle J".into()],
        disc_values: vec!["Disc Two".into()],
        album_version: vec!["2nd Edition".into()],
        catalog_number: vec!["213535".into()],
        tags,
        participants,
        folder_ids: Some(vec!["fld-1".into(), "fld-2".into()]),
        discs: Some({
            let mut m = BTreeMap::new();
            m.insert(1, "Original".into());
            m
        }),
    };
    let (map, maint) = album_post_map_args(ColumnMap::new(base), derived);

    // INSERT leg.
    let put = nd_db::album_repo::put_marshalled_sql("alb-zuma", &map, &maint, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);
    // Go's `Put` runs the participant/tag maintenance in the same tx after
    // the row put (album_repository.go:164-172) — execute the legs.
    insert(&store, &put.participants_delete);
    // The album has participants → the insert leg is present (built by
    // sql_participations::update_participants_sql from the same
    // Participants value the JSON column/FTS used).
    assert_eq!(exec_maybe(&store, &put.participants_insert), Some(1));
    insert(&store, &put.tags_delete);
    assert!(put.tags_insert.is_some());
    exec_maybe(&store, &put.tags_insert);

    // Distinct values land in their own columns.
    assert_eq!(row_text(&store, "SELECT id FROM album"), "alb-zuma");
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Zuma");
    assert_eq!(
        row_text(&store, "SELECT sort_album_name FROM album"),
        "Zuma Sort"
    );
    assert_eq!(
        row_text(&store, "SELECT album_artist FROM album"),
        "King Crimson"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-03-01 12:00:00+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT imported_at FROM album"),
        "2024-03-03 14:00:00+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT discs FROM album"),
        r#"{"1":"Original"}"#
    );
    assert_eq!(
        row_text(&store, "SELECT folder_ids FROM album"),
        r#"["fld-1","fld-2"]"#
    );
    let tags_col = row_text(&store, "SELECT tags FROM album");
    assert!(
        tags_col.contains("\"genre\"") && tags_col.contains("Prog"),
        "tags column: {tags_col}"
    );
    let participants_col = row_text(&store, "SELECT participants FROM album");
    assert!(
        participants_col.contains("\"albumartist\"") && participants_col.contains("ar-1"),
        "participants column: {participants_col}"
    );
    let full_text = row_text(&store, "SELECT full_text FROM album");
    assert!(full_text.starts_with(' ') && full_text.contains("zuma"));
    assert!(full_text.contains("edition"), "full_text: {full_text}");
    assert!(full_text.contains("213535"), "full_text: {full_text}");
    assert_eq!(
        row_text(&store, "SELECT search_participants FROM album"),
        "Beatle J"
    );
    // search_normalized: NormalizeForFTS(Name, AlbumArtist) — plain ASCII
    // words contribute no transliteration variants, so Go stores ""
    // (the FTS column is still written, just empty).
    assert_eq!(row_text(&store, "SELECT search_normalized FROM album"), "");

    // Junction tables (FKs: the seeded artist + tag dictionary row).
    assert_eq!(
        row_text(
            &store,
            "SELECT artist_id FROM album_artists WHERE album_id = 'alb-zuma'"
        ),
        "ar-1"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT role FROM album_artists WHERE album_id = 'alb-zuma'"
        ),
        "albumartist"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT tag_id FROM album_tags WHERE album_id = 'alb-zuma'"
        ),
        genre_id
    );

    // FTS trigger: the album_fts row exists for the inserted row.
    assert_eq!(row_i64(&store, "SELECT count(*) FROM album_fts"), 1);

    // --- UPDATE: name changes, created_at must NOT. ---
    let mut base2 = BTreeMap::new();
    base2.insert("name".to_string(), Value::Text("Zuma Remaster".into()));
    base2.insert(
        "sort_album_name".to_string(),
        Value::Text("Zuma Sort".into()),
    );
    base2.insert(
        "album_artist".to_string(),
        Value::Text("King Crimson".into()),
    );
    // A deliberately different created_at: the update must drop it, so the
    // stored value must be the ORIGINAL, not this one.
    base2.insert(
        "created_at".to_string(),
        Value::Text("2099-01-01 00:00:00+00:00".into()),
    );
    base2.insert(
        "updated_at".to_string(),
        Value::Text("2024-04-01 00:00:00+00:00".into()),
    );

    let participants2 = Participants::from([(
        Role::from("composer"),
        vec![Participant {
            id: "ar-2".into(),
            name: "Frahm Nils".into(),
            sub_role: "".into(),
        }],
    )]);
    let derived2 = AlbumDerived {
        name: "Zuma Remaster".into(),
        sort_album_name: "Zuma Sort".into(),
        album_artist: "King Crimson".into(),
        participant_names: vec!["Frahm Nils".into()],
        disc_values: vec![],
        album_version: vec![],
        catalog_number: vec![],
        tags: sqltags::DbTags::new(),
        participants: participants2,
        folder_ids: None,
        discs: None,
    };
    let (map2, maint2) = album_post_map_args(ColumnMap::new(base2), derived2);
    let up = nd_db::album_repo::put_marshalled_sql("alb-zuma", &map2, &maint2, &[]).unwrap();

    // The rendered legs are distinct: no created_at in the SET list.
    assert!(
        !up.update.0.contains("created_at = ?"),
        "created_at must not be updated: {}",
        up.update.0
    );
    assert_eq!(
        insert(&store, &up.update),
        1,
        "the update must match the row"
    );

    // created_at untouched; updated_at and name changed.
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-03-01 12:00:00+00:00",
        "created_at must survive the update (selectUpdateColumns drops it)"
    );
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Zuma Remaster");
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM album"),
        "2024-04-01 00:00:00+00:00"
    );
    // Derived search columns recomputed.
    assert_eq!(
        row_text(&store, "SELECT search_participants FROM album"),
        "Frahm Nils"
    );
    assert!(
        row_text(&store, "SELECT full_text FROM album").contains("remaster")
            && row_text(&store, "SELECT full_text FROM album").contains("zuma"),
        "full_text must carry the renamed album"
    );

    // Stale participant/tag relationships: delete-then-insert (Go order).
    insert(&store, &up.participants_delete);
    // The update's participants (composer) are non-empty → insert present.
    assert_eq!(exec_maybe(&store, &up.participants_insert), Some(1));
    assert_eq!(
        row_text(
            &store,
            "SELECT role FROM album_artists WHERE album_id = 'alb-zuma'"
        ),
        "composer",
        "the stale albumartist row must be gone and the new composer row present"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM album_artists WHERE album_id = 'alb-zuma'"
        ),
        1
    );
    insert(&store, &up.tags_delete);
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM album_tags WHERE album_id = 'alb-zuma'"
        ),
        0
    );
    assert!(
        maint2.indexed_tag_ids.is_empty(),
        "no indexed tags on update"
    );

    drop(store);
    std::fs::remove_file(path).ok();
}

/// Executed restricted update: camelCase `colsToUpdate` (`updatedAt`,
/// `MaxYear`) is Go-`toSnakeCase`-normalized, and a real UPDATE changes
/// **only** the two selected columns with changed values. The positional SET
/// binding is proven by the read-back: a rotation would put the new timestamp
/// into `max_year` (an INTEGER column, which SQLite's type affinity would not
/// silently accept as an int read) and the new int into `updated_at`. The
/// unselected row values (name/imported_at/created_at) survive, and the
/// INSERT set stayed full.
#[test]
fn album_restricted_update_changes_only_selected_columns() {
    let (store, path) = store();
    seed_library(&store);
    seed_artist(&store, "ar-1", "Beatle J");

    let base: BTreeMap<String, Value> = [
        ("name".to_string(), Value::Text("Zuma".into())),
        (
            "sort_album_name".to_string(),
            Value::Text("Zuma Sort".into()),
        ),
        (
            "album_artist".to_string(),
            Value::Text("King Crimson".into()),
        ),
        (
            "created_at".to_string(),
            Value::Text("2024-03-01 12:00:00+00:00".into()),
        ),
        (
            "updated_at".to_string(),
            Value::Text("2024-03-02 13:00:00+00:00".into()),
        ),
        ("max_year".to_string(), Value::Integer(1969)),
        (
            "imported_at".to_string(),
            Value::Text("2024-03-03 14:00:00+00:00".into()),
        ),
    ]
    .into_iter()
    .collect();
    let derived = AlbumDerived {
        name: "Zuma".into(),
        sort_album_name: "Zuma Sort".into(),
        album_artist: "King Crimson".into(),
        participant_names: vec![],
        disc_values: vec![],
        album_version: vec![],
        catalog_number: vec![],
        tags: sqltags::DbTags::new(),
        participants: Participants::new(),
        folder_ids: None,
        discs: None,
    };
    let (map, maint) = album_post_map_args(ColumnMap::new(base.clone()), derived);
    // INSERT set is full (all base columns + derived, sorted, id last).
    let put = nd_db::album_repo::put_marshalled_sql("alb-re", &map, &maint, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);
    let insert_cols: Vec<&str> = put.insert.0.split(',').map(str::trim).collect();
    for c in [
        "name",
        "updated_at",
        "max_year",
        "imported_at",
        "created_at",
    ] {
        assert!(
            insert_cols.contains(&c),
            "INSERT must be full: {}",
            put.insert.0
        );
    }

    // A *new* map with the two candidate fields changed; restrict the UPDATE
    // to them via camelCase names.
    let mut base2 = base.clone();
    base2.insert(
        "updated_at".to_string(),
        Value::Text("2024-05-01 08:00:00+00:00".into()),
    );
    base2.insert("max_year".to_string(), Value::Integer(2024));
    let derived2 = AlbumDerived {
        name: "Zuma".into(),
        sort_album_name: "Zuma Sort".into(),
        album_artist: "King Crimson".into(),
        participant_names: vec![],
        disc_values: vec![],
        album_version: vec![],
        catalog_number: vec![],
        tags: sqltags::DbTags::new(),
        participants: Participants::new(),
        folder_ids: None,
        discs: None,
    };
    let (map2, maint2) = album_post_map_args(ColumnMap::new(base2), derived2);
    let up =
        nd_db::album_repo::put_marshalled_sql("alb-re", &map2, &maint2, &["updatedAt", "MaxYear"])
            .unwrap();
    // Only the two selected (normalized) columns are in the SET, sorted —
    // the camelCase input proves the toSnakeCase normalization end to end.
    assert_eq!(
        up.update.0,
        "UPDATE album SET max_year = ?, updated_at = ? WHERE id = ?",
    );
    assert_eq!(
        insert(&store, &up.update),
        1,
        "restricted update must match"
    );

    // Selected columns hold their NEW, correctly-bound values. A positional
    // rotation of the two SET args would bind "2024-05-01…" to max_year (int
    // column) and 2024 to updated_at — both assertions fail.
    assert_eq!(
        row_i64(&store, "SELECT max_year FROM album"),
        2024,
        "max_year must hold its own new value, not the timestamp"
    );
    assert_eq!(
        row_text(&store, "SELECT updated_at FROM album"),
        "2024-05-01 08:00:00+00:00",
        "updated_at must hold its own new value, not the int"
    );
    // Unselected columns are untouched by the restricted update.
    assert_eq!(row_text(&store, "SELECT name FROM album"), "Zuma");
    assert_eq!(
        row_text(&store, "SELECT created_at FROM album"),
        "2024-03-01 12:00:00+00:00",
    );
    assert_eq!(
        row_text(&store, "SELECT imported_at FROM album"),
        "2024-03-03 14:00:00+00:00",
    );

    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Mediafile write→read.
// ---------------------------------------------------------------------------

/// Mediafile write→read: the 9-field `full_text` + search fields + tags +
/// participants, then an update that proves `created_at` (and `birth_time`)
/// are untouched.
#[test]
fn mediafile_write_read_and_update_keeps_created_at() {
    let (store, path) = store();
    seed_library(&store);
    seed_artist(&store, "ar-9", "Sänger S");
    let genre_id = sqltags::indexed_tag_ids(sqltags::DbTags::from([(
        "genre".to_string(),
        vec!["Ambient".into()],
    )]))[0]
        .clone();
    seed_tag(&store, &genre_id, "genre", "Ambient");

    let mut base = BTreeMap::new();
    base.insert("title".to_string(), Value::Text("Stille".into()));
    base.insert("album".to_string(), Value::Text("Nebula".into()));
    base.insert("artist".to_string(), Value::Text("Sänger S".into()));
    base.insert(
        "album_artist".to_string(),
        Value::Text("Nebula Ensemble".into()),
    );
    base.insert("sort_title".to_string(), Value::Text("Stille Sort".into()));
    base.insert(
        "sort_album_name".to_string(),
        Value::Text("Nebula Sort".into()),
    );
    base.insert(
        "sort_artist_name".to_string(),
        Value::Text("Sänger Sort".into()),
    );
    base.insert(
        "sort_album_artist_name".to_string(),
        Value::Text("Nebula Ensemble Sort".into()),
    );
    base.insert(
        "disc_subtitle".to_string(),
        Value::Text("Second Disc".into()),
    );
    base.insert(
        "path".to_string(),
        Value::Text("/music/Nebula/01 Stille.flac".into()),
    );
    base.insert("library_id".to_string(), Value::Integer(1));
    base.insert(
        "created_at".to_string(),
        Value::Text("2024-06-01 09:00:00+00:00".into()),
    );
    base.insert(
        "birth_time".to_string(),
        Value::Text("2024-06-01 09:00:01+00:00".into()),
    );

    // full_title: config-resolved by the caller via the feeder (subtitle on).
    let full_title = nd_db::sql_feeders::full_title("Stille", "[Deluxe]", true);
    assert_eq!(full_title, "Stille [Deluxe]");
    let tags = sqltags::DbTags::from([
        ("genre".to_string(), vec!["Ambient".into()]),
        ("subtitle".to_string(), vec!["Deluxe".into()]),
    ]);
    let participants = Participants::from([(
        Role::from("artist"),
        vec![Participant {
            id: "ar-9".into(),
            name: "Sänger S".into(),
            sub_role: "vocals".into(),
        }],
    )]);
    let derived = MediaFileDerived {
        full_title,
        album: "Nebula".into(),
        artist: "Sänger S".into(),
        album_artist: "Nebula Ensemble".into(),
        sort_title: "Stille Sort".into(),
        sort_album_name: "Nebula Sort".into(),
        sort_artist_name: "Sänger Sort".into(),
        sort_album_artist_name: "Nebula Ensemble Sort".into(),
        disc_subtitle: "Second Disc".into(),
        participant_names: vec!["Sänger S".into()],
        tags,
        participants,
    };
    let (map, maint) = mediafile_post_map_args(ColumnMap::new(base), derived);
    let put = nd_db::mediafile_repo::put_marshalled_sql("mf-1", &map, &maint, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);

    // Distinct values in their own columns.
    assert_eq!(row_text(&store, "SELECT id FROM media_file"), "mf-1");
    assert_eq!(row_text(&store, "SELECT title FROM media_file"), "Stille");
    assert_eq!(row_text(&store, "SELECT album FROM media_file"), "Nebula");
    assert_eq!(
        row_text(&store, "SELECT artist FROM media_file"),
        "Sänger S"
    );
    assert_eq!(
        row_text(&store, "SELECT album_artist FROM media_file"),
        "Nebula Ensemble"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM media_file"),
        "2024-06-01 09:00:00+00:00"
    );
    assert_eq!(
        row_text(&store, "SELECT birth_time FROM media_file"),
        "2024-06-01 09:00:01+00:00"
    );
    // 9-field full_text (full_title + album + artist + album_artist + the 5
    // sort/subtitle fields + participant names), leading space, sorted.
    let full_text = row_text(&store, "SELECT full_text FROM media_file");
    assert!(full_text.starts_with(' '));
    for word in ["stille", "deluxe", "nebula", "sanger", "ensemble", "second"].iter() {
        assert!(
            full_text.contains(word),
            "full_text {full_text:?} must contain {word:?}"
        );
    }
    assert_eq!(
        row_text(&store, "SELECT search_participants FROM media_file"),
        "Sänger S"
    );
    let tags_col = row_text(&store, "SELECT tags FROM media_file");
    assert!(tags_col.contains("Ambient"), "tags column: {tags_col}");
    let participants_col = row_text(&store, "SELECT participants FROM media_file");
    assert!(
        participants_col.contains("\"artist\"") && participants_col.contains("vocals"),
        "participants column: {participants_col}"
    );

    // Junctions (Go order: execute the put's participants legs, then tags).
    insert(&store, &put.participants_delete);
    // Non-empty participants → insert present.
    assert_eq!(exec_maybe(&store, &put.participants_insert), Some(1));
    insert(&store, &put.tags_delete);
    assert!(put.tags_insert.is_some());
    exec_maybe(&store, &put.tags_insert);
    assert_eq!(
        row_text(
            &store,
            "SELECT artist_id FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        "ar-9"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT sub_role FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        "vocals"
    );
    assert_eq!(
        row_text(
            &store,
            "SELECT tag_id FROM media_file_tags WHERE media_file_id = 'mf-1'"
        ),
        genre_id
    );

    // FTS trigger fired on insert.
    assert_eq!(row_i64(&store, "SELECT count(*) FROM media_file_fts"), 1);

    // --- UPDATE: title changes; created_at/birth_time must NOT. ---
    let mut base2 = BTreeMap::new();
    base2.insert("title".to_string(), Value::Text("Stille II".into()));
    base2.insert("album".to_string(), Value::Text("Nebula".into()));
    base2.insert("artist".to_string(), Value::Text("Sänger S".into()));
    base2.insert(
        "album_artist".to_string(),
        Value::Text("Nebula Ensemble".into()),
    );
    base2.insert(
        "created_at".to_string(),
        Value::Text("2099-01-01 00:00:00+00:00".into()),
    );
    base2.insert(
        "birth_time".to_string(),
        Value::Text("2099-01-02 00:00:00+00:00".into()),
    );
    let derived2 = MediaFileDerived {
        full_title: "Stille II".into(),
        album: "Nebula".into(),
        artist: "Sänger S".into(),
        album_artist: "Nebula Ensemble".into(),
        sort_title: "Stille Sort".into(),
        sort_album_name: "Nebula Sort".into(),
        sort_artist_name: "Sänger Sort".into(),
        sort_album_artist_name: "Nebula Ensemble Sort".into(),
        disc_subtitle: "Second Disc".into(),
        participant_names: vec![],
        tags: sqltags::DbTags::new(),
        participants: Participants::new(),
    };
    let (map2, maint2) = mediafile_post_map_args(ColumnMap::new(base2), derived2);
    let up = nd_db::mediafile_repo::put_marshalled_sql("mf-1", &map2, &maint2, &[]).unwrap();
    assert!(!up.update.0.contains("created_at = ?"));
    assert!(!up.update.0.contains("birth_time = ?"));
    assert_eq!(insert(&store, &up.update), 1);

    assert_eq!(
        row_text(&store, "SELECT title FROM media_file"),
        "Stille II"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM media_file"),
        "2024-06-01 09:00:00+00:00",
        "created_at must survive the update"
    );
    assert_eq!(
        row_text(&store, "SELECT birth_time FROM media_file"),
        "2024-06-01 09:00:01+00:00",
        "birth_time must survive the update"
    );
    assert_eq!(
        row_text(&store, "SELECT search_participants FROM media_file"),
        ""
    );
    assert!(maint2.indexed_tag_ids.is_empty());
    // **Empty participants → delete + NO insert** (fix-3 pin). Go returns
    // before building the insert (`sql_participations.go:87-89`), so the
    // bundle's insert leg is `None`; the delete still runs and clears the
    // junction table the earlier (non-empty) put created.
    assert!(
        up.participants_insert.is_none(),
        "empty participants must not emit an insert (Go len(participants)==0)"
    );
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        1
    );
    insert(&store, &up.participants_delete);
    exec_maybe(&store, &up.participants_insert);
    assert_eq!(
        row_i64(
            &store,
            "SELECT count(*) FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        0,
        "the delete must clear the junction table when participants are empty"
    );
    insert(&store, &up.tags_delete);

    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Folder write→read.
// ---------------------------------------------------------------------------

/// Folder write→read: `None` image_files stores `[]` (never `null`); a
/// non-empty update stores the JSON; `created_at` untouched by the update.
#[test]
fn folder_write_read_nil_image_files_and_update() {
    let (store, path) = store();
    seed_library(&store);

    let mut base = BTreeMap::new();
    base.insert("name".to_string(), Value::Text("Rock".into()));
    base.insert("path".to_string(), Value::Text("/music/Rock".into()));
    base.insert("library_id".to_string(), Value::Integer(1));
    base.insert(
        "created_at".to_string(),
        Value::Text("2024-07-01 00:00:00+00:00".into()),
    );

    let map = folder_post_map_args(ColumnMap::new(base), FolderDerived { image_files: None });
    let put = nd_db::folder_repo::put_marshalled_sql("fld-rock", &map, &[]).unwrap();
    assert_eq!(insert(&store, &put.insert), 1);

    assert_eq!(row_text(&store, "SELECT id FROM folder"), "fld-rock");
    assert_eq!(row_text(&store, "SELECT name FROM folder"), "Rock");
    assert_eq!(row_text(&store, "SELECT path FROM folder"), "/music/Rock");
    assert_eq!(
        row_text(&store, "SELECT image_files FROM folder"),
        "[]",
        "nil ImageFiles must store the literal [] (folder_repository.go:48-50)"
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM folder"),
        "2024-07-01 00:00:00+00:00"
    );

    // Update: non-empty image_files stored as JSON; created_at untouched.
    let mut base2 = BTreeMap::new();
    base2.insert("name".to_string(), Value::Text("Rock".into()));
    base2.insert(
        "created_at".to_string(),
        Value::Text("2099-01-01 00:00:00+00:00".into()),
    );
    let map2 = folder_post_map_args(
        ColumnMap::new(base2),
        FolderDerived {
            image_files: Some(vec!["cover.jpg".into(), "back.jpg".into()]),
        },
    );
    let up = nd_db::folder_repo::put_marshalled_sql("fld-rock", &map2, &[]).unwrap();
    assert!(!up.update.0.contains("created_at = ?"));
    assert_eq!(insert(&store, &up.update), 1);

    assert_eq!(
        row_text(&store, "SELECT image_files FROM folder"),
        r#"["cover.jpg","back.jpg"]"#
    );
    assert_eq!(
        row_text(&store, "SELECT created_at FROM folder"),
        "2024-07-01 00:00:00+00:00",
        "created_at must survive the update"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}

// ---------------------------------------------------------------------------
// Non-vacuity: the tests must fail on a field swap / order rotation.
// ---------------------------------------------------------------------------

/// Non-vacuity arm: rotating the insert args (binding `id` first, the
/// pre-`8c89aeb9` defect) produces a visibly wrong row that the assertions
/// above would reject — proving the read-backs are sensitive to order rather
/// than passing for any ordering.
#[test]
fn rotated_args_would_corrupt_the_row_the_read_backs_detect() {
    let (store, path) = store();
    seed_library(&store);

    // The base map carries just two distinct columns so a rotation is
    // unambiguous. Derived content is left to defaults (empty).
    let mut base = BTreeMap::new();
    base.insert("name".to_string(), Value::Text("Rotated".into()));
    base.insert("album_artist".to_string(), Value::Text("RotArtist".into()));
    let (map, _m) = album_post_map_args(ColumnMap::new(base), bare_album_derived());

    // The correct unit: columns sorted, id last in both.
    let row = MarshalledRow::new("album", "alb-rot", &map, &[]).unwrap();
    // The rotated defect (pre-`8c89aeb9`): id FIRST in the args, columns
    // otherwise unchanged. Every set value shifts one column left.
    let mut rotated_args = row.insert_args.clone();
    let id_val = rotated_args.pop().unwrap();
    rotated_args.insert(0, id_val);
    let corrupted = (row.insert_sql.clone(), rotated_args);

    // The corrupt insert stores a visibly wrong row. (The FTS triggers would
    // reject the rotated JSON columns — a separate signal the defect is real;
    // drop them here so the row lands and the misalignment is directly
    // observable.)
    store
        .with_connection_sync(|conn| {
            conn.execute_batch(
                "DROP TRIGGER album_fts_ai; DROP TRIGGER album_fts_ad; DROP TRIGGER album_fts_au;",
            )
        })
        .unwrap();
    assert_eq!(insert(&store, &corrupted), 1);

    // The corruption: `id` holds a set value and every column is misaligned.
    // The read-back assertions in the write→read tests fail on exactly this
    // shape — this arm proves they are sensitive to order, not string order.
    assert_ne!(
        row_text(&store, "SELECT id FROM album"),
        "alb-rot",
        "rotated args must mis-store the id"
    );
    // `name` is 4th in the sorted column list, so it now holds the 4th value
    // (full_text) instead of "Rotated", `album_artist` the id, and `id` a JSON
    // value: every column is misaligned.
    assert_eq!(
        row_text(&store, "SELECT album_artist FROM album"),
        "alb-rot",
        "the rotation must land the id in a set column"
    );
    assert_ne!(
        row_text(&store, "SELECT name FROM album"),
        "Rotated",
        "the rotation must shift the values out of name"
    );
    drop(store);
    std::fs::remove_file(path).ok();
}
