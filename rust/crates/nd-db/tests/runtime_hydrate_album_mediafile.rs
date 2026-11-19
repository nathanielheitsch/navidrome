//! S-D3b golden — album + mediafile hydration EXECUTED against a real SQLite
//! DB: fresh schema via `nd_dbschema`, one isolated temp file per test
//! (harness shape copied from `tests/runtime_hydrate_folder_playlist.rs`).
//!
//! Go truth under test: `dbAlbum`/`PostScan`
//! (`persistence/album_repository.go:29-69`) and `dbMediaFile`/`PostScan`
//! (`persistence/mediafile_repository.go:29-67`), the two base selects
//! (`selectAlbum` `:226-232`, `selectMediaFile` `:204-210` through
//! `withAnnotation` `sql_annotations.go:63-91` and `withBookmark`
//! `sql_bookmarks.go:18-28`), promoted onto the S-D3 shared seam
//! (`nd-db/src/hydrate.rs`) as `row_to_album` → `album_post_scan` and
//! `row_to_mediafile` → `mediafile_post_scan`.
//!
//! The claims this slice is really about, each asserted against a live row:
//! (1) the four/four JSON shims decode with Go's EXACT prefixes and Go's guard
//! pattern (participants UNGUARDED, discs/tags/folder_ids guarded);
//! (2) `Tags.ToGenres` (`model/tag.go:138-148`) derives `genre`/`genres` —
//! stored order, no dedup, case-insensitive ids — and an empty tags shim leaves
//! the `genre` COLUMN value alone;
//! (3) annotation + bookmark columns hydrate for a logged user, decode to zeros
//! with no annotation row, and decode to zeros on light queries that omit them;
//! (4) NULL into a shim string aborts the read (Go's `converting NULL to string
//! is unsupported` parity), as does NULL into a non-pointer timestamp;
//! (5) the write maps are unchanged: none of the hydration-only columns
//! (`library_path`, `library_name`, annotation, `bookmark_position`, `genres`,
//! `id`) is an INSERT/UPDATE key.

use nd_db::album_repo::{self, AlbumRecord, AlbumRepo, AlbumRow};
use nd_db::hydrate::{self, HydrateError, RowGet};
use nd_db::mediafile_repo::{self, MediaFileRecord, MediaFileRepo, MediaFileRow};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::Genre;
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;
use std::collections::BTreeMap;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
const LIB_PATH: &str = "/music";
/// The `starred_at` cell: a stored offset the driver preserves, so the wire text
/// is `+03:00` and NOT a UTC-normalised stamp.
const STARRED_AT: &str = "2024-03-03 03:00:00+03:00";

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled so `SEEDEDRAND`/`NATSORT` are registered per connection
/// (`schema-contract.md` §4).
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-hydrate-am-", ".db"));
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

fn stamp(text: &str) -> chrono::DateTime<chrono::Utc> {
    nd_db::time::parse(text).unwrap_or_else(|| panic!("{text} parses"))
}

/// `rusqlite::types::Value` for a [`Value`].
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
fn hydrate_album_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumRecord> {
    hydrate::row_to_album(row)
        .map_err(HydrateError::Sqlite)
        .and_then(AlbumRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// The same two phases for `dbMediaFile`.
fn hydrate_mediafile_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaFileRecord> {
    hydrate::row_to_mediafile(row)
        .map_err(HydrateError::Sqlite)
        .and_then(MediaFileRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// The album-side library filter in `album_repo`'s pinned unit form, bound to the
/// seeded user so the clause actually narrows rows.
fn album_lib_filter(user_id: &str) -> (String, Vec<Value>) {
    (
        "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from(user_id)],
    )
}

/// `library.id` is the INTEGER PK; `album.library_id` references it.
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

/// `user_library` row making `user_id` a viewer of `lib_id`. Without it the
/// library-filter clause matches nothing, which would make the ordering tests
/// vacuous rather than proof.
fn grant_library(store: &SQLStore, user_id: &str, lib_id: i64) {
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[
            rusqlite::types::Value::Text(user_id.to_string()),
            rusqlite::types::Value::Integer(lib_id),
        ],
    );
}

/// The annotation row a logged-in user's LEFT JOIN picks up.
fn seed_annotation(store: &SQLStore, user_id: &str, item_type: &str, item_id: &str) {
    insert(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, play_date, rating, \
         starred, starred_at, rated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text(user_id.to_string()),
            rusqlite::types::Value::Text(item_id.to_string()),
            rusqlite::types::Value::Text(item_type.to_string()),
            rusqlite::types::Value::Integer(7),
            rusqlite::types::Value::Null,
            rusqlite::types::Value::Integer(4),
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Text(STARRED_AT.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
        ],
    );
}

/// `withBookmark`'s row for a media file (`bookmark.position` is the coalesced
/// `bookmark_position` column).
fn seed_bookmark(store: &SQLStore, user_id: &str, item_id: &str, position: i64) {
    insert(
        store,
        "INSERT INTO bookmark (user_id, item_id, item_type, comment, position, created_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text(user_id.to_string()),
            rusqlite::types::Value::Text(item_id.to_string()),
            rusqlite::types::Value::Text("media_file".to_string()),
            rusqlite::types::Value::Text("at 42".to_string()),
            rusqlite::types::Value::Integer(position),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
        ],
    );
}

/// A realistic tags column: `Rock`/`Jazz`/`rock`/`Rock` plus a non-genre name, so
/// the derivation asserts order, no-dedup, and the case-insensitive ids
/// (`5qDZoz1FBC36K73YeoJ2lF` for Rock/rock, `2qBFcIXtWyKohpkDe44Mv5` for Jazz).
const TAGS_JSON: &str = r#"{"albumversion":[{"id":"v1","value":"Deluxe"}],"genre":[{"id":"g1","value":"Rock"},{"id":"g2","value":"Jazz"},{"id":"g3","value":"rock"},{"id":"g4","value":"Rock"}]}"#;

/// An album written through the record-driven write path, so every shim column
/// holds exactly what `PostMapArgs` marshals.
fn seed_album(store: &SQLStore, lib_id: i64, id: &str) -> AlbumRecord {
    let mut discs = BTreeMap::new();
    discs.insert(1, "disc-one".to_string());
    discs.insert(10, "disc-ten".to_string());
    discs.insert(2, "disc-two".to_string());
    let mut tags = nd_db::sql_tags::DbTags::new();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Jazz".to_string()],
    );
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-1".to_string(),
            name: "Nils Frahm".to_string(),
            sub_role: String::new(),
        }],
    );
    let record = AlbumRecord {
        id: id.to_string(),
        library_id: lib_id,
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
        compilation: false,
        comment: "c".to_string(),
        song_count: 17,
        duration: 1234.5,
        size: 87_654_321,
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
        folder_ids: Some(vec!["fold-1".to_string(), "fold-2".to_string()]),
        explicit_status: "E".to_string(),
        rg_album_gain: Some(-8.5),
        rg_album_peak: Some(0.99),
        description: "desc".to_string(),
        small_image_url: "http://img/s".to_string(),
        medium_image_url: "http://img/m".to_string(),
        large_image_url: "http://img/l".to_string(),
        external_url: "http://ext".to_string(),
        external_info_updated_at: Some(stamp(T1)),
        genre: "ColumnGenre".to_string(),
        tags,
        participants,
        participant_names: vec![album_repo::NamedParticipant {
            id: "ar-1".to_string(),
            name: "Nils Frahm".to_string(),
            sort_name: "Frahm, Nils".to_string(),
            sub_role: String::new(),
        }],
        missing: false,
        imported_at: stamp(T1),
        created_at: Some(stamp(T0)),
        updated_at: Some(stamp(T1)),
        // Join-only, hydration-owned.
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        // Read-path-only artwork state (S-D3c): the seed writes no artwork, and
        // these two fields are never write-map keys.
        image: nd_db::artwork_hydration::ItemImage::default(),
    };
    let put = album_repo::put_sql(id, &record, stamp(T1), &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(store, &put.insert.0, &args);
    record
}

/// An album inserted with raw values, so a shim/annotation/rg cell can bypass the
/// write path (`created_at`/`updated_at` are non-pointer `time.Time` in Go, so
/// they must be non-NULL for the row to read at all).
fn insert_album_raw(store: &SQLStore, lib_id: i64, id: &str, cells: &[(&str, &str)]) {
    let mut columns = vec![
        "id",
        "library_id",
        "name",
        "genre",
        "created_at",
        "updated_at",
        "imported_at",
    ];
    let mut values: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(id.to_string()),
        rusqlite::types::Value::Integer(lib_id),
        rusqlite::types::Value::Text("Abbey Road".to_string()),
        rusqlite::types::Value::Text("ColumnGenre".to_string()),
        rusqlite::types::Value::Text(T0.to_string()),
        rusqlite::types::Value::Text(T1.to_string()),
        rusqlite::types::Value::Text(T1.to_string()),
    ];
    for (column, value) in cells {
        columns.push(column);
        values.push(rusqlite::types::Value::Text(value.to_string()));
    }
    let placeholders = vec!["?"; columns.len()].join(", ");
    insert(
        store,
        &format!(
            "INSERT INTO album ({}) VALUES ({})",
            columns.join(", "),
            placeholders
        ),
        &values,
    );
}

fn insert_mediafile_raw(store: &SQLStore, lib_id: i64, id: &str, cells: &[(&str, &str)]) {
    let mut columns = vec![
        "id",
        "library_id",
        "title",
        "genre",
        "created_at",
        "updated_at",
        "birth_time",
    ];
    let mut values: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(id.to_string()),
        rusqlite::types::Value::Integer(lib_id),
        rusqlite::types::Value::Text("Something".to_string()),
        rusqlite::types::Value::Text("ColumnGenre".to_string()),
        rusqlite::types::Value::Text(T0.to_string()),
        rusqlite::types::Value::Text(T1.to_string()),
        rusqlite::types::Value::Text(T1.to_string()),
    ];
    for (column, value) in cells {
        columns.push(column);
        values.push(rusqlite::types::Value::Text(value.to_string()));
    }
    let placeholders = vec!["?"; columns.len()].join(", ");
    insert(
        store,
        &format!(
            "INSERT INTO media_file ({}) VALUES ({})",
            columns.join(", "),
            placeholders
        ),
        &values,
    );
}

/// Base-read hydrate of one album through `select_album_sql` (`Get`).
fn hydrate_album(
    store: &SQLStore,
    id: &str,
    user_id: Option<&str>,
) -> Result<AlbumRecord, rusqlite::Error> {
    Ok(query_all(
        store,
        &album_repo::get_sql(id, user_id, false, None),
        hydrate_album_row,
    )?
    .pop()
    .expect("one album row"))
}

/// Base-read hydrate of one media file through `select_media_file` (`Get`).
fn hydrate_mediafile(
    store: &SQLStore,
    id: &str,
    user_id: Option<&str>,
) -> Result<MediaFileRecord, rusqlite::Error> {
    Ok(query_all(
        store,
        &mediafile_repo::get_sql(id, user_id, None),
        hydrate_mediafile_row,
    )?
    .pop()
    .expect("one media file row"))
}

/// The message a failed read carries, unwrapped from rusqlite's
/// `FromSqlConversionFailure` (the seam surfaces Go's `ScanStruct` error as a
/// column-conversion failure, so the Go text is the payload — same extraction as
/// `tests/runtime_hydrate_folder_playlist.rs`). Panics when the read succeeds:
/// every malformed-shim case must fail, and fail with Go's prefix.
fn read_error(got: Result<AlbumRecord, rusqlite::Error>) -> String {
    let err = match got {
        Err(err) => err,
        Ok(record) => panic!("expected a read error, got {record:?}"),
    };
    go_error_text(&err)
}

fn mediafile_read_error(got: Result<MediaFileRecord, rusqlite::Error>) -> String {
    let err = match got {
        Err(err) => err,
        Ok(record) => panic!("expected a read error, got {record:?}"),
    };
    go_error_text(&err)
}

fn go_error_text(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::FromSqlConversionFailure(_, _, msg) => msg.to_string(),
        other => panic!("expected a conversion failure, got {other:?}"),
    }
}

/// Base read of a written album: the four shims decode, the library display
/// columns and the row's own `id` land on the record, `rg_*` is read from its own
/// column (no Go shim needed), and `genre`/`genres` are DERIVED from the decoded
/// tags — overwriting the column value.
#[test]
fn album_base_read_hydrates_shims_library_id_rg_and_genres() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    let written = seed_album(&store, lib, "al-1");

    let got = hydrate_album(&store, "al-1", None).expect("base read hydrates");

    // discs: map[int]string decoded, keys numeric (Go's json.Unmarshal into
    // map[int]string accepts the stringified keys the column holds).
    let expected_discs: BTreeMap<i64, String> = written
        .discs
        .clone()
        .unwrap_or_else(|| panic!("seeded discs"));
    assert_eq!(got.discs, Some(expected_discs));

    // folder_ids: nil-vs-empty preserved by unmarshal_nullable_strings.
    assert_eq!(
        got.folder_ids,
        Some(vec!["fold-1".to_string(), "fold-2".to_string()])
    );

    // participants: role-keyed map decoded, and the FTS name input derived from
    // it (sort_name empty — the column does not store sort names).
    assert_eq!(got.participants, written.participants);
    assert_eq!(
        got.participant_names,
        vec![album_repo::NamedParticipant {
            id: "ar-1".to_string(),
            name: "Nils Frahm".to_string(),
            sort_name: String::new(),
            sub_role: String::new(),
        }]
    );

    // tags: stored order survives the decode round-trip.
    assert_eq!(
        got.tags.get("genre"),
        Some(&vec!["Rock".to_string(), "Jazz".to_string()])
    );

    // genre/genres derived by ToGenres, overwriting the column value.
    assert_eq!(got.genre, "Rock".to_string(), "genres[0].Name");
    assert_eq!(
        got.genres,
        vec![
            Genre {
                id: "5qDZoz1FBC36K73YeoJ2lF".to_string(),
                name: "Rock".to_string(),
            },
            Genre {
                id: "2qBFcIXtWyKohpkDe44Mv5".to_string(),
                name: "Jazz".to_string(),
            },
        ]
    );

    // Join-only display columns + the row's own identity.
    assert_eq!(got.id, "al-1");
    assert_eq!(got.library_path, LIB_PATH);
    assert_eq!(got.library_name, "Main");
    assert_eq!(got.library_id, lib);

    // rg_* read straight from the columns (Go's shim copies are identity no-ops).
    assert_eq!(got.rg_album_gain, Some(-8.5));
    assert_eq!(got.rg_album_peak, Some(0.99));

    // Plain persisted columns.
    assert_eq!(got.name, "Abbey Road");
    assert_eq!(got.album_artist, "Nils Frahm");
    assert_eq!(got.catalog_num, "CAT-1");
    assert_eq!(got.song_count, 17);
    assert_eq!(got.duration, 1234.5);
    assert_eq!(got.size, 87_654_321);
    assert!(!got.missing);
    assert_eq!(got.created_at, Some(stamp(T0)));
    assert_eq!(got.updated_at, Some(stamp(T1)));
    assert_eq!(got.imported_at, stamp(T1));
    assert_eq!(got.external_info_updated_at, Some(stamp(T1)));

    // With no logged user the annotation columns are not emitted at all
    // (`withAnnotation` adds only `album.average_rating`), so every field is the
    // zero value — but the base `average_rating` column IS present (0 by DDL).
    assert_eq!(got.annotation.play_count, 0);
    assert_eq!(got.annotation.rating, 0);
    assert!(!got.annotation.starred);
    assert_eq!(got.annotation.starred_at, None);
    assert_eq!(got.annotation.average_rating, 0.0);

    // Write-map invariance.
    assert_write_map_unchanged(&got);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A logged-in user's base read, executed twice: with an annotation row the six
/// columns land on the record (timestamps as Go's RFC3339Nano text WITH the stored
/// zone), and with none the coalesce'd columns are zeros and the three pointers
/// are `None`.
#[test]
fn album_annotation_hydrates_with_a_row_and_is_zero_without_one() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    seed_album(&store, lib, "al-ann");
    insert_album_raw(&store, lib, "al-none", &[("tags", "{}")]);
    seed_annotation(&store, "u-1", "album", "al-ann");

    // Assert: no annotation row → LEFT-join miss.
    let got = hydrate_album(&store, "al-none", Some("u-1")).expect("hydrates without a row");
    assert_eq!(
        got.annotation,
        nd_model::annotation::Annotation {
            play_count: 0,
            play_date: None,
            rating: 0,
            rated_at: None,
            starred: false,
            starred_at: None,
            average_rating: 0.0,
        }
    );

    // Assert: an annotation row → values, and the wire text keeps the stored zone
    // (`+03:00`), not a UTC-normalised instant.
    let got = hydrate_album(&store, "al-ann", Some("u-1")).expect("hydrates with a row");
    assert_eq!(got.annotation.play_count, 7);
    assert_eq!(got.annotation.rating, 4);
    assert!(got.annotation.starred);
    assert_eq!(got.annotation.play_date, None, "NULL *time.Time is nil");
    assert_eq!(
        got.annotation.starred_at.as_deref(),
        Some("2024-03-03T03:00:00+03:00"),
        "RFC3339Nano with the zone the driver parsed"
    );
    assert_eq!(
        got.annotation.rated_at.as_deref(),
        Some("2024-02-02T00:00:00Z"),
        "a UTC-stored stamp renders Z"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The tags guard (`album_repository.go:48`): an EMPTY shim runs no decode, so
/// `genre` keeps the COLUMN value and `genres` stays empty. Executed against a row
/// whose `tags` cell is the empty string — the "column not scanned" spelling.
#[test]
fn album_empty_tags_shim_keeps_the_genre_column_and_no_genres() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    // `album_fts_ai` json_each()s `NEW.tags` (`schema_ddl.rs:85`), so an EMPTY
    // tags cell cannot be inserted while that trigger exists — the empty shim is a
    // legacy-row / projection shape, not a write the current schema allows. Drop
    // the FTS maintenance triggers for this test only; the read path under test
    // does not consult FTS at all.
    for trigger in ["album_fts_ai", "album_fts_au", "album_fts_ad"] {
        insert(&store, &format!("DROP TRIGGER IF EXISTS {trigger}"), &[]);
    }
    insert_album_raw(&store, lib, "al-notags", &[("tags", "")]);

    let got = hydrate_album(&store, "al-notags", None).expect("empty shim is not an error");
    assert_eq!(
        got.genre,
        "ColumnGenre".to_string(),
        "the column value survives"
    );
    assert!(got.genres.is_empty(), "no ToGenres assignment ran");
    assert!(got.tags.is_empty(), "Tags stays nil/empty like Go's");

    // An empty JSON object is also a valid payload — and ToGenres on it is Go's
    // early return, which DOES overwrite genre with "".
    insert_album_raw(&store, lib, "al-emptymap", &[("tags", "{}")]);
    let got = hydrate_album(&store, "al-emptymap", None).expect("{} decodes");
    assert_eq!(
        got.genre,
        String::new(),
        "ToGenres returns \"\" for no genre"
    );
    assert!(got.genres.is_empty());

    // A tags payload with only non-genre names likewise clears genre.
    insert_album_raw(
        &store,
        lib,
        "al-other",
        &[("tags", r#"{"mood":[{"id":"m","value":"Chill"}]}"#)],
    );
    let got = hydrate_album(&store, "al-other", None).expect("decodes");
    assert_eq!(got.genre, String::new());
    assert!(got.genres.is_empty());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `Tags.ToGenres` on the duplicate/case-variant set, executed through a real
/// read: 4 entries, stored order, no dedup, and `Rock`/`rock`/`Rock` sharing the
/// case-insensitive dictionary id (`NewTag` lowercases name AND value).
#[test]
fn album_genres_duplicate_and_case_ids_match_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    insert_album_raw(&store, lib, "al-genres", &[("tags", TAGS_JSON)]);

    let got = hydrate_album(&store, "al-genres", None).expect("tags decode");
    assert_eq!(got.genre, "Rock".to_string(), "scalar = genres[0].Name");
    let names: Vec<&str> = got.genres.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["Rock", "Jazz", "rock", "Rock"]);
    assert_eq!(got.genres.len(), 4, "no dedup, no sort");
    assert_eq!(got.genres[0].id, "5qDZoz1FBC36K73YeoJ2lF");
    assert_eq!(got.genres[1].id, "2qBFcIXtWyKohpkDe44Mv5");
    assert_eq!(got.genres[2].id, "5qDZoz1FBC36K73YeoJ2lF");
    assert_eq!(got.genres[3].id, "5qDZoz1FBC36K73YeoJ2lF");
    // The non-genre name is decoded into Tags but never contributes a genre.
    assert_eq!(
        got.tags.get("albumversion"),
        Some(&vec!["Deluxe".to_string()])
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Each malformed shim fails the read with Go's EXACT prefix — not serde's
/// wording: discs → `parsing album discs from db: `, participants and tags →
/// `parsing album from db: `, folder_ids → `parsing album folder_ids from db: `.
#[test]
fn album_malformed_shims_carry_gos_exact_prefixes() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    // The default `{}` tags cell keeps each shim's failure attributable to its own
    // column; the tags case supplies its payload AS the tags cell.
    insert_album_raw(
        &store,
        lib,
        "al-bad-discs",
        &[("tags", "{}"), ("discs", r#"{"1": 2}"#)],
    );
    insert_album_raw(
        &store,
        lib,
        "al-bad-participants",
        &[("tags", "{}"), ("participants", "{\"albumartist\":\"x\"}")],
    );
    insert_album_raw(
        &store,
        lib,
        "al-bad-tags",
        &[("tags", r#"{"genre":"Rock"}"#)],
    );
    insert_album_raw(
        &store,
        lib,
        "al-bad-folder-ids",
        &[("tags", "{}"), ("folder_ids", r#"{"a":1}"#)],
    );
    // The participants column is the one with NO guard, so an EMPTY string is
    // also a Go failure (`json.Unmarshal("")` → unexpected end of JSON input).
    insert_album_raw(
        &store,
        lib,
        "al-empty-participants",
        &[("participants", "")],
    );

    let msg = read_error(hydrate_album(&store, "al-bad-discs", None));
    assert!(msg.starts_with(hydrate::ALBUM_DISCS_PREFIX), "{msg}");

    let msg = read_error(hydrate_album(&store, "al-bad-participants", None));
    assert!(msg.starts_with(hydrate::ALBUM_PREFIX), "{msg}");
    assert!(msg.contains("parsing participants:"), "{msg}");

    let msg = read_error(hydrate_album(&store, "al-bad-tags", None));
    assert!(msg.starts_with(hydrate::ALBUM_PREFIX), "{msg}");
    assert!(msg.contains("parsing tags:"), "{msg}");

    let msg = read_error(hydrate_album(&store, "al-bad-folder-ids", None));
    assert!(msg.starts_with(hydrate::ALBUM_FOLDER_IDS_PREFIX), "{msg}");

    let msg = read_error(hydrate_album(&store, "al-empty-participants", None));
    assert!(msg.starts_with(hydrate::ALBUM_PREFIX), "{msg}");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// NULL parity on the album shims and on a non-pointer timestamp: Go's
/// `database/sql` refuses NULL into a non-pointer `string`/`time.Time`, so both
/// abort the row read rather than decoding to a zero.
#[test]
fn album_null_shim_and_null_timestamp_abort_the_read() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    // The DDL declares the shim columns NOT NULL (`schema_ddl.rs:58`), so NULL
    // cannot be stored in one. A PROJECTION can still emit NULL for the column —
    // and a leading alias wins rusqlite's name lookup — which is the shape that
    // reaches Go's `converting NULL to string is unsupported` arm.
    insert_album_raw(&store, lib, "al-null", &[("tags", "{}")]);
    let null_shim = (
        "SELECT NULL as participants, album.* FROM album WHERE album.id = ?".to_string(),
        vec![nd_db::sqlrepo::Value::from("al-null")],
    );
    let msg = go_error_text(
        &query_all(&store, &null_shim, hydrate_album_row)
            .expect_err("NULL into Go's non-pointer string shim fails the scan"),
    );
    assert!(
        msg.contains("NULL") && msg.contains("`participants`"),
        "Go parity, and the failing column must be named: {msg}"
    );
    // The message names exactly one column: a sibling shim must not be blamed, so
    // a caller can locate the failing cell rather than guess among the four shims.
    for other in ["tags", "discs", "folder_ids", "created_at"] {
        assert!(
            !msg.contains(&format!("`{other}`")),
            "only the failing column is named, not `{other}`: {msg}"
        );
    }

    // NULL in a non-pointer `time.Time` column (created_at is nullable in DDL).
    insert(
        &store,
        "INSERT INTO album (id, library_id, name, created_at, updated_at, imported_at, tags) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("al-nulltime".to_string()),
            rusqlite::types::Value::Integer(lib),
            rusqlite::types::Value::Text("Abbey Road".to_string()),
            rusqlite::types::Value::Null,
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text("{}".to_string()),
        ],
    );
    let msg = read_error(hydrate_album(&store, "al-nulltime", None));
    assert!(
        msg.contains("NULL") && msg.contains("`created_at`"),
        "Go parity, and the failing column must be named: {msg}"
    );
    assert!(
        !msg.contains("`updated_at`") && !msg.contains("`imported_at`"),
        "the failing timestamp column alone is named: {msg}"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Base read of a written media file for a logged-in user with annotation AND
/// bookmark rows: the annotation fields plus `bookmark_position` land on the
/// record, the two shims decode, `genres` is derived, and the four `rg_*` columns
/// read directly (Go's four shim copies are no-ops).
#[test]
fn mediafile_base_read_hydrates_annotation_bookmark_and_shims() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    let record = mediafile_seed(&store, lib, "mf-1");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 1234);

    let got = hydrate_mediafile(&store, "mf-1", Some("u-1")).expect("base read hydrates");

    assert_eq!(got.bookmark_position, 1234);
    assert_eq!(got.annotation.play_count, 7);
    assert_eq!(got.annotation.rating, 4);
    assert!(got.annotation.starred);
    assert_eq!(
        got.annotation.starred_at.as_deref(),
        Some("2024-03-03T03:00:00+03:00")
    );

    // Shims.
    assert_eq!(got.participants, record.participants);
    assert_eq!(
        got.tags.get("genre"),
        Some(&vec![
            "Rock".to_string(),
            "Jazz".to_string(),
            "rock".to_string()
        ])
    );
    assert_eq!(got.genre, "Rock".to_string());
    let names: Vec<&str> = got.genres.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, vec!["Rock", "Jazz", "rock"]);
    assert_eq!(got.genres[2].id, got.genres[0].id, "case-insensitive id");
    assert_eq!(
        got.participant_names,
        vec![album_repo::NamedParticipant {
            id: "ar-1".to_string(),
            name: "Song Artist".to_string(),
            sort_name: String::new(),
            sub_role: String::new(),
        }]
    );

    // Identity + join-only columns + rg_* + base average_rating.
    assert_eq!(got.id, "mf-1");
    assert_eq!(got.library_path, LIB_PATH);
    assert_eq!(got.library_name, "Main");
    assert_eq!(got.rg_track_gain, Some(-7.0));
    assert_eq!(got.rg_track_peak, Some(0.5));
    assert_eq!(got.rg_album_gain, None, "NULL *float64 is nil");
    assert_eq!(got.bit_depth, None, "NULL *int is nil");
    assert_eq!(got.bit_rate, 320);
    assert_eq!(got.annotation.average_rating, 0.0);

    // Write-map invariance.
    assert_mediafile_write_map_unchanged(&got);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The bookmark column is emitted ONLY for a logged-in user
/// (`withBookmark`, `sql_bookmarks.go:18-28`), so an anonymous base read and a
/// logged-in read with no bookmark row both decode it to Go's zero (`0`); the
/// coalesce makes a LEFT-join miss `0` rather than NULL.
#[test]
fn mediafile_bookmark_position_is_zero_when_absent_or_missing() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    mediafile_seed(&store, lib, "mf-1");

    // No user: no bookmark column at all in the SELECT.
    let got = hydrate_mediafile(&store, "mf-1", None).expect("anonymous read");
    assert_eq!(got.bookmark_position, 0);
    // No annotation columns either.
    assert_eq!(got.annotation, nd_model::annotation::Annotation::default());

    // Logged in, no bookmark row: the coalesce gives 0.
    let got = hydrate_mediafile(&store, "mf-1", Some("u-1")).expect("logged-in read");
    assert_eq!(got.bookmark_position, 0);
    assert_eq!(got.annotation.play_count, 0);
    assert_eq!(got.annotation.starred_at, None);

    // A bookmark row with a NULL position is also Go's 0 (coalesce).
    insert(
        &store,
        "INSERT INTO bookmark (user_id, item_id, item_type, comment, position) \
         VALUES (?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("u-1".to_string()),
            rusqlite::types::Value::Text("mf-1".to_string()),
            rusqlite::types::Value::Text("media_file".to_string()),
            rusqlite::types::Value::Text(String::new()),
            rusqlite::types::Value::Null,
        ],
    );
    let got = hydrate_mediafile(&store, "mf-1", Some("u-1")).expect("null position coalesces");
    assert_eq!(got.bookmark_position, 0);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The light queries: `get_all_ids_sql` and `get_random_rowid_sql` emit no
/// annotation/bookmark columns (and the ids projection only emits `id`), so a
/// hydrator run over a projection without them must not error — every absent
/// optional column decodes to its zero. `average_rating` is a base-table column
/// and is emitted by the base read in both user modes.
#[test]
fn mediafile_light_queries_decode_absent_columns_to_zeros() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    mediafile_seed(&store, lib, "mf-1");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 99);

    // The id-only projection: hydrating it must not fail on the missing columns.
    let ids = query_all(
        &store,
        &mediafile_repo::get_all_ids_sql(Some("u-1"), None, false),
        |row| row.get_text("id"),
    )
    .expect("light id projection reads");
    assert_eq!(ids, vec!["mf-1".to_string()]);

    // A projection with only id + the media_file columns (no join, no annotation,
    // no bookmark): every optional column decodes to zero, and the row still
    // hydrates end to end.
    let sql = (
        "SELECT media_file.* FROM media_file".to_string(),
        Vec::new(),
    );
    let got = query_all(&store, &sql, hydrate_mediafile_row)
        .expect("media_file.* alone hydrates")
        .pop()
        .expect("one row");
    assert_eq!(got.id, "mf-1");
    assert_eq!(got.bookmark_position, 0);
    assert_eq!(got.annotation.play_count, 0);
    assert_eq!(got.annotation.starred_at, None);
    // library_path/library_name are absent aliases → Go's zero, not an error.
    assert_eq!(got.library_path, String::new());
    assert_eq!(got.library_name, String::new());
    // The base-table average_rating is still read (its own column, DDL default 0).
    assert_eq!(got.annotation.average_rating, 0.0);
    // The shims come from the base table, so they still decode.
    assert_eq!(got.genre, "Rock".to_string());
    assert_eq!(got.genres.len(), 3);

    // get_random_rowid_sql's projection is rowid-only — it never reaches the
    // entity hydrator, and must still execute.
    let rowids = query_all(
        &store,
        &mediafile_repo::get_random_rowid_sql(None, None, 0),
        |row| row.get::<_, i64>(0),
    )
    .expect("random rowid projection reads");
    assert_eq!(rowids.len(), 1);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// NULL into Go's non-pointer `participants`/`tags` shim aborts the read
/// (`converting NULL to string is unsupported`), and an empty-string participants
/// cell reaches the UNGUARDED decode and fails with `MEDIAFILE_PREFIX` plus the
/// codec's `parsing participants:` message.
#[test]
fn mediafile_null_participants_abort_and_empty_string_carries_the_prefix() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    insert_mediafile_raw(&store, lib, "mf-null", &[("tags", "{}")]);
    insert_mediafile_raw(
        &store,
        lib,
        "mf-empty",
        &[("participants", ""), ("tags", "{}")],
    );

    // Same projection trick as the album case: `participants` is NOT NULL in the
    // DDL (`schema_ddl.rs:64`), so NULL is reached through a projection.
    let null_shim = (
        "SELECT NULL as participants, media_file.* FROM media_file WHERE media_file.id = ?"
            .to_string(),
        vec![nd_db::sqlrepo::Value::from("mf-null")],
    );
    let msg = go_error_text(
        &query_all(&store, &null_shim, hydrate_mediafile_row)
            .expect_err("NULL into Go's non-pointer string shim fails the scan"),
    );
    assert!(
        msg.contains("NULL") && msg.contains("`participants`"),
        "Go NULL-into-string parity, and the failing column must be named: {msg}"
    );
    assert!(
        !msg.contains("`tags`") && !msg.contains("`created_at`"),
        "only the failing column is named: {msg}"
    );

    let msg = mediafile_read_error(hydrate_mediafile(&store, "mf-empty", None));
    assert!(msg.starts_with(hydrate::MEDIAFILE_PREFIX), "{msg}");
    assert!(msg.contains("parsing participants:"), "{msg}");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A malformed media_file tags payload fails with `MEDIAFILE_PREFIX` +
/// `parsing tags:`, and an empty tags shim leaves the genre column intact.
#[test]
fn mediafile_tags_guard_and_prefix_match_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    insert_mediafile_raw(
        &store,
        lib,
        "mf-bad-tags",
        &[("tags", r#"{"genre":"Rock"}"#)],
    );
    insert_mediafile_raw(&store, lib, "mf-no-tags", &[("tags", "")]);

    let msg = mediafile_read_error(hydrate_mediafile(&store, "mf-bad-tags", None));
    assert!(msg.starts_with(hydrate::MEDIAFILE_PREFIX), "{msg}");
    assert!(msg.contains("parsing tags:"), "{msg}");

    let got = hydrate_mediafile(&store, "mf-no-tags", None).expect("empty shim is not an error");
    assert_eq!(got.genre, "ColumnGenre".to_string());
    assert!(got.genres.is_empty());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Bulk reads keep each row's own identity on its record — the reason `id` is a
/// record field and not a parameter of the hydrator. Asserted for both entities,
/// including a media_file bulk read whose shim payloads differ per row.
#[test]
fn bulk_reads_keep_each_rows_own_id() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_album(&store, lib, "al-2");
    insert_album_raw(
        &store,
        lib,
        "al-3",
        &[
            ("tags", r#"{"genre":[{"id":"a","value":"Techno"}]}"#),
            ("genre", "ColumnGenre"),
        ],
    );

    let mut albums = query_all(
        &store,
        &album_repo::get_all_sql(None, false, None, vec!["album.id".to_string()], 0, 0),
        hydrate_album_row,
    )
    .expect("bulk album read");
    albums.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = albums.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["al-1", "al-2", "al-3"]);
    // Each row keeps its own derived genre.
    assert_eq!(albums[0].genre, "Rock".to_string());
    assert_eq!(albums[2].genre, "Techno".to_string());
    assert_eq!(albums[2].genres[0].name, "Techno".to_string());

    mediafile_seed(&store, lib, "mf-1");
    insert_mediafile_raw(
        &store,
        lib,
        "mf-2",
        &[("tags", r#"{"genre":[{"id":"t","value":"Techno"}]}"#)],
    );
    let mut files = query_all(
        &store,
        &mediafile_repo::get_all_sql(None, None, &[], 0, 0),
        hydrate_mediafile_row,
    )
    .expect("bulk mediafile read");
    files.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = files.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["mf-1", "mf-2"]);
    assert_eq!(files[0].genre, "Rock".to_string());
    assert_eq!(files[1].genre, "Techno".to_string());
    assert_eq!(files[0].genres.len(), 3);
    assert_eq!(files[1].genres.len(), 1);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The DDL sentinel `'0000-00-00 00:00:00'` in `imported_at` (the album's DDL
/// default) SCANS as Go's zero time rather than failing the row: the pinned driver
/// swallows every DATETIME parse failure
/// (`go-sqlite3@v1.14.52/sqlite3.go:2831-2838`).
#[test]
fn album_imported_at_sentinel_scans_as_gos_zero_time() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    insert(
        &store,
        "INSERT INTO album (id, library_id, name, created_at, updated_at, imported_at, tags) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text("al-sentinel".to_string()),
            rusqlite::types::Value::Integer(lib),
            rusqlite::types::Value::Text("Abbey Road".to_string()),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text("0000-00-00 00:00:00".to_string()),
            rusqlite::types::Value::Text("{}".to_string()),
        ],
    );

    let got = hydrate_album(&store, "al-sentinel", None).expect("the sentinel scans");
    assert!(
        hydrate::is_go_zero(&got.imported_at),
        "'0000-00-00 00:00:00' is Go's zero time, not an error"
    );
    assert_eq!(
        hydrate::to_go_time_text(&got.imported_at),
        "0001-01-01T00:00:00Z"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A media file written through the record-driven write path, with a three-value
/// genre list (including a case variant), participants, and one NULL `rg_*`.
fn mediafile_seed(store: &SQLStore, lib_id: i64, id: &str) -> MediaFileRecord {
    let mut tags = nd_db::sql_tags::DbTags::new();
    tags.insert(
        "genre".to_string(),
        vec!["Rock".to_string(), "Jazz".to_string(), "rock".to_string()],
    );
    let mut participants = Participants::new();
    participants.insert(
        Role::from("artist"),
        vec![Participant {
            id: "ar-1".to_string(),
            name: "Song Artist".to_string(),
            sub_role: String::new(),
        }],
    );
    let record = MediaFileRecord {
        id: id.to_string(),
        album: "Abbey Road".to_string(),
        album_artist: "Nils Frahm".to_string(),
        album_artist_id: "aa-1".to_string(),
        album_id: "al-1".to_string(),
        artist: "Song Artist".to_string(),
        artist_id: "ar-1".to_string(),
        birth_time: stamp(T1),
        bit_depth: None,
        bit_rate: 320,
        bpm: None,
        catalog_num: "CAT-1".to_string(),
        channels: 2,
        codec: "flac".to_string(),
        comment: "c".to_string(),
        compilation: false,
        created_at: Some(stamp(T0)),
        date: "1969-09-26".to_string(),
        disc_number: 1,
        disc_subtitle: "d-s".to_string(),
        duration: 24.5,
        explicit_status: "E".to_string(),
        folder_id: "fold-1".to_string(),
        genre: "ColumnGenre".to_string(),
        has_cover_art: true,
        library_id: lib_id,
        lyrics: "[]".to_string(),
        mbz_recording_id: "mbz-rec".to_string(),
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
        path: "/music/a.flac".to_string(),
        pid: "pid-1".to_string(),
        probe_data: "{\"codec\":\"flac\"}".to_string(),
        release_date: "1970-02-01".to_string(),
        release_year: 1970,
        rg_album_gain: None,
        rg_album_peak: None,
        rg_track_gain: Some(-7.0),
        rg_track_peak: Some(0.5),
        sample_rate: 44100,
        size: 12_345_678,
        sort_album_artist_name: "saa".to_string(),
        sort_album_name: "sal".to_string(),
        sort_artist_name: "sar".to_string(),
        sort_title: "sti".to_string(),
        suffix: ".flac".to_string(),
        tags,
        title: "Something".to_string(),
        track_number: 17,
        year: 1969,
        participants,
        participant_names: vec![album_repo::NamedParticipant {
            id: "ar-1".to_string(),
            name: "Song Artist".to_string(),
            sort_name: String::new(),
            sub_role: String::new(),
        }],
        updated_at: Some(stamp(T1)),
        // Join-only, hydration-owned.
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        // Read-path-only artwork state (S-D3c): the seed writes no artwork, and
        // neither field is ever a write-map key.
        image: nd_db::artwork_hydration::ItemImage::default(),
        album_image: nd_db::artwork_hydration::ItemImage::default(),
    };
    let put = mediafile_repo::put_sql(id, &record, stamp(T1), false, &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(store, &put.insert.0, &args);
    record
}

/// The album write map must not carry any hydration-only column, and a hydrated
/// record must render byte-identical write legs to one whose derived fields are
/// empty (Go's `structs:"-"` blocks them from `structs.Map`).
fn assert_write_map_unchanged(got: &AlbumRecord) {
    assert!(
        !got.genres.is_empty(),
        "the fixture must carry derived genres"
    );
    let album_map = album_repo::album_row(got);
    let keys: Vec<&str> = album_map.map().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 48, "45 base columns + 3 FTS columns");
    for forbidden in [
        "library_path",
        "library_name",
        "genres",
        "annotation",
        "play_count",
        "rating",
        "starred",
        "starred_at",
        "play_date",
        "rated_at",
        "average_rating",
        "id",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "{forbidden} must not reach the write map: {keys:?}"
        );
    }
    assert_eq!(
        album_repo::put_sql("al-1", got, stamp(T1), &[]).expect("renders"),
        album_repo::put_sql(
            "al-1",
            &AlbumRecord {
                id: String::new(),
                library_path: String::new(),
                library_name: String::new(),
                annotation: nd_model::annotation::Annotation::default(),
                genres: Vec::new(),
                ..got.clone()
            },
            stamp(T1),
            &[]
        )
        .expect("renders"),
        "the hydrated record re-renders the identical write legs"
    );
}

/// The media_file counterpart.
fn assert_mediafile_write_map_unchanged(got: &MediaFileRecord) {
    assert!(
        !got.genres.is_empty(),
        "the fixture must carry derived genres"
    );
    assert_ne!(
        got.bookmark_position, 0,
        "the fixture must carry a bookmark"
    );
    let mediafile_map = mediafile_repo::mediafile_row(got, false);
    let keys: Vec<&str> = mediafile_map.map().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 66, "63 base columns + 3 FTS columns");
    for forbidden in [
        "library_path",
        "library_name",
        "genres",
        "annotation",
        "play_count",
        "rating",
        "starred",
        "starred_at",
        "play_date",
        "rated_at",
        "average_rating",
        "bookmark_position",
        "id",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "{forbidden} must not reach the write map: {keys:?}"
        );
    }
    assert_eq!(
        mediafile_repo::put_sql("mf-1", got, stamp(T1), false, &[]).expect("renders"),
        mediafile_repo::put_sql(
            "mf-1",
            &MediaFileRecord {
                id: String::new(),
                library_path: String::new(),
                library_name: String::new(),
                annotation: nd_model::annotation::Annotation::default(),
                bookmark_position: 0,
                genres: Vec::new(),
                ..got.clone()
            },
            stamp(T1),
            false,
            &[]
        )
        .expect("renders"),
        "the hydrated record re-renders the identical write legs"
    );
}

/// The squirrel part-order fix, EXECUTED: `selectMediaFile` renders its joins
/// after `FROM` and before WHERE (`select.go:97-113`), and the light queries bind
/// join args before the filter args. Each statement here is run against SQLite, so
/// a mis-ordered clause fails the test rather than the caller.
#[test]
fn mediafile_select_renders_joins_and_filters_in_sql_order() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "admin");
    mediafile_seed(&store, lib, "mf-1");
    mediafile_seed(&store, lib, "mf-2");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 55);

    let lib_filter = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) OR media_file.library_id = ?"
            .to_string(),
        vec![
            nd_db::sqlrepo::Value::from("u-1"),
            nd_db::sqlrepo::Value::Integer(lib),
        ],
    );

    // The base read with BOTH joins and a library filter: valid SQL, and the
    // filter narrows the rows.
    let got = query_all(
        &store,
        &mediafile_repo::get_all_sql(Some("u-1"), Some(lib_filter.clone()), &["title asc"], 0, 0),
        hydrate_mediafile_row,
    )
    .expect("base select with filter executes");
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].bookmark_position, 55,
        "ORDER BY title asc puts mf-1 first"
    );
    assert_eq!(
        got[1].bookmark_position, 0,
        "no bookmark row coalesces to 0"
    );

    // get_all_ids_sql with the annotation join AND a filter: args bind join-first.
    let ids = query_all(
        &store,
        &mediafile_repo::get_all_ids_sql(Some("u-1"), Some(lib_filter.clone()), true),
        |row| row.get_text("id"),
    )
    .expect("id projection with annotation join executes");
    assert_eq!(ids.len(), 2);

    // count_all_sql with the annotation join AND a filter.
    let count = query_all(
        &store,
        &mediafile_repo::count_all_sql(Some("u-1"), Some(lib_filter), true),
        |row| row.get_i64("count"),
    )
    .expect("count with annotation join executes")
    .pop()
    .expect("one count");
    assert_eq!(count, 2);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// squirrel's part order, EXECUTED on the album side: `CountAll` and `getAllIDs`
/// apply Go's library filter first (`album_repository.go:193-200`, `:263-270`) but
/// the builder renders `Joins` before `WHERE` (`select.go:92-118`), so with BOTH a
/// library filter and `need_annotation` the join text must be emitted first and its
/// args must bind first. Rendering WHERE first is not a cosmetic difference: SQLite
/// rejects `FROM album WHERE ... LEFT JOIN annotation ...` with
/// `near "LEFT": syntax error`, so these executed cases fail if the order regresses.
#[test]
fn album_light_queries_bind_join_first_with_a_library_filter() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    grant_library(&store, "u-1", lib);
    seed_album(&store, lib, "al-1");
    seed_album(&store, lib, "al-2");
    seed_annotation(&store, "u-1", "album", "al-1");

    // CountAll: join BEFORE WHERE, join args BEFORE the filter arg.
    let (count_sql, count_args) =
        album_repo::count_all_sql(Some("u-1"), false, Some(album_lib_filter("u-1")), true);
    assert!(
        count_sql.find("LEFT JOIN").expect("join present")
            < count_sql.find(" WHERE ").expect("where present"),
        "join must precede WHERE: {count_sql}"
    );
    assert_eq!(
        count_args,
        vec![Value::from("album"), Value::from("u-1"), Value::from("u-1")]
    );
    let count = query_all(&store, &(count_sql, count_args), |row| row.get_i64("count"))
        .expect("count with join AND filter executes")
        .pop()
        .expect("one count");
    assert_eq!(count, 2, "the granted library still sees both albums");

    // getAllIDs: same order and same arg interleaving, executed.
    let (ids_sql, ids_args) =
        album_repo::get_all_ids_sql(Some(album_lib_filter("u-1")), true, Some("u-1"), false);
    assert!(
        ids_sql.find("LEFT JOIN").expect("join present")
            < ids_sql.find(" WHERE ").expect("where present"),
        "join must precede WHERE: {ids_sql}"
    );
    let ids = query_all(&store, &(ids_sql, ids_args), |row| row.get_text("id"))
        .expect("id projection with join AND filter executes");
    assert_eq!(ids.len(), 2);

    // A user granted no library produces the SAME valid SQL but zero rows, so the
    // filter is genuinely applied rather than merely parseable.
    seed_user(&store, "u-2", "nobody");
    let (none_sql, none_args) =
        album_repo::count_all_sql(Some("u-2"), false, Some(album_lib_filter("u-2")), true);
    let zero = query_all(&store, &(none_sql, none_args), |row| row.get_i64("count"))
        .expect("count executes for the ungranted user")
        .pop()
        .expect("one count");
    assert_eq!(zero, 0, "the library filter narrows the count to zero");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `Get`/`GetAll` on media_file with a library filter: the id predicate must join
/// the filter's existing `WHERE` with ` AND `, not open a second one
/// (`select.go:112-117`). Two ` WHERE ` clauses are a SQLite syntax error
/// (`near "WHERE"`), so this is an executed proof rather than a rendering check.
#[test]
fn mediafile_get_and_get_all_join_one_where_with_a_library_filter() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    grant_library(&store, "u-1", lib);
    mediafile_seed(&store, lib, "mf-1");
    mediafile_seed(&store, lib, "mf-2");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 42);

    let filter = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from("u-1")],
    );

    // Get(id, user, filter): exactly one WHERE, the id AND-joined after the filter,
    // and the annotation/bookmark joins still contribute their columns.
    let (sql, args) = mediafile_repo::get_sql("mf-1", Some("u-1"), Some(filter.clone()));
    // One OUTER WHERE: the library clause contributes its own inner
    // `WHERE ul.user_id = ?` inside the subquery, so two occurrences total. The
    // old shape (a second outer WHERE) would make it three.
    assert_eq!(
        sql.matches(" WHERE ").count(),
        2,
        "outer WHERE plus the subquery's own: {sql}"
    );
    assert!(
        !sql.contains(" WHERE media_file.id = ?"),
        "the id predicate must AND-join, not open a second WHERE: {sql}"
    );
    assert!(sql.ends_with(" AND media_file.id = ?"), "{sql}");
    // Arg order follows the clauses: annotation join (item_type, user), bookmark
    // join (user), the library filter (user), then the id.
    assert_eq!(
        args,
        vec![
            Value::from("media_file"),
            Value::from("u-1"),
            Value::from("u-1"),
            Value::from("u-1"),
            Value::from("mf-1"),
        ]
    );
    let got = query_all(&store, &(sql, args), hydrate_mediafile_row)
        .expect("get_sql with a library filter executes")
        .pop()
        .expect("one row");
    assert_eq!(got.id, "mf-1");
    assert_eq!(got.bookmark_position, 42);
    assert_eq!(got.annotation.play_count, 7);

    // GetAll(user, filter, order/max/offset): one WHERE, ordered page, executed.
    let (all_sql, all_args) =
        mediafile_repo::get_all_sql(Some("u-1"), Some(filter), &["title asc"], 1, 1);
    assert_eq!(
        all_sql.matches(" WHERE ").count(),
        2,
        "outer WHERE plus the subquery's own: {all_sql}"
    );
    assert!(
        all_sql.ends_with(" LIMIT 1 OFFSET 1"),
        "pagination still applies: {all_sql}"
    );
    let page = query_all(&store, &(all_sql, all_args), hydrate_mediafile_row)
        .expect("get_all_sql with a library filter executes");
    assert_eq!(page.len(), 1, "max = 1 caps the page");
    assert_eq!(page[0].id, "mf-2", "offset = 1 skips the first row");

    // The same Get statement for a user with no granted library: valid SQL, zero
    // rows — the filter is applied, not just parseable.
    seed_user(&store, "u-2", "nobody");
    let (miss_sql, miss_args) = mediafile_repo::get_sql(
        "mf-1",
        Some("u-2"),
        Some((
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("u-2")],
        )),
    );
    assert!(
        query_all(&store, &(miss_sql, miss_args), hydrate_mediafile_row)
            .expect("get_sql executes for the ungranted user")
            .is_empty(),
        "the library filter hides the row"
    );

    // The GetRandom two-pass pair, executed as Go runs it: phase 1 picks rowids
    // under BOTH a filter clause and the library filter (one AND-joined `WHERE`,
    // `mediafile_repository.go:257-258`), phase 2 hydrates those rowids on the base
    // select. Together they prove neither pass carries a stray second `WHERE` and
    // that phase 2's rows decode through the hydrator.
    let (rowid_sql, rowid_args) = mediafile_repo::get_random_rowid_sql(
        Some(("media_file.missing = ?", vec![nd_db::sqlrepo::Value::Bool(false)])),
        Some((
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("u-1")],
        )),
        0,
    );
    assert_eq!(
        rowid_sql.matches(" WHERE ").count(),
        2,
        "one outer WHERE plus the subquery's own: {rowid_sql}"
    );
    let rowids = query_all(&store, &(rowid_sql, rowid_args), |row| row.get_i64("rowid"))
        .expect("the rowid pick executes with filter AND library clause");
    assert_eq!(rowids.len(), 2);
    // Phase 2 re-applies the library filter: Go builds it with `selectMediaFile(ctx)`
    // (:274), whose last step is an unconditional `applyLibraryFilter` (:209), so the
    // hydrated set is {rowids} ∩ {visible libraries}.
    let (hydrate_sql, hydrate_args) = mediafile_repo::get_random_hydrate_sql(
        Some("u-1"),
        Some((
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("u-1")],
        )),
        &rowids,
    );
    assert_eq!(
        hydrate_sql.matches(" WHERE ").count(),
        2,
        "one outer WHERE (the library clause) plus the subquery's own: {hydrate_sql}"
    );
    assert!(
        hydrate_sql.contains(" AND media_file.rowid IN (?, ?) ORDER BY random()"),
        "the rowid IN-list AND-joins the library clause rather than opening a second WHERE: {hydrate_sql}"
    );
    let shuffled = query_all(&store, &(hydrate_sql, hydrate_args), hydrate_mediafile_row)
        .expect("phase 2 hydrates the picked rowids");
    assert_eq!(shuffled.len(), 2);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Insert an album with a caller-chosen `song_count` and an annotation row with a
/// caller-chosen `play_count`, so the normalized play-count expression can be
/// steered into each `%g` arm.
fn seed_normalized_album(
    store: &SQLStore,
    lib_id: i64,
    id: &str,
    user_id: &str,
    song_count: i64,
    play_count: i64,
) {
    insert(
        store,
        "INSERT INTO album (id, library_id, name, genre, song_count, created_at, updated_at, \
         imported_at, tags) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text(id.to_string()),
            rusqlite::types::Value::Integer(lib_id),
            rusqlite::types::Value::Text("Abbey Road".to_string()),
            rusqlite::types::Value::Text("ColumnGenre".to_string()),
            rusqlite::types::Value::Integer(song_count),
            rusqlite::types::Value::Text(T0.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
            rusqlite::types::Value::Text("{}".to_string()),
        ],
    );
    insert(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Text(user_id.to_string()),
            rusqlite::types::Value::Text(id.to_string()),
            rusqlite::types::Value::Text("album".to_string()),
            rusqlite::types::Value::Integer(play_count),
            rusqlite::types::Value::Integer(0),
            rusqlite::types::Value::Integer(0),
        ],
    );
}

/// The normalized album play count (`sql_annotations.go:80-83`) is an expression
/// column, so SQLite gives it REAL storage. `hydrate::annotation_play_count`
/// reproduces Go's int64 scan arm exactly: the `%g` spelling of the float decides
/// the outcome (`database/sql/convert.go:499-505`, `asString` at `:570-571`).
///
/// 7 plays over 3 songs is `round(round(7.0/3.0, 1), 0)` = 2.0, whose `%g` spelling
/// is "2", so the read succeeds with 2 — neither an error nor a truncation of the
/// intermediate 2.3333. The unnormalized mode stays on the INTEGER path.
#[test]
fn normalized_album_play_count_is_a_real_cell_that_hydrates() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    seed_normalized_album(&store, lib, "al-norm", "u-1", 3, 7);

    // The premise: the normalized expression really is a REAL cell.
    let cell_type = query_all(
        &store,
        &(
            "SELECT typeof(round(coalesce(round(cast(7 as float) / coalesce(3, 1), 1), 0))) \
             as cell_type"
                .to_string(),
            Vec::new(),
        ),
        |row| row.get_text("cell_type"),
    )
    .expect("typeof probe executes")
    .pop()
    .expect("one value");
    assert_eq!(cell_type, "real", "the normalized column is REAL");

    let got = query_all(
        &store,
        &album_repo::get_sql("al-norm", Some("u-1"), true, None),
        hydrate_album_row,
    )
    .expect("normalized play count hydrates")
    .pop()
    .expect("one album");
    assert_eq!(
        got.annotation.play_count, 2,
        "round(7/3) is Go's 2, read through the %g spelling"
    );

    // Unnormalized mode is `coalesce(play_count, 0)`: an INTEGER cell, unchanged.
    let plain = query_all(
        &store,
        &album_repo::get_sql("al-norm", Some("u-1"), false, None),
        hydrate_album_row,
    )
    .expect("plain play count hydrates")
    .pop()
    .expect("one album");
    assert_eq!(plain.annotation.play_count, 7);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A normalized play count whose `%g` spelling uses exponent notation is Go's
/// conversion FAILURE rather than a truncated integer: `strconv.ParseInt("1e+06",
/// 10, 64)` errors, `convert.go:504` normalises that to `strconv.ErrSyntax`, and
/// `convert.go:505` formats
/// `converting driver.Value type float64 ("1e+06") to a int64: invalid syntax`.
/// The message must name the column so a bulk read stays diagnosable.
#[test]
fn normalized_album_play_count_in_exponent_form_fails_like_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    // song_count 1, play_count 1_000_000 -> the expression yields 1e+06.
    seed_normalized_album(&store, lib, "al-big", "u-1", 1, 1_000_000);

    let msg = go_error_text(
        &query_all(
            &store,
            &album_repo::get_sql("al-big", Some("u-1"), true, None),
            hydrate_album_row,
        )
        .expect_err("the exponent-form normalized cell fails the int64 scan like Go"),
    );
    assert!(
        msg.contains("converting driver.Value type float64 (\"1e+06\") to a int64"),
        "Go's conversion message, spelling included: {msg}"
    );
    assert!(
        msg.contains("invalid syntax") && msg.contains("play_count"),
        "the failure reason and the failing column are both named: {msg}"
    );

    // The unnormalized mode reads the same row fine (INTEGER cell), so the failure
    // is the REAL spelling, not the stored value.
    let plain = query_all(
        &store,
        &album_repo::get_sql("al-big", Some("u-1"), false, None),
        hydrate_album_row,
    )
    .expect("the integer path is unaffected")
    .pop()
    .expect("one album");
    assert_eq!(plain.annotation.play_count, 1_000_000);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// An infinite REAL `play_count` must surface Go's scan error, never a panic.
/// SQLite accepts the literal `9e999` into the INTEGER-declared
/// `annotation.play_count` column and keeps it as the REAL value `Inf` (verified:
/// `typeof()` = `real`), and the normalized expression propagates that infinity.
/// Go renders the driver `float64` with `%g` as `+Inf` and `strconv.ParseInt`
/// rejects it, surfacing `converting driver.Value type float64 ("+Inf") to a int64:
/// invalid syntax` (verified against the pinned go-sqlite3 driver). Rust's `{:e}`
/// prints `inf` with NO exponent, so the shared `%g` renderer would have PANICED
/// here (`criteria_sql.rs::go_fmt_float64`); it now spells non-finite values Go's
/// way so the caller gets Go's error.
#[test]
fn normalized_album_play_count_infinity_fails_like_go_instead_of_panicking() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    seed_normalized_album(&store, lib, "al-inf", "u-1", 1, 5);
    // The literal, exactly as a legacy row could arrive: SQLite coerces it to +Inf.
    insert(
        &store,
        "UPDATE annotation SET play_count = 9e999 WHERE item_id = 'al-inf'",
        &[],
    );

    // The premise: the normalized expression really yields an infinite REAL cell.
    let cells = query_all(
        &store,
        &(
            "SELECT typeof(round(coalesce(round(cast(play_count as float) / \
                 coalesce(album.song_count, 1), 1), 0))) as t, \
             round(coalesce(round(cast(play_count as float) / \
                 coalesce(album.song_count, 1), 1), 0)) as p \
             FROM album LEFT JOIN annotation ON (annotation.item_id = album.id \
             AND annotation.item_type = 'album' AND annotation.user_id = 'u-1') \
             WHERE album.id = 'al-inf'"
                .to_string(),
            Vec::new(),
        ),
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
    )
    .expect("the normalized cell is readable raw");
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].0, "real", "normalized cell is REAL");
    assert!(
        cells[0].1.is_infinite(),
        "expected +Inf, got {}",
        cells[0].1
    );

    let msg = go_error_text(
        &query_all(
            &store,
            &album_repo::get_sql("al-inf", Some("u-1"), true, None),
            hydrate_album_row,
        )
        .expect_err("an infinite REAL cell fails the int64 scan like Go, not by panicking"),
    );
    assert!(
        msg.contains("converting driver.Value type float64 (\"+Inf\") to a int64"),
        "Go's conversion message with Go's %g spelling: {msg}"
    );
    assert!(
        msg.contains("invalid syntax") && msg.contains("play_count"),
        "the failure reason and the failing column are both named: {msg}"
    );

    // The NEGATIVE infinity arm, executed too: SQLite keeps `-9e999` as the REAL
    // value -Inf and Go's `%g` spells it `-Inf`, which `strconv.ParseInt` rejects
    // with the same shape (verified against the pinned driver: `sql: Scan error on
    // column index 0, name "play_count": converting driver.Value type float64
    // ("-Inf") to a int64: invalid syntax`). Rust's `{:e}` prints `-inf`, so this
    // arm also exercised the pre-fix panic path.
    insert(
        &store,
        "UPDATE annotation SET play_count = -9e999 WHERE item_id = 'al-inf'",
        &[],
    );
    let msg = go_error_text(
        &query_all(
            &store,
            &album_repo::get_sql("al-inf", Some("u-1"), true, None),
            hydrate_album_row,
        )
        .expect_err("-Inf fails the int64 scan like Go"),
    );
    assert!(
        msg.contains("converting driver.Value type float64 (\"-Inf\") to a int64"),
        "Go's %g spelling of negative infinity: {msg}",
    );
    assert!(
        msg.contains("invalid syntax") && msg.contains("play_count"),
        "the failure reason and the failing column are both named: {msg}",
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `created_at`/`updated_at` survive a repo `put` followed by the repo read, for
/// BOTH entities. Go's non-pointer `time.Time` fields are nullable in the DDL, but
/// a Rust-written row never stores NULL in them: mediafile's `put_sql` stamps
/// `created_at` when the record holds `None` (`mediafile_repository.go:184-190`),
/// and album rows arrive from the scanner with both stamps set. So the NULL read
/// error is reserved for foreign/legacy rows, and the round-trip here proves the
/// write path and the read path agree.
#[test]
fn put_then_hydrate_round_trips_timestamps_for_both_entities() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);

    // Album: written through put_sql (insert leg), read back through selectAlbum.
    let album = seed_album(&store, lib, "al-rt");
    let got = hydrate_album(&store, "al-rt", None).expect("album round-trip hydrates");
    assert_eq!(got.created_at, album.created_at, "created_at survives");
    assert_eq!(got.updated_at, album.updated_at, "updated_at survives");
    assert_eq!(got.imported_at, album.imported_at, "imported_at survives");

    // Media file: created_at None on the record, so put_sql's stamp arm fires
    // (`mediafile_repository.go:184-190`) and the stored cell is non-NULL.
    let mf = mediafile_seed(&store, lib, "mf-rt");
    let got = hydrate_mediafile(&store, "mf-rt", None).expect("media_file round-trip hydrates");
    assert_eq!(got.created_at, mf.created_at, "created_at survives");
    assert_eq!(got.updated_at, mf.updated_at, "updated_at survives");

    // The stamp arm explicitly: a record whose created_at is None hydrates back as
    // the stamp the write bound, never as NULL (which would abort the read).
    let stamped_at = stamp(T1);
    let mut unstamped = mf.clone();
    unstamped.created_at = None;
    let put =
        mediafile_repo::put_sql("mf-stamp", &unstamped, stamped_at, false, &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(&store, &put.insert.0, &args);
    let got = hydrate_mediafile(&store, "mf-stamp", None)
        .expect("a stamped created_at is readable, not Go's NULL scan error");
    assert_eq!(
        got.created_at,
        Some(stamped_at),
        "put_sql's stamp arm keeps created_at non-NULL"
    );
    assert_eq!(got.updated_at, mf.updated_at);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `has_column` is the accessor the hydrators branch on; asserted here so the
/// absent-optional-column rule cannot silently become a panic or an error.
#[test]
fn has_column_reports_join_projections() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    mediafile_seed(&store, lib, "mf-1");

    let present = query_all(
        &store,
        &mediafile_repo::get_sql("mf-1", Some("u-1"), None),
        |row| {
            Ok((
                RowGet::has_column(row, "bookmark_position"),
                RowGet::has_column(row, "starred_at"),
                RowGet::has_column(row, "library_path"),
                RowGet::has_column(row, "genre"),
            ))
        },
    )
    .expect("base read")
    .pop()
    .expect("one row");
    assert_eq!(present, (true, true, true, true));

    let (store2, path2) = fresh_store();
    let lib2 = seed_library(&store2);
    mediafile_seed(&store2, lib2, "mf-1");
    let absent = query_all(
        &store2,
        &(
            "SELECT media_file.id FROM media_file".to_string(),
            Vec::new(),
        ),
        |row| Ok(RowGet::has_column(row, "bookmark_position")),
    )
    .expect("projection reads")
    .pop()
    .expect("one row");
    assert!(!absent, "an id-only projection has no bookmark_position");

    drop(store);
    std::fs::remove_file(&path).unwrap();
    drop(store2);
    std::fs::remove_file(&path2).unwrap();
}

/// Nested JSON `null` in the tags/participants shims: Go's `encoding/json`
/// assigns the **zero value** to a non-pointer list/element/field for `null`
/// (`decode.go` `literalStore`), so these cells decode with no error. Executed
/// against the real `unmarshalTags`/`unmarshalParticipants`: `{"genre":null}` →
/// `[]string{}`, `{"genre":[null]}` → `[""]`, `{"albumartist":[null]}` → one zero
/// participant. serde's plain derive rejects all three, which would fail the whole
/// row on a cell Go reads — so the codecs decode through nullable fields.
#[test]
fn nested_json_null_in_shims_decodes_like_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);

    // `{"genre":null}`: tags keeps an EMPTY genre list, so ToGenres returns "" and
    // overwrites the `genre` column exactly as Go does (`album_repository.go:57`).
    insert_album_raw(
        &store,
        lib,
        "al-null-genre-list",
        &[("tags", r#"{"genre":null}"#)],
    );
    let got =
        hydrate_album(&store, "al-null-genre-list", None).expect("Go reads a null genre list");
    assert_eq!(got.genre, "", "ToGenres overwrites the column with \"\"");
    assert_eq!(
        got.tags.get("genre"),
        Some(&Vec::new()),
        "the genre name survives with an empty value list"
    );

    // `{"genre":[null]}`: one element, decoded to "". len(values) == 1, so
    // `genres[0].Name` is "" — the scalar genre is empty but Genres is non-empty.
    insert_album_raw(
        &store,
        lib,
        "al-null-genre-element",
        &[("tags", r#"{"genre":[null]}"#)],
    );
    let got = hydrate_album(&store, "al-null-genre-element", None)
        .expect("Go reads a null tag element as \"\"");
    assert_eq!(got.tags.get("genre"), Some(&vec![String::new()]));
    assert_eq!(got.genre, "");

    // `{"albumartist":[{"id":null,"name":null}]}`: one zero participant, no error.
    insert_album_raw(
        &store,
        lib,
        "al-null-participant",
        &[
            ("tags", "{}"),
            (
                "participants",
                r#"{"albumartist":[{"id":null,"name":null}]}"#,
            ),
        ],
    );
    let got = hydrate_album(&store, "al-null-participant", None)
        .expect("Go reads null participant fields as the zero value");
    let role = nd_db::sql_participations::Role::from("albumartist");
    assert_eq!(
        got.participants.get(&role),
        Some(&vec![nd_db::sql_participations::Participant::default()]),
        "the entry is kept and zero-filled, not dropped"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The same null tolerance on the mediafile shims, whose PostScan is
/// `mediafile_repository.go:49-67`.
#[test]
fn mediafile_nested_json_null_in_shims_decodes_like_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    insert_mediafile_raw(
        &store,
        lib,
        "mf-null-shims",
        &[
            ("participants", r#"{"artist":[null]}"#),
            ("tags", r#"{"genre":[{"id":null,"value":null}]}"#),
        ],
    );
    let got = hydrate_mediafile(&store, "mf-null-shims", None)
        .expect("Go reads null elements and null fields as zero values");
    let role = nd_db::sql_participations::Role::from("artist");
    assert_eq!(
        got.participants.get(&role),
        Some(&vec![nd_db::sql_participations::Participant::default()])
    );
    assert_eq!(got.tags.get("genre"), Some(&vec![String::new()]));
    // `{"genre":[{"id":null,"value":null}]}` yields one value (""), so ToGenres
    // takes the `genres[0].Name` branch and the scalar genre becomes "".
    assert_eq!(got.genre, "");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// S-D3c: the executed repo reads, wired to the shared seam.
//
// Everything above drives the seam from the test (row mapper + `query_all`).
// These tests drive `AlbumRepo`/`MediaFileRepo` themselves, so the wiring —
// statement → `query_all` → `row_to_*` → `into_record` →
// `HydrateError::into_sqlite` → artwork layer — is what executes.
// ---------------------------------------------------------------------------

/// Run one closure on a leased connection for the S-D3c repo tests, which return
/// the repo's own error type rather than a raw `rusqlite::Error` (the `on` helper
/// above exists for the seam-level reads that return `rusqlite::Result`).
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// `AlbumRepo::get` / `AlbumRepo::get_all` run the real statements on the fresh
/// schema (`album_repository.go:234-243`, `:245-256`).
#[test]
fn album_repo_get_and_get_all_run_through_the_seam() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_album(&store, lib, "al-2");

    let one = lease(&store, |conn| {
        AlbumRepo::new(conn).get("al-1", None, false, None)
    })
    .expect("get reads the row");
    assert_eq!(one.id, "al-1");
    assert_eq!(one.library_path, LIB_PATH, "the join columns hydrate");
    assert_eq!(one.library_name, "Main");
    // The PostScan layer ran: the shim JSON is decoded, not left raw.
    assert_eq!(one.genre, "Rock");
    assert_eq!(one.genres.len(), 2);
    assert!(one.discs.is_some(), "the discs shim decoded");
    assert!(one.folder_ids.is_some(), "the folder_ids shim decoded");
    // No artwork was seeded, so the second layer leaves it unresolved — not absent.
    assert_eq!(one.image, nd_db::artwork_hydration::ItemImage::default());
    // Write-map invariance through the repo read, not just the raw seam read.
    assert_write_map_unchanged(&one);

    let all = lease(&store, |conn| {
        AlbumRepo::new(conn).get_all(None, false, None, vec!["album.id".to_string()], 0, 0)
    })
    .expect("get_all reads both rows");
    assert_eq!(all.artwork_error, None);
    let ids: Vec<&str> = all.items.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["al-1", "al-2"], "each row keeps its own id");

    // Go's Get decides NotFound from the empty slice (`album_repository.go:239-241`).
    let miss = lease(&store, |conn| {
        AlbumRepo::new(conn).get("al-missing", None, false, None)
    });
    assert!(
        matches!(miss, Err(album_repo::AlbumError::NotFound)),
        "{miss:?}"
    );
    // …while GetAll of the same empty set is an EMPTY Vec, not NotFound
    // (dbx `All` makes an empty non-nil slice and never returns sql.ErrNoRows:
    // `dbx@v1.12.0/query.go:274-277`, `rows.go:109-125`). Emptiness comes from a
    // library filter that matches nothing — `get_all_sql` takes order-by
    // expressions, not filters, so this is the filter-shaped way to get zero rows.
    let nobody = album_lib_filter("u-nobody");
    let none = lease(&store, |conn| {
        AlbumRepo::new(conn).get_all(
            None,
            false,
            Some(nobody),
            vec!["album.id".to_string()],
            0,
            0,
        )
    })
    .expect("empty page is not an error");
    assert!(none.items.is_empty());
    assert_eq!(
        none.artwork_error, None,
        "no rows, so no artwork query fires"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `MediaFileRepo::get` / `get_all` run the real statements, annotation and
/// bookmark columns included for a logged-in user
/// (`mediafile_repository.go:212-221`, `:232-242`).
#[test]
fn mediafile_repo_get_and_get_all_run_through_the_seam() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    grant_library(&store, "u-1", lib);
    mediafile_seed(&store, lib, "mf-1");
    mediafile_seed(&store, lib, "mf-2");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 4242);

    let filter = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from("u-1")],
    );

    let one = lease(&store, |conn| {
        MediaFileRepo::new(conn).get("mf-1", Some("u-1"), Some(filter.clone()), true)
    })
    .expect("get reads the row");
    assert_eq!(one.id, "mf-1");
    assert_eq!(one.bookmark_position, 4242, "the bookmark column hydrates");
    assert_eq!(one.annotation.play_count, 7);
    assert!(one.annotation.starred);
    assert_eq!(
        one.annotation.starred_at.as_deref(),
        Some("2024-03-03T03:00:00+03:00")
    );
    assert_eq!(one.library_path, LIB_PATH);
    assert_eq!(one.genre, "Rock");
    assert_eq!(one.genres.len(), 3);
    assert_mediafile_write_map_unchanged(&one);

    let all = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_all(
            Some("u-1"),
            Some(filter.clone()),
            &["media_file.id"],
            0,
            0,
            true,
        )
    })
    .expect("get_all reads both rows");
    assert_eq!(all.artwork_error, None);
    let ids: Vec<&str> = all.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["mf-1", "mf-2"]);
    // Per-row identity: mf-1 has the annotation/bookmark, mf-2 has neither.
    let second = all.items.iter().find(|m| m.id == "mf-2").expect("mf-2");
    assert_eq!(second.bookmark_position, 0);
    assert_eq!(second.annotation.play_count, 0);

    let miss = lease(&store, |conn| {
        MediaFileRepo::new(conn).get("mf-nope", Some("u-1"), Some(filter.clone()), true)
    });
    assert!(
        matches!(miss, Err(mediafile_repo::MediaFileError::NotFound)),
        "{miss:?}"
    );
    // Empty page ⇒ empty Vec, same dbx rule as the album side. A library filter
    // bound to a user with no grant narrows the statement to zero rows.
    let ungranted = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from("u-nobody")],
    );
    let none = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, Some(ungranted), &["media_file.id"], 0, 0, true)
    })
    .expect("empty page is not an error");
    assert!(none.items.is_empty());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `MediaFileRepo::get_random` (`mediafile_repository.go:250-283`): phase 1 reads
/// bare rowids (never handed to a hydrator), phase 2 hydrates them, and the
/// artwork layer runs on the result. `LIMIT` caps phase 1, so the returned set is
/// exactly that many rows — each with its own id.
#[test]
fn mediafile_repo_get_random_runs_both_passes() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    for n in 1..=5 {
        mediafile_seed(&store, lib, &format!("mf-{n}"));
    }
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");

    let outcome = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_random(None, None, 3, None, true)
    })
    .expect("both passes run");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), 3, "phase 1's LIMIT is the page size");
    let ids: Vec<&str> = outcome.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids.len(), 3);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "each row is a distinct track: {ids:?}");
    // Phase 2 hydrated full rows (not rowids): the join columns and shims are present.
    for track in &outcome.items {
        assert_eq!(track.library_path, LIB_PATH, "{}", track.id);
        assert_eq!(track.genre, "Rock", "{}", track.id);
        // The album kind was hydrated onto every track (artwork_hydration.go:110).
        assert_eq!(track.album_image.image_hash, "albumhash", "{}", track.id);
    }

    // A filter that matches nothing short-circuits after phase 1: no phase-2
    // statement, no artwork query, empty Vec (`:267-269`).
    let none = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_random(
            Some(("media_file.id = ?", vec![Value::from("mf-nope")])),
            None,
            0,
            None,
            true,
        )
    })
    .expect("empty rowid set is not an error");
    assert!(none.items.is_empty());
    assert_eq!(none.artwork_error, None);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `GetRandom` phase 2 re-applies the library filter. Go's phase 2 is
/// `r.selectMediaFile(ctx)` (`mediafile_repository.go:274`) and that helper ends
/// with an UNCONDITIONAL `return r.applyLibraryFilter(ctx, sql)` (`:204-210`, `:209`),
/// so the hydrated row set is {phase-1 rowids} ∩ {visible libraries}
/// (`sql_base_repository.go:261-283`) — NOT merely the rowids. Two libraries,
/// one granted: a rowid belonging to the ungranted one must not survive phase 2.
#[test]
fn mediafile_repo_get_random_phase_two_reapplies_the_library_filter() {
    let (store, path) = fresh_store();
    let granted = seed_library(&store);
    // A second library, so phase 1 has rows from both to choose from. `insert`
    // returns rows-affected, so the id is the literal bound above.
    let ungranted: i64 = 2;
    insert(
        &store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[
            rusqlite::types::Value::Integer(ungranted),
            rusqlite::types::Value::Text("Hidden".to_string()),
            rusqlite::types::Value::Text("/hidden".to_string()),
            rusqlite::types::Value::Text(String::new()),
        ],
    );
    seed_user(&store, "u-1", "viewer");
    grant_library(&store, "u-1", granted);
    mediafile_seed(&store, granted, "mf-in");
    mediafile_seed(&store, ungranted, "mf-out");

    let filter = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from("u-1")],
    );

    // The executed read: both rowids exist, but the ungranted library's track
    // cannot come back. `max = 0` means no LIMIT, so phase 1 definitely picks both.
    let outcome = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_random(None, Some(filter.clone()), 0, Some("u-1"), true)
    })
    .expect("both passes run under a library filter");
    assert_eq!(outcome.artwork_error, None);
    let ids: Vec<&str> = outcome.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["mf-in"],
        "phase 2 excluded the ungranted library: {ids:?}"
    );

    // The clause-level proof that the assertion above came from phase 2 (not from
    // phase 1 happening to skip the rowid): the phase-2 statement carries the
    // library clause AND-joined with the rowid IN-list, and its arg list binds the
    // filter's user id BEFORE the rowids.
    let (phase2_sql, phase2_args) =
        mediafile_repo::get_random_hydrate_sql(Some("u-1"), Some(filter), &[1, 2]);
    assert!(
        phase2_sql.contains("media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"),
        "phase 2 must carry the library clause: {phase2_sql}"
    );
    assert!(
        phase2_sql.contains(" AND media_file.rowid IN (?, ?) ORDER BY random()"),
        "the IN-list AND-joins the library clause: {phase2_sql}"
    );
    assert_eq!(
        phase2_sql.matches(" WHERE ").count(),
        2,
        "one outer WHERE plus the subquery's own: {phase2_sql}"
    );
    // annotation join (item_type, user), bookmark join (user), filter user, rowids.
    assert_eq!(
        phase2_args,
        vec![
            Value::from("media_file"),
            Value::from("u-1"),
            Value::from("u-1"),
            Value::from("u-1"),
            Value::Integer(1),
            Value::Integer(2),
        ]
    );

    // Without the library clause the SAME rowids hydrate BOTH tracks — the control
    // that shows the executed assertion above is the filter doing the work. It
    // keeps `user_id` identical to the filtered read above, so the ONLY difference
    // is the library clause (the annotation/bookmark joins are LEFT JOINs and never
    // exclude a row, but keeping them identical removes even that dimension).
    let (unfiltered_sql, unfiltered_args) =
        mediafile_repo::get_random_hydrate_sql(Some("u-1"), None, &[1, 2]);
    assert_eq!(
        unfiltered_sql.matches(" WHERE ").count(),
        1,
        "no filter, so the IN-list opens the only WHERE: {unfiltered_sql}"
    );
    let both = query_all(
        &store,
        &(unfiltered_sql, unfiltered_args),
        hydrate_mediafile_row,
    )
    .expect("the unfiltered phase-2 shape executes")
    .len();
    assert_eq!(both, 2, "the same rowids hydrate two rows without a filter");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A user with no `user_library` grant sees ZERO rows through the repo reads —
/// the library-filter clause genuinely narrows the statement rather than the test
/// vacuously passing (`sql_base_repository.go:261-282`'s resolved clause).
#[test]
fn repo_reads_return_nothing_for_an_ungranted_user() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-outsider", "outsider");
    seed_album(&store, lib, "al-1");
    mediafile_seed(&store, lib, "mf-1");

    let album_filter = album_lib_filter("u-outsider");
    let albums = lease(&store, |conn| {
        AlbumRepo::new(conn).get_all(None, false, Some(album_filter.clone()), vec![], 0, 0)
    })
    .expect("statement runs");
    assert!(albums.items.is_empty(), "no grant, no albums");
    assert!(matches!(
        lease(&store, |conn| AlbumRepo::new(conn).get(
            "al-1",
            None,
            false,
            Some(album_filter)
        )),
        Err(album_repo::AlbumError::NotFound)
    ));

    let track_filter = (
        "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from("u-outsider")],
    );
    let tracks = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, Some(track_filter.clone()), &[], 0, 0, true)
    })
    .expect("statement runs");
    assert!(tracks.items.is_empty(), "no grant, no tracks");
    assert!(matches!(
        lease(&store, |conn| MediaFileRepo::new(conn).get(
            "mf-1",
            None,
            Some(track_filter),
            true
        )),
        Err(mediafile_repo::MediaFileError::NotFound)
    ));
    // Same clause bound to a GRANTED user returns the row, proving the previous
    // assertions came from the filter and not from a broken statement.
    seed_user(&store, "u-in", "in");
    grant_library(&store, "u-in", lib);
    let granted = lease(&store, |conn| {
        MediaFileRepo::new(conn).get_all(
            None,
            Some((
                "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                    .to_string(),
                vec![Value::from("u-in")],
            )),
            &[],
            0,
            0,
            true,
        )
    })
    .expect("granted read");
    assert_eq!(granted.items.len(), 1);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A hydrated record re-renders byte-identical write legs, so the two new
/// read-path-only artwork fields never reach the map: `album_row` stays at 48
/// keys and `mediafile_row` at 66 (`docs/rust-port/next-work.md:775-779`).
#[test]
fn repo_hydrated_records_re_render_the_same_write_maps() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_user(&store, "u-1", "viewer");
    grant_library(&store, "u-1", lib);
    seed_album(&store, lib, "al-1");
    mediafile_seed(&store, lib, "mf-1");
    seed_annotation(&store, "u-1", "album", "al-1");
    seed_annotation(&store, "u-1", "media_file", "mf-1");
    seed_bookmark(&store, "u-1", "mf-1", 99);
    // Seed artwork so the hydrated records carry NON-zero image fields — the
    // case a zero-valued fixture could not prove.
    seed_state(&store, "al", "al-1", "albumhash");
    seed_state(&store, "mf", "mf-1", "ownhash");

    let album = lease(&store, |conn| {
        AlbumRepo::new(conn).get("al-1", Some("u-1"), false, None)
    })
    .expect("album read");
    assert_eq!(album.image.image_hash, "albumhash");
    let album_map = album_repo::album_row(&album);
    let keys: Vec<&str> = album_map.map().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 48, "45 base + 3 FTS keys");
    for forbidden in [
        "id",
        "library_path",
        "library_name",
        "annotation",
        "genres",
        "image",
        "image_hash",
        "image_absent",
        "blur_hash",
        "thumb_hash",
        "dominant_color",
        "image_width",
        "image_height",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "{forbidden} reached the album map: {keys:?}"
        );
    }
    assert_eq!(
        album_repo::put_sql("al-1", &album, stamp(T1), &[]).expect("renders"),
        album_repo::put_sql(
            "al-1",
            &AlbumRecord {
                image: nd_db::artwork_hydration::ItemImage::default(),
                ..album.clone()
            },
            stamp(T1),
            &[]
        )
        .expect("renders"),
        "a populated image changes no write leg"
    );

    let track = lease(&store, |conn| {
        MediaFileRepo::new(conn).get("mf-1", Some("u-1"), None, true)
    })
    .expect("media_file read");
    assert_eq!(track.image.image_hash, "ownhash");
    assert_eq!(track.album_image.image_hash, "albumhash");
    let track_map = mediafile_repo::mediafile_row(&track, false);
    let keys: Vec<&str> = track_map.map().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 66, "63 base + 3 FTS keys");
    for forbidden in [
        "id",
        "library_path",
        "library_name",
        "annotation",
        "bookmark_position",
        "genres",
        "image",
        "album_image",
        "image_hash",
        "image_absent",
        "album_image_hash",
        "blur_hash",
        "thumb_hash",
        "dominant_color",
        "image_width",
        "image_height",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "{forbidden} reached the media_file map: {keys:?}"
        );
    }
    assert_eq!(
        mediafile_repo::put_sql("mf-1", &track, stamp(T1), false, &[]).expect("renders"),
        mediafile_repo::put_sql(
            "mf-1",
            &MediaFileRecord {
                image: nd_db::artwork_hydration::ItemImage::default(),
                album_image: nd_db::artwork_hydration::ItemImage::default(),
                ..track.clone()
            },
            stamp(T1),
            false,
            &[]
        )
        .expect("renders"),
        "populated artwork fields change no write leg"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A seeded `item_artwork` + `artwork` row, in the kind-prefix spelling the
/// column actually holds (`artwork_repository.go:96`, `model/artwork_id.go:22`).
fn seed_state(store: &SQLStore, kind: &str, id: &str, hash: &str) {
    insert(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, updated_at) \
         VALUES (?, ?, 'primary', ?, 'folder', ?)",
        &[
            rusqlite::types::Value::Text(kind.to_string()),
            rusqlite::types::Value::Text(id.to_string()),
            rusqlite::types::Value::Text(hash.to_string()),
            rusqlite::types::Value::Text(T1.to_string()),
        ],
    );
}
