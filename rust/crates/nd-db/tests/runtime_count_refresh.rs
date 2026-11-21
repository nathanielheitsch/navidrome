//! DB-17 — the EXECUTED narrow count refresh: album/artist `RefreshPlayCounts`,
//! tag `UpdateCounts`, and library `RefreshStats`, each run on the CALLER's typed
//! transaction over the fresh production schema.
//!
//! Go truth under test, SOURCE-VERIFIED at HEAD `f3bb81b2`:
//!   * `persistence/album_repository.go:411-428` — the CTE `sum(play_count)` /
//!     `max(play_date)` over `media_file JOIN annotation`, `where total_play_count > 0`,
//!     upsert `item_type='album'` setting ONLY `play_count`/`play_date`; returns
//!     `executeSQL`'s `RowsAffected` (`:427`).
//!   * `persistence/artist_repository.go:441-460` — the same shape folded over
//!     `json_tree(participants, '$.artist')` (`atom is not null and key = 'id'`),
//!     `item_type='artist'`.
//!   * `persistence/tag_repository.go:54-76` — the two templated `library_tag` upserts
//!     (`album`, then `media_file`), genre projection only, `DO UPDATE SET
//!     <table>_count = excluded.<table>_count`; the affected count goes only to
//!     `log.Debug` (`:67`) and errors wrap as `updating %s library tag counts: %w` (`:71`).
//!   * `persistence/library_repository.go:196-251` — eight aggregate probes under
//!     `run.Parallel` (`:201`), then the totals UPDATE with `Set("updated_at", time.Now())`
//!     (`:247`); the affected count is discarded (`:249`).
//!   * `persistence/sql_base_repository.go:336-353` — `executeSQL` returns `RowsAffected`,
//!     which is the Rust helpers' return shape: statement-level, never entity-level.
//!   * Go TRIGGERS, NOT ported here (this lane ships primitives with no production caller):
//!     `scanner/phase_3_refresh_albums.go:138`/`:150` inside `WithTxRetry` (`:136`/`:148`),
//!     `changesDetected`-gated at `:129`; `scanner/scanner.go:316` (tag counts) and
//!     `:344` (library stats) inside `runUpdateLibraries`' tx.
//!
//! **Pin 6f — the `play_date` update condition, SOURCE-VERIFIED.** Go's album and artist
//! conflict arms carry a bare `play_date = excluded.play_date` with NO date comparison,
//! unlike `IncPlayCount`'s `play_date = max(ifnull(play_date,''), ?)`
//! (`sql_annotations.go:163-165`). Two consequences, both pinned here rather than assumed:
//! a refresh can move `play_date` BACKWARDS, and a replay of unchanged data is
//! value-idempotent but NOT row-count-idempotent (SQLite's `ON CONFLICT DO UPDATE` reports
//! a change even when the new value equals the stored one). See
//! `playcount_refresh_replays_the_same_values_without_advancing_play_date`.
//!
//! **Pin 4 — NULL aggregates, SOURCE-VERIFIED.** None of the four refresh statements
//! carries a `coalesce` (`album_repository.go:411-428`, `artist_repository.go:441-460`,
//! `tag_repository.go:54-76`). An empty aggregate yields NULL, which `total_play_count > 0`
//! cannot match (`NULL > 0` is NULL), so NOTHING is inserted — neither a `0` row nor a NULL
//! row. The library probes are the only place a coalesce-equivalent exists, and it is
//! `ifnull(sum(…),0)` INSIDE Go's own SQL (`library_repository.go:221-231`), which the
//! renderer already carries; the update then binds it as a real value, so the stored cell
//! is never NULL.
//!
//! Writes go through the production `nd_db` API: libraries through DB-05's `put_in_tx`,
//! artists through DB-08's `put_artist_in_tx` + `add_artist_in_tx`, albums through DB-07's
//! `put_album_in_tx`, folders through DB-09's `put_folder_in_tx`, tags through DB-06's
//! `add_in_tx`, and the four refreshes through DB-17's `*_in_tx` helpers inside
//! `try_with_tx_sync`. `media_file` rows and `annotation` rows are hand-INSERTed (the
//! `runtime_move_persistence.rs:419` `media_row` precedent: a media file and its annotation
//! must exist BEFORE the refresh feeds on them, and `annotation` has no write primitive in
//! this lane); their `tags`/`participants` JSON comes from the PRODUCTION marshallers
//! (`sql_tags::marshal_tags`, `sql_participations::marshal_participants`), not hand-written
//! JSON. Read-back goes through the production getters — `AlbumRepo::get`,
//! `ArtistRepo::get`, `LibraryRepo::get`, `TagRepo::get_all_plain` — with raw SQL reserved
//! for the annotation row and the per-library `library_tag` rows, which have no getter.
//!
//! No DB-18 symbol is exercised (`refresh_stats_ids_sql`, `refresh_stats_batch_sql`,
//! `refresh_stats_cleanup_sql`, `mark_orphans_missing`, `REFRESH_STATS_BATCH_SIZE`, and
//! `library_artist.stats` is asserted to stay at its DDL default), and no `purge_unused_sql`
//! (DB-20).

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    put_album_in_tx, refresh_play_counts_in_tx as refresh_album_play_counts_in_tx, AlbumError,
    AlbumRecord, AlbumRepo, AlbumWriteError,
};
use nd_db::artist_repo::{
    put_artist_in_tx, refresh_play_counts_in_tx as refresh_artist_play_counts_in_tx, ArtistRecord,
    ArtistRepo, ArtistWriteError,
};
use nd_db::folder_repo::{put_folder_in_tx, FolderRecord, FolderWriteError};
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx as put_library_in_tx, refresh_library_totals_in_tx, LibraryColumn,
    LibraryError, LibraryRecord, LibraryRepo, LibraryWriteError, LibraryWriteResult,
    RefreshStatsTotals, PROBE_ALBUMS, PROBE_ARTISTS, PROBE_DURATION, PROBE_FILES, PROBE_FOLDERS,
    PROBE_MISSING, PROBE_SIZE, PROBE_SONGS,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_annotations::ANNOTATION_TABLE;
use nd_db::sql_participations::{marshal_participants, Participant, Participants, Role};
use nd_db::sql_tags::{marshal_tags, DbTags};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::tag_repo::{add_in_tx, update_counts_in_tx, Tag, TagRepo};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clock (DB-02): every stamp here comes from here, never the
/// wall clock.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, min, s, nanos)
            .unwrap(),
    )
}

/// Annotation play dates: deliberately not monotonic with media-file insertion order, so
/// a `max()` that picked the wrong arm would disagree with the expected value.
fn play_stamp(offset_seconds: i64) -> DateTime<Utc> {
    at(2024, 6, 1, 12, 0, 0, 0) + chrono::Duration::seconds(offset_seconds)
}

/// The seed stamp for libraries/artists/albums/folders.
fn seed_stamp() -> DateTime<Utc> {
    at(2024, 5, 1, 9, 8, 7, 111_000_000)
}

/// The refresh stamp — pin 2's subject: this is what must reach `library.updated_at`.
fn refresh_stamp() -> DateTime<Utc> {
    at(2025, 2, 3, 4, 5, 6, 789_000_000)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, pooled (2 handles, as in every sibling lane),
/// freshly migrated, `_foreign_keys=on` (Go's default DSN parameter, `consts/consts.go:14`).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-count-refresh-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("cannot remove temp db: {e}"));
}

/// Run one closure on a leased connection.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement — used ONLY for `media_file`/`annotation` rows, `user` rows,
/// `user_library` grants, and the two deliberately-damaged cells. No row a production
/// write primitive can produce is hand-written here.
fn fixture_insert(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}
fn real(value: f64) -> SqlValue {
    SqlValue::Real(value)
}
fn null() -> SqlValue {
    SqlValue::Null
}

/// One scalar probe on a FRESH lease (state after a commit or a rollback).
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// `user` row (`schema_ddl.rs:57`): `created_at`/`updated_at` are `not null`.
fn seed_user(store: &SQLStore, id: &str) {
    fixture_insert(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            text(&time::to_text(&at(2024, 1, 1, 0, 0, 0, 0))),
            text(&time::to_text(&at(2024, 1, 2, 0, 0, 0, 0))),
        ],
    );
}

/// `user_library` grant — a fixture write, so it cannot be a side effect of
/// `put_in_tx`'s admin-assignment leg.
fn seed_access(store: &SQLStore, user_id: &str, library_id: i64) {
    fixture_insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[text(user_id), integer(library_id)],
    );
}

/// A library record whose every field carries a DIFFERENT value (DB-05's fixture shape).
/// The eight totals start NON-zero and distinct, so a refresh that writes nothing — or
/// writes the wrong probe into the wrong column — cannot pass by coincidence.
fn library_record(id: i64, tag: &str) -> LibraryRecord {
    LibraryRecord {
        id,
        name: format!("name-{tag}"),
        path: format!("/path-{tag}"),
        remote_path: format!("remote-{tag}"),
        last_scan_at: at(2024, 3, 4, 5, 6, 7, 111_000_000),
        last_scan_started_at: at(2024, 3, 4, 5, 6, 8, 222_000_000),
        full_scan_in_progress: true,
        updated_at: at(2024, 3, 4, 5, 6, 9, 333_000_000),
        created_at: at(2024, 3, 4, 5, 6, 10, 444_000_000),
        total_songs: 11,
        total_albums: 12,
        total_artists: 13,
        total_folders: 14,
        total_files: 15,
        total_missing_files: 16,
        total_size: 17,
        total_duration: 18.5,
        default_new_users: true,
        pid_album: format!("pidalbum-{tag}"),
        pid_track: format!("pidtrack-{tag}"),
        scanned_pid_album: format!("scannedalbum-{tag}"),
        scanned_pid_track: format!("scannedtrack-{tag}"),
    }
}

/// Commit one library through DB-05's production `Put`; returns its id. The autoincrement
/// arm yields 1, 2, … in insert order, so the labels fix the ids.
fn put_library(store: &SQLStore, tag: &str) -> i64 {
    store
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_library_in_tx(
                tx,
                &library_record(0, tag),
                &seed_stamp(),
                &[LibraryColumn::Path],
            )
        })
        .unwrap_or_else(|e| panic!("put_in_tx({tag}) failed: {e}"))
        .id
}

/// Commit one artist through DB-08's production `Put`, then link it to each library
/// through DB-05's `add_artist_in_tx`, so `ArtistRepo::get`'s INNER join can see it and
/// the `total_artists` probe has real junction rows.
fn put_artist(store: &SQLStore, id: &str, library_ids: &[i64], missing: bool) {
    let record = ArtistRecord {
        id: id.to_string(),
        name: format!("Name-{id}"),
        sort_artist_name: format!("Sort-{id}"),
        order_artist_name: format!("Order-{id}"),
        mbz_artist_id: format!("mbz-{id}"),
        biography: format!("Bio-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        similar_artists: "[]".to_string(),
        external_info_updated_at: Some(seed_stamp()),
        missing,
        uploaded_image: format!("upload-{id}"),
        created_at: Some(seed_stamp()),
        updated_at: Some(seed_stamp()),
        ..ArtistRecord::default()
    };
    store
        .try_with_tx_sync::<nd_db::artist_repo::ArtistWriteResult, ArtistWriteError>(|tx| {
            put_artist_in_tx(tx, &record, &seed_stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("put_artist_in_tx({id}) failed: {e}"));
    for lib in library_ids {
        store
            .try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, *lib, id))
            .unwrap_or_else(|e| panic!("add_artist_in_tx({lib}, {id}) failed: {e}"));
    }
}

/// The production album record. Every persisted field is distinct, so a swapped bind list
/// cannot land on a value that happens to satisfy another column. `song_count`/`size`/
/// `duration` carry the exact values the library probes are expected to sum.
#[allow(clippy::too_many_arguments)] // mirrors the eight axes the album fixture varies
fn album_record(
    id: &str,
    library_id: i64,
    song_count: i32,
    size: i64,
    duration: f64,
    missing: bool,
    genre: Option<&str>,
    folder_ids: &[String],
) -> AlbumRecord {
    let mut tags = DbTags::new();
    if let Some(genre) = genre {
        tags.insert("genre".to_string(), vec![genre.to_string()]);
    }
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: "ar-main".to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    AlbumRecord {
        id: id.to_string(),
        library_id,
        name: format!("Album-{id}"),
        embed_art_path: format!("embed-{id}"),
        album_artist: "Main Act".to_string(),
        album_artist_id: "ar-main".to_string(),
        min_year: 1979,
        max_year: 1981,
        date: format!("date-{id}"),
        min_original_year: 1978,
        max_original_year: 1977,
        original_date: format!("orig-{id}"),
        release_date: format!("rel-{id}"),
        compilation: false,
        comment: format!("cmt-{id}"),
        song_count,
        duration,
        size,
        discs: None,
        sort_album_name: format!("sort-{id}"),
        sort_album_artist_name: format!("sortaa-{id}"),
        order_album_name: format!("order-{id}"),
        order_album_artist_name: format!("orderaa-{id}"),
        catalog_num: format!("cat-{id}"),
        mbz_album_id: format!("mbz-{id}"),
        mbz_album_artist_id: format!("mbzaa-{id}"),
        mbz_album_type: format!("mbzt-{id}"),
        mbz_album_comment: format!("mbzc-{id}"),
        mbz_release_group_id: format!("mbzrg-{id}"),
        folder_ids: Some(folder_ids.to_vec()),
        explicit_status: format!("exp-{id}"),
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        description: format!("desc-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        external_info_updated_at: None,
        genre: genre.unwrap_or("").to_string(),
        tags,
        participants,
        participant_names: Vec::new(),
        missing,
        imported_at: seed_stamp(),
        created_at: Some(seed_stamp()),
        updated_at: Some(seed_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
    }
}

/// Commit one album through DB-07's production `Put`.
fn put_album(store: &SQLStore, record: &AlbumRecord) {
    store
        .try_with_tx_sync::<nd_db::album_repo::AlbumWriteResult, AlbumWriteError>(|tx| {
            put_album_in_tx(tx, record, &seed_stamp())
        })
        .unwrap_or_else(|e| panic!("put_album_in_tx({}) failed: {e}", record.id));
}

/// Commit one folder through DB-09's production `Put` with the exact
/// `num_audio_files`/`num_playlists`/`image_files` values the two folder probes read.
fn put_folder(
    store: &SQLStore,
    id: &str,
    library_id: i64,
    num_audio_files: i64,
    num_playlists: i64,
    image_files: usize,
    missing: bool,
) {
    let record = FolderRecord {
        id: id.to_string(),
        library_id,
        path: format!("/path-folder-{id}"),
        name: format!("Folder-{id}"),
        parent_id: String::new(),
        num_audio_files,
        num_playlists,
        image_files: Some(
            (0..image_files)
                .map(|n| format!("cover-{id}-{n}.jpg"))
                .collect(),
        ),
        images_updated_at: seed_stamp(),
        hash: format!("hash-{id}"),
        missing,
        updated_at: seed_stamp(),
        created_at: Some(seed_stamp()),
        library_path: String::new(),
    };
    store
        .try_with_tx_sync::<nd_db::folder_repo::FolderWriteResult, FolderWriteError>(|tx| {
            put_folder_in_tx(tx, &record, &seed_stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("put_folder_in_tx({id}) failed: {e}"));
}

/// `media_file` fixture (`runtime_move_persistence.rs:419` precedent): the row exists
/// BEFORE the annotation that feeds the aggregate. `tags`/`participants` are produced by
/// the PRODUCTION marshallers, so the JSON the refresh statements' `json_tree` folds read
/// is the writer's own shape. `artist_ids` is the `$.artist` fold, and the row's own
/// `artist_id` column carries only the first entry — which is exactly what makes the
/// participants-fold control non-vacuous.
fn media_row(
    store: &SQLStore,
    id: &str,
    library_id: i64,
    album_id: &str,
    artist_ids: &[&str],
    genre: Option<&str>,
    missing: bool,
) {
    let mut tags = DbTags::new();
    if let Some(genre) = genre {
        tags.insert("genre".to_string(), vec![genre.to_string()]);
    }
    let mut participants = Participants::new();
    participants.insert(
        Role::from("artist"),
        artist_ids
            .iter()
            .map(|artist| Participant {
                id: (*artist).to_string(),
                name: format!("Name-{artist}"),
                sub_role: String::new(),
            })
            .collect(),
    );
    fixture_insert(
        store,
        "INSERT INTO media_file (id, library_id, path, title, album, album_id, artist, artist_id, \
         album_artist, album_artist_id, created_at, updated_at, birth_time, missing, folder_id, \
         tags, participants, duration, size) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(library_id),
            text(&format!("/path-media-{id}")),
            text(&format!("Title-{id}")),
            text(&format!("Album-{album_id}")),
            text(album_id),
            text(&format!("TrackArtist-{id}")),
            text(artist_ids.first().copied().unwrap_or("ar-main")),
            text("Main Act"),
            text("ar-main"),
            text(&time::to_text(&seed_stamp())),
            text(&time::to_text(&seed_stamp())),
            text(&time::to_text(&seed_stamp())),
            integer(i64::from(missing)),
            text(&format!("fold-{id}")),
            text(&marshal_tags(tags)),
            text(&marshal_participants(&participants)),
            real(201.25),
            integer(2_222),
        ],
    );
}

/// One annotation row with a NON-zero play count, an explicit play date, and rating/star
/// values the refresh must NOT disturb. `item_type` is Go's table name, as stored.
#[allow(clippy::too_many_arguments)] // mirrors annotation's play/rating/star columns
fn seed_annotation(
    store: &SQLStore,
    user_id: &str,
    item_id: &str,
    item_type: &str,
    play_count: i64,
    play_date: Option<DateTime<Utc>>,
    rating: i64,
    starred: bool,
) {
    fixture_insert(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, play_date, rating, \
         starred, starred_at, rated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(user_id),
            text(item_id),
            text(item_type),
            integer(play_count),
            play_date.map_or_else(null, |d| text(&time::to_text(&d))),
            integer(rating),
            integer(i64::from(starred)),
            text(&time::to_text(&at(2024, 3, 3, 3, 3, 3, 0))),
            text(&time::to_text(&at(2024, 4, 4, 4, 4, 4, 0))),
        ],
    );
}

/// The stored annotation row for one `(user, item, type)`. `None` = no row at all.
type AnnotationState = (
    i64,
    Option<String>,
    i64,
    bool,
    Option<String>,
    Option<String>,
);

fn annotation_state(
    store: &SQLStore,
    user_id: &str,
    item_id: &str,
    item_type: &str,
) -> Option<AnnotationState> {
    let sql = annotation_select();
    lease(store, |conn| {
        conn.query_row(&sql, [user_id, item_id, item_type], map_annotation)
            .ok()
    })
}

fn annotation_select() -> String {
    format!(
        "SELECT play_count, play_date, rating, starred, starred_at, rated_at \
         FROM {ANNOTATION_TABLE} WHERE user_id = ? AND item_id = ? AND item_type = ?"
    )
}

fn map_annotation(row: &rusqlite::Row<'_>) -> rusqlite::Result<AnnotationState> {
    Ok((
        row.get(0)?,
        row.get::<_, Option<String>>(1)?,
        row.get(2)?,
        row.get::<_, bool>(3)?,
        row.get::<_, Option<String>>(4)?,
        row.get::<_, Option<String>>(5)?,
    ))
}

/// The same read INSIDE a transaction (in-flight visibility). A `Transaction` derefs to
/// `&Connection`, so this is the identical statement, not a second one.
fn annotation_state_tx(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
    item_id: &str,
    item_type: &str,
) -> Option<AnnotationState> {
    tx.query_row(
        &annotation_select(),
        [user_id, item_id, item_type],
        map_annotation,
    )
    .ok()
}

/// `typeof()` of one cell — the storage-class check pin 4's zero-not-NULL rule needs.
fn typeof_column(store: &SQLStore, table: &str, column: &str, predicate: &str) -> String {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT typeof({column}) FROM {table} WHERE {predicate}"),
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} typeof: {e}"))
    })
}

/// The production album read (`album_repository.go:234-243` via `AlbumRepo::get`).
fn album_lib_filter(user_id: &str) -> (String, Vec<Value>) {
    (
        "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
            .to_string(),
        vec![Value::from(user_id)],
    )
}

fn read_album(store: &SQLStore, id: &str, user_id: &str) -> AlbumRecord {
    lease(store, |conn| {
        AlbumRepo::new(conn)
            .get(id, Some(user_id), false, Some(album_lib_filter(user_id)))
            .unwrap_or_else(|e| panic!("AlbumRepo::get({id}) for {user_id}: {e}"))
    })
}

fn read_album_attempt(
    store: &SQLStore,
    id: &str,
    user_id: &str,
) -> Result<AlbumRecord, AlbumError> {
    lease(store, |conn| {
        AlbumRepo::new(conn).get(id, Some(user_id), false, Some(album_lib_filter(user_id)))
    })
}

/// The production artist read (`artist_repository.go:255-267` via `ArtistRepo::get`), with
/// the logged user's annotation join so `record.annotation` is the derived annotation.
fn read_artist(store: &SQLStore, id: &str, user_id: &str) -> ArtistRecord {
    lease(store, |conn| {
        ArtistRepo::new(conn)
            .get(id, &Some(user_id.to_string()), Some(user_id))
            .unwrap_or_else(|e| panic!("ArtistRepo::get({id}) for {user_id}: {e}"))
    })
}

/// The production library read (`library_repository.go:35-40` via `LibraryRepo::get`).
fn read_library(store: &SQLStore, id: i64) -> LibraryRecord {
    lease(store, |conn| {
        LibraryRepo::new(conn)
            .get(id)
            .unwrap_or_else(|e| panic!("LibraryRepo::get({id}): {e}"))
    })
}

fn read_library_attempt(store: &SQLStore, id: i64) -> Result<LibraryRecord, LibraryError> {
    lease(store, |conn| LibraryRepo::new(conn).get(id))
}

/// The production tag read (`tag_repository.go:78-83` via `TagRepo::get_all_plain`), as
/// `value/albums/songs` cells, sorted. `COALESCE(SUM(…))` over the user's libraries.
fn tag_cells(store: &SQLStore, user_id: Option<&str>) -> Vec<String> {
    let tags = lease(store, |conn| {
        TagRepo::new(conn)
            .get_all_plain("genre", user_id)
            .unwrap_or_else(|e| panic!("TagRepo::get_all_plain(genre): {e}"))
    });
    let mut cells: Vec<String> = tags
        .iter()
        .map(|t: &Tag| format!("{}/{}/{}", t.tag_value, t.album_count, t.song_count))
        .collect();
    cells.sort();
    cells
}

/// One `library_tag` link row's counts — no getter exposes per-library rows, because
/// `TagRepo` aggregates across the user's libraries. `None` = no link row exists.
fn link_counts(store: &SQLStore, tag_id: &str, library_id: i64) -> Option<(i64, i64)> {
    lease(store, |conn| {
        conn.query_row(
            "SELECT album_count, media_file_count FROM library_tag WHERE tag_id = ? AND library_id = ?",
            rusqlite::params![tag_id, library_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
    })
}

/// The same link-row read INSIDE a transaction, for the in-flight visibility checks the
/// swallowed-error regressions need. A `Transaction` derefs to `&Connection`.
fn link_counts_tx(
    tx: &rusqlite::Transaction<'_>,
    tag_id: &str,
    library_id: i64,
) -> Option<(i64, i64)> {
    tx.query_row(
        "SELECT album_count, media_file_count FROM library_tag WHERE tag_id = ? AND library_id = ?",
        rusqlite::params![tag_id, library_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

/// All `library_tag` rows as `tag/lib/albums/songs`, sorted by id — the census view.
fn link_census(store: &SQLStore) -> Vec<(String, i64, i64, i64)> {
    lease(store, |conn| {
        let mut stmt = conn
            .prepare("SELECT tag_id, library_id, album_count, media_file_count FROM library_tag ORDER BY tag_id, library_id")
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    })
}

/// Genre dictionary tag, id from Go's `model.NewTag` equivalent.
fn genre_tag(value: &str) -> Tag {
    Tag {
        id: nd_id::new_tag_id("genre", value),
        tag_name: "genre".to_string(),
        tag_value: value.to_string(),
        album_count: 0,
        song_count: 0,
    }
}

/// Commit one production `add_in_tx` call through the typed seam — the only way dictionary
/// and link rows are written in this file.
fn add_tags_committed(store: &SQLStore, library_id: i64, tags: &[Tag]) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| add_in_tx(tx, library_id, tags))
        .unwrap_or_else(|e| panic!("add_in_tx(library {library_id}) failed: {e}"));
}

/// Commit one production refresh through the typed seam exactly as a caller would (Go wraps
/// each in `WithTxRetry`; the primitives take the caller's `&Transaction`). All four DB-17
/// helpers share the `DbOperationError<Infallible>` error type, so one callback can compose
/// them without conversion.
fn committed<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, DbOperationError<Infallible>>,
) -> T {
    store
        .try_with_tx_sync::<T, Infallible>(call)
        .unwrap_or_else(|e| panic!("refresh transaction failed: {e}"))
}

/// The extended SQLite code when the failure carries one — proof the seam kept the
/// concrete cause rather than a stringified Go-style wrapper (pin 6, rule E1).
fn extended_code(err: &SqliteError) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// A real mid-transaction SQLite failure for rollback injection: `annotation.user_id`
/// references `user(id)` (`schema_ddl.rs:59`) and every store here is `_foreign_keys=on`,
/// so a row for a non-existent user aborts with `SQLITE_CONSTRAINT_FOREIGNKEY`. This
/// replaces a fabricated error value: the rollback is triggered by SQLite itself
/// (`runtime_annotation_transfers.rs:104` precedent).
fn inject_fk_failure(tx: &rusqlite::Transaction<'_>) -> DbOperationError<Infallible> {
    tx.execute(
        "INSERT INTO annotation(user_id,item_id,item_type) VALUES('ghost-user','poison','album')",
        [],
    )
    .err()
    .map(DbOperationError::sqlite)
    .expect("FK violation on a missing user must fail")
}

// ---------------------------------------------------------------------------
// Pin 1 — zero bound arguments in every refresh statement.
// ---------------------------------------------------------------------------

#[test]
fn all_four_refresh_statements_carry_zero_bound_args() {
    // Arrange/Act: the exact renderers the four executed helpers run.
    let album = nd_db::album_repo::refresh_play_counts_sql();
    let artist = nd_db::artist_repo::refresh_play_counts_sql();
    let tags = nd_db::tag_repo::update_counts_sql();
    let update = nd_db::library_repo::refresh_stats_update_sql(&RefreshStatsTotals {
        id: 1,
        songs: 0,
        albums: 0,
        artists: 0,
        folders: 0,
        files: 0,
        missing: 0,
        size: 0,
        duration: 0.0,
        updated_at: refresh_stamp(),
    });

    // Assert: no chunking and no variable-number exposure anywhere in DB-17. The
    // 4×1000-param concern belongs to DB-18's batch builder, not any statement here.
    assert!(album.1.is_empty(), "album refresh binds {:?}", album.1);
    assert!(artist.1.is_empty(), "artist refresh binds {:?}", artist.1);
    assert_eq!(tags.len(), 2, "Go loops over album then media_file (`:66`)");
    for statement in &tags {
        assert!(
            statement.1.is_empty(),
            "tag count statement binds {:?}",
            statement.1
        );
    }
    assert_eq!(
        update.1.len(),
        10,
        "eight totals + caller stamp + the WHERE id"
    );
    assert_eq!(
        update.0.matches('?').count(),
        10,
        "one placeholder per bound value, no more"
    );
    // The caller's stamp travels as a bound value (pin 2), never as SQL text.
    assert!(
        !update.0.contains(&time::to_text(&refresh_stamp())),
        "the stamp must be bound, not interpolated into the statement"
    );
    // The eight probes' own arg lists are per-library and constant-length (2-3 values),
    // so nothing in DB-17 scales with the number of rows.
    for probe in nd_db::library_repo::refresh_stats_probes_sql(7) {
        assert!(probe.1.len() <= 3, "probe binds {:?}", probe.1);
    }
}

#[test]
fn probe_indices_pair_each_aggregate_with_its_own_predicate() {
    // The executed helper reads probes by these constants; this pins the constants against
    // the renderer's own text so a re-ordered pair is caught at the source too.
    let probes = nd_db::library_repo::refresh_stats_probes_sql(7);
    assert_eq!(probes.len(), 8);
    assert_eq!(
        (PROBE_SONGS, PROBE_ALBUMS, PROBE_ARTISTS, PROBE_FOLDERS),
        (0, 1, 2, 3)
    );
    assert_eq!(
        (PROBE_FILES, PROBE_MISSING, PROBE_SIZE, PROBE_DURATION),
        (4, 5, 6, 7)
    );
    assert!(probes[PROBE_SONGS].0.contains("FROM media_file"));
    assert!(probes[PROBE_SONGS].0.contains("missing = ?"));
    assert_eq!(
        extended_probe_sql(&probes[PROBE_SONGS].0),
        extended_probe_sql(&probes[PROBE_MISSING].0),
        "songs and missing share the statement and differ only in the bound `missing` value"
    );
    assert!(probes[PROBE_ALBUMS].0.contains("FROM album"));
    assert!(probes[PROBE_ARTISTS].0.contains("library_artist la"));
    assert!(probes[PROBE_ARTISTS].0.contains("a.missing = ?"));
    assert!(probes[PROBE_FOLDERS].0.contains("num_audio_files > ?"));
    assert!(probes[PROBE_FILES]
        .0
        .contains("json_array_length(image_files)"));
    assert!(probes[PROBE_FILES].0.contains("ifnull("));
    assert!(
        !probes[PROBE_FILES].0.contains("num_audio_files > ?"),
        "files has no > 0 filter"
    );
    assert!(probes[PROBE_SIZE].0.contains("ifnull(sum(size),0)"));
    assert!(probes[PROBE_DURATION].0.contains("ifnull(sum(duration),0)"));
}

/// The statement text with the `missing` literal normalized, so the songs/missing pair can
/// be compared as the SAME statement.
fn extended_probe_sql(sql: &str) -> String {
    sql.replace(" WHERE library_id = ? AND missing = ?", "|")
}

// ---------------------------------------------------------------------------
// Pin 7b — rating/star/stamps survive: only play_count/play_date are written.
// ---------------------------------------------------------------------------

#[test]
fn playcount_refresh_preserves_other_annotation_fields() {
    let (db, path) = fresh_store("preserve");
    let lib = put_library(&db, "preserve");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_artist(&db, "ar-fold", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 2, 4096, 120.5, false, None, &[]),
    );
    media_row(
        &db,
        "mf-1",
        lib,
        "al-1",
        &["ar-main", "ar-fold"],
        None,
        false,
    );
    media_row(&db, "mf-2", lib, "al-1", &["ar-main"], None, false);
    // mf-1: 5 plays, newest date; mf-2: 3 plays, older date.
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        5,
        Some(play_stamp(86_400)),
        4,
        true,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-2",
        "media_file",
        3,
        Some(play_stamp(0)),
        0,
        false,
    );
    // Pre-existing album + artist annotations: stale play values AND non-zero rating/star
    // with their own stamps. A whole-row replacement clobbers those.
    seed_annotation(
        &db,
        "u1",
        "al-1",
        "album",
        999,
        Some(play_stamp(999_999)),
        3,
        true,
    );
    seed_annotation(
        &db,
        "u1",
        "ar-main",
        "artist",
        999,
        Some(play_stamp(999_999)),
        5,
        true,
    );

    // Act
    let album_rows = committed(&db, refresh_album_play_counts_in_tx);
    let artist_rows = committed(&db, refresh_artist_play_counts_in_tx);

    // Album aggregate = 5+3 plays; date = max = the newer media date.
    let album = annotation_state(&db, "u1", "al-1", "album").expect("album annotation upserted");
    assert_eq!(album.0, 8, "album play_count = sum over its media files");
    assert_eq!(
        album.1,
        Some(time::to_text(&play_stamp(86_400))),
        "play_date = max"
    );
    // The surviving columns (pin 7b's whole-row-replacement control).
    assert_eq!(album.2, 3, "album annotation rating survives");
    assert!(album.3, "album annotation starred survives");
    assert_eq!(
        album.4,
        Some(time::to_text(&at(2024, 3, 3, 3, 3, 3, 0))),
        "starred_at survives"
    );
    assert_eq!(
        album.5,
        Some(time::to_text(&at(2024, 4, 4, 4, 4, 4, 0))),
        "rated_at survives"
    );

    // The artist fold: mf-1 credits BOTH artists; mf-2 only ar-main.
    let fold = annotation_state(&db, "u1", "ar-fold", "artist").expect("folded artist created");
    assert_eq!(fold.0, 5, "ar-fold gets mf-1's plays only");
    assert_eq!(
        fold.1,
        Some(time::to_text(&play_stamp(86_400))),
        "folded artist max date"
    );
    assert_eq!(
        fold.2, 0,
        "a fresh annotation row gets the DDL rating default 0"
    );
    assert!(!fold.3, "a fresh annotation row is unstarred");
    let main = annotation_state(&db, "u1", "ar-main", "artist").expect("artist annotation");
    assert_eq!(main.0, 8, "ar-main gets 5+3 across both media files");
    assert_eq!(main.2, 5, "artist annotation rating survives");
    assert!(main.3, "artist annotation starred survives");
    assert_eq!(
        main.4,
        Some(time::to_text(&at(2024, 3, 3, 3, 3, 3, 0))),
        "artist starred_at survives"
    );
    assert_eq!(
        main.5,
        Some(time::to_text(&at(2024, 4, 4, 4, 4, 4, 0))),
        "artist rated_at survives"
    );

    // The media-file annotations that fed the aggregate are untouched by either statement.
    let mf = annotation_state(&db, "u1", "mf-1", "media_file").expect("media annotation intact");
    assert_eq!(mf.0, 5, "media annotation play_count is NOT re-aggregated");
    assert_eq!(mf.2, 4, "media annotation rating intact");

    // Pin 5: statement-level counts, not entity counts — 1 album update; 1 artist update
    // plus 1 artist insert.
    assert_eq!(album_rows, 1, "album statement RowsAffected");
    assert_eq!(
        artist_rows, 2,
        "artist statement RowsAffected (1 update + 1 insert)"
    );

    // Production read-back through the getters: the annotation the album/artist selects'
    // LEFT JOIN derives.
    let record = read_album(&db, "al-1", "u1");
    assert_eq!(record.annotation.play_count, 8, "AlbumRepo::get play_count");
    assert_eq!(
        record.annotation.rating, 3,
        "AlbumRepo::get rating survives"
    );
    assert!(record.annotation.starred, "AlbumRepo::get starred survives");
    let artist = read_artist(&db, "ar-main", "u1");
    assert_eq!(
        artist.annotation.play_count, 8,
        "ArtistRepo::get play_count"
    );
    assert_eq!(
        artist.annotation.rating, 5,
        "ArtistRepo::get rating survives"
    );
    assert!(
        artist.annotation.starred,
        "ArtistRepo::get starred survives"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pin 4 — empty/NULL aggregates: 0-or-nothing, never NULL.
// ---------------------------------------------------------------------------

#[test]
fn album_playcount_refresh_zero_population_is_zero_not_null() {
    let (db, path) = fresh_store("empty-album");
    let lib = put_library(&db, "empty");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.0, false, None, &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], None, false);
    // A media annotation whose play_count is NULL: `sum(play_count)` over it is NULL and
    // `NULL > 0` is NULL, so Go's filter excludes the group entirely.
    seed_annotation(&db, "u1", "mf-1", "media_file", 0, None, 0, false);
    fixture_insert(
        &db,
        "UPDATE annotation SET play_count = NULL WHERE item_id = 'mf-1'",
        &[],
    );

    let rows = committed(&db, refresh_album_play_counts_in_tx);

    // Nothing is inserted: not a `0` row and not a NULL row.
    assert_eq!(
        rows, 0,
        "0 affected rows on an all-NULL play_count population"
    );
    assert!(
        annotation_state(&db, "u1", "al-1", "album").is_none(),
        "Go's `where total_play_count > 0` cannot match NULL, so no row is written"
    );

    // Now a real play_count with NULL dates: the count lands as INTEGER, the date stays
    // NULL (max over NULL is NULL, and there is no coalesce in Go's statement).
    fixture_insert(
        &db,
        "UPDATE annotation SET play_count = 4 WHERE item_id = 'mf-1'",
        &[],
    );
    let rows = committed(&db, refresh_album_play_counts_in_tx);
    assert_eq!(rows, 1, "the group now passes the `> 0` filter");
    let album = annotation_state(&db, "u1", "al-1", "album").expect("count-driven insert ran");
    assert_eq!(album.0, 4, "sum(play_count) with NULL dates");
    assert_eq!(
        album.1, None,
        "max(play_date) over NULL dates is NULL, not an error"
    );
    assert_eq!(
        typeof_column(&db, ANNOTATION_TABLE, "play_count", "item_id = 'al-1'"),
        "integer",
        "the aggregate is written as an integer, never NULL"
    );
    // The production getter reads the same row as a zero play_date rather than failing.
    assert_eq!(read_album(&db, "al-1", "u1").annotation.play_count, 4);
    assert_eq!(read_album(&db, "al-1", "u1").annotation.play_date, None);

    cleanup(db, path);
}

#[test]
fn artist_playcount_refresh_multi_annotation_population() {
    let (db, path) = fresh_store("artist-pop");
    let lib = put_library(&db, "artist");
    seed_user(&db, "u1");
    seed_user(&db, "u2");
    seed_access(&db, "u1", lib);
    seed_access(&db, "u2", lib);
    put_artist(&db, "ar-a", &[lib], false);
    put_artist(&db, "ar-b", &[lib], false);
    put_artist(&db, "ar-c", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 3, 100, 10.0, false, None, &[]),
    );
    // ar-a on all three rows, ar-b on two, ar-c on one. Each row's own `artist_id` column
    // holds only its FIRST participant, so ar-c's existence proves the fold ran over the
    // `$.artist` participants JSON and not over `media_file.artist_id`.
    media_row(&db, "mf-1", lib, "al-1", &["ar-a"], None, false);
    media_row(&db, "mf-2", lib, "al-1", &["ar-a", "ar-b"], None, false);
    media_row(
        &db,
        "mf-3",
        lib,
        "al-1",
        &["ar-a", "ar-b", "ar-c"],
        None,
        false,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        2,
        Some(play_stamp(10)),
        0,
        false,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-2",
        "media_file",
        4,
        Some(play_stamp(20)),
        0,
        false,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-3",
        "media_file",
        8,
        Some(play_stamp(30)),
        0,
        false,
    );
    // A second user, so the `(user_id, atom)` grouping is exercised, not just the fold.
    seed_annotation(
        &db,
        "u2",
        "mf-2",
        "media_file",
        7,
        Some(play_stamp(40)),
        0,
        false,
    );

    let rows = committed(&db, refresh_artist_play_counts_in_tx);

    // u1: ar-a = 2+4+8 = 14, ar-b = 4+8 = 12, ar-c = 8; dates are the per-artist max.
    assert_eq!(
        annotation_state(&db, "u1", "ar-a", "artist"),
        Some((
            14,
            Some(time::to_text(&play_stamp(30))),
            0,
            false,
            None,
            None
        )),
    );
    assert_eq!(
        annotation_state(&db, "u1", "ar-b", "artist"),
        Some((
            12,
            Some(time::to_text(&play_stamp(30))),
            0,
            false,
            None,
            None
        )),
    );
    assert_eq!(
        annotation_state(&db, "u1", "ar-c", "artist"),
        Some((
            8,
            Some(time::to_text(&play_stamp(30))),
            0,
            false,
            None,
            None
        )),
        "ar-c is reachable only through the participants fold"
    );
    // u2 annotated only mf-2, so it gets ar-a and ar-b from that row alone.
    assert_eq!(
        annotation_state(&db, "u2", "ar-a", "artist"),
        Some((
            7,
            Some(time::to_text(&play_stamp(40))),
            0,
            false,
            None,
            None
        ))
    );
    assert_eq!(
        annotation_state(&db, "u2", "ar-b", "artist"),
        Some((
            7,
            Some(time::to_text(&play_stamp(40))),
            0,
            false,
            None,
            None
        ))
    );
    assert_eq!(
        annotation_state(&db, "u2", "ar-c", "artist"),
        None,
        "u2 has no annotation on a media file carrying ar-c"
    );
    // 5 (user, artist) groups → 5 statement-level changes.
    assert_eq!(rows, 5, "affected rows = (user, artist) groups written");
    // The production artist getter agrees with the stored row for both users.
    assert_eq!(read_artist(&db, "ar-a", "u1").annotation.play_count, 14);
    assert_eq!(read_artist(&db, "ar-a", "u2").annotation.play_count, 7);

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pin 7d + pin 5 — tag counts transition from DB-06's `0, 0` seed.
// ---------------------------------------------------------------------------

#[test]
fn tag_update_counts_from_db06_seeded_zero() {
    let (db, path) = fresh_store("tag-zero");
    let lib1 = put_library(&db, "tag-lib1");
    let lib2 = put_library(&db, "tag-lib2");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib1);
    seed_access(&db, "u1", lib2);
    put_artist(&db, "ar-main", &[lib1, lib2], false);
    put_album(
        &db,
        &album_record("al-1", lib1, 2, 100, 10.0, false, Some("Rock"), &[]),
    );
    put_album(
        &db,
        &album_record("al-2", lib1, 1, 100, 5.0, false, Some("Jazz"), &[]),
    );
    put_album(
        &db,
        &album_record("al-3", lib2, 1, 100, 5.0, false, Some("Rock"), &[]),
    );
    media_row(&db, "mf-1", lib1, "al-1", &["ar-main"], Some("Rock"), false);
    media_row(&db, "mf-2", lib1, "al-1", &["ar-main"], Some("Rock"), false);
    media_row(&db, "mf-3", lib1, "al-2", &["ar-main"], Some("Jazz"), false);
    media_row(&db, "mf-4", lib2, "al-3", &["ar-main"], Some("Pop"), false);

    // DB-06's `add_in_tx` seeds every link row at literal `0, 0`, so the transition below
    // cannot pass by coincidence.
    let rock = genre_tag("Rock");
    let jazz = genre_tag("Jazz");
    let pop = genre_tag("Pop");
    add_tags_committed(&db, lib1, &[rock.clone(), jazz.clone()]);
    add_tags_committed(&db, lib2, &[rock.clone(), pop.clone()]);

    assert_eq!(link_counts(&db, &rock.id, lib1), Some((0, 0)));
    assert_eq!(link_counts(&db, &rock.id, lib2), Some((0, 0)));
    assert_eq!(link_counts(&db, &jazz.id, lib1), Some((0, 0)));
    assert_eq!(link_counts(&db, &pop.id, lib2), Some((0, 0)));
    assert_eq!(
        tag_cells(&db, Some("u1")),
        ["Jazz/0/0", "Pop/0/0", "Rock/0/0"]
    );

    // Act
    let rows = committed(&db, update_counts_in_tx);

    // Album leg: Rock lib1 = al-1, Rock lib2 = al-3, Jazz lib1 = al-2.
    // Media leg: Rock lib1 = mf-1 + mf-2 (2 distinct ids), Jazz lib1 = mf-3, Pop lib2 = mf-4.
    assert_eq!(link_counts(&db, &rock.id, lib1), Some((1, 2)));
    assert_eq!(link_counts(&db, &rock.id, lib2), Some((1, 0)));
    assert_eq!(link_counts(&db, &jazz.id, lib1), Some((1, 1)));
    assert_eq!(link_counts(&db, &pop.id, lib2), Some((0, 1)));
    // Production getter: COALESCE(SUM(…)) over u1's two libraries.
    assert_eq!(
        tag_cells(&db, Some("u1")),
        ["Jazz/1/1", "Pop/0/1", "Rock/2/2"]
    );

    // Pin 5: the return is the SUM of the two statements' RowsAffected (3 + 3), which is
    // neither a tag-entity count (3 dictionary tags) nor a link-row count (4 rows).
    assert_eq!(rows, 6, "sum of both statements' RowsAffected");
    assert_eq!(link_census(&db).len(), 4, "4 link rows, 6 writes");

    // Idempotence: values settle, but `DO UPDATE` still reports a change.
    let again = committed(&db, update_counts_in_tx);
    assert_eq!(
        again, 6,
        "DO UPDATE reports a change even when the value is unchanged"
    );
    assert_eq!(
        tag_cells(&db, Some("u1")),
        ["Jazz/1/1", "Pop/0/1", "Rock/2/2"],
        "the same values after the second pass"
    );

    cleanup(db, path);
}

#[test]
fn tag_update_counts_writes_only_the_genre_projection() {
    let (db, path) = fresh_store("tag-genre");
    let lib = put_library(&db, "genre-scope");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    // An album carrying genre + a non-genre name; a media file carrying genre + another.
    let record = {
        let mut r = album_record("al-1", lib, 1, 100, 10.0, false, Some("Rock"), &[]);
        r.tags
            .insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
        r
    };
    put_album(&db, &record);
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Rock"), false);
    fixture_insert(
        &db,
        "UPDATE media_file SET tags = ? WHERE id = 'mf-1'",
        &[text(&{
            let mut tags = DbTags::new();
            tags.insert("genre".to_string(), vec!["Rock".to_string()]);
            tags.insert("mood".to_string(), vec!["Chill".to_string()]);
            marshal_tags(tags)
        })],
    );
    let rock = genre_tag("Rock");
    let deluxe = Tag {
        id: nd_id::new_tag_id("albumversion", "Deluxe"),
        tag_name: "albumversion".to_string(),
        tag_value: "Deluxe".to_string(),
        album_count: 0,
        song_count: 0,
    };
    let chill = Tag {
        id: nd_id::new_tag_id("mood", "Chill"),
        tag_name: "mood".to_string(),
        tag_value: "Chill".to_string(),
        album_count: 0,
        song_count: 0,
    };
    add_tags_committed(&db, lib, &[rock.clone(), deluxe.clone(), chill.clone()]);

    committed(&db, update_counts_in_tx);

    // Only `$.genre` is aggregated (`tag_repository.go:59`): the two non-genre dictionary
    // ids keep their `0, 0` seed and are never zero-cleared or invented.
    assert_eq!(link_counts(&db, &rock.id, lib), Some((1, 1)));
    assert_eq!(link_counts(&db, &deluxe.id, lib), Some((0, 0)));
    assert_eq!(link_counts(&db, &chill.id, lib), Some((0, 0)));
    // The production getter only sees genre rows anyway (`GetAll("genre")`), so the
    // non-genre rows are asserted at the table.
    assert_eq!(tag_cells(&db, Some("u1")), ["Rock/1/1"]);

    cleanup(db, path);
}

#[test]
fn tag_update_counts_drops_a_genre_id_absent_from_the_dictionary() {
    let (db, path) = fresh_store("tag-inner-join");
    let lib = put_library(&db, "tag-join");
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.0, false, Some("Ghost"), &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Ghost"), false);

    committed(&db, update_counts_in_tx);

    // `JOIN tag ON tag.id = jt.value` is INNER (`tag_repository.go:60`): a genre id with no
    // dictionary row yields NO row, not a zeroed one. This is also why a tag whose link row
    // already exists keeps its stored counts when its projection produces nothing.
    assert!(
        link_census(&db).is_empty(),
        "no library_tag row is invented"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pins 2, 3, 4, 7a — library totals: eight probes, distinct columns, caller stamp.
// ---------------------------------------------------------------------------

/// One library whose eight probe values are ALL distinct, so any pairwise swap of probe
/// readings changes at least one column (pin 7a).
fn seed_distinct_library(db: &SQLStore, lib: i64, other: i64) {
    put_artist(db, "ar-live", &[lib], false);
    put_artist(db, "ar-gone", &[lib], true);
    put_artist(db, "ar-other", &[other], false);
    // 2 non-missing albums (size 100+200, duration 10.5+20.25) → albums = 2, size = 300,
    // duration = 30.75. One missing album and one album in the OTHER library are excluded.
    put_album(
        db,
        &album_record("al-1", lib, 2, 100, 10.5, false, None, &["fold-1".into()]),
    );
    put_album(
        db,
        &album_record("al-2", lib, 1, 200, 20.25, false, None, &["fold-2".into()]),
    );
    put_album(
        db,
        &album_record("al-3", lib, 1, 9_000, 90.0, true, None, &["fold-3".into()]),
    );
    put_album(
        db,
        &album_record("al-4", other, 1, 77, 7.0, false, None, &[]),
    );
    // Folders: 3 with audio (fold-1 3/1/2, fold-2 2/0/1, fold-3 1/2/0), one zero-audio
    // folder, one missing folder.
    put_folder(db, "fold-1", lib, 3, 1, 2, false);
    put_folder(db, "fold-2", lib, 2, 0, 1, false);
    put_folder(db, "fold-3", lib, 1, 2, 0, false);
    put_folder(db, "fold-0", lib, 0, 0, 0, false);
    put_folder(db, "fold-9", lib, 4, 1, 1, true);
    // media_file: 4 non-missing, 5 missing in this library; 1 in the other.
    media_row(db, "mf-1", lib, "al-1", &["ar-live"], None, false);
    media_row(db, "mf-2", lib, "al-1", &["ar-live"], None, false);
    media_row(db, "mf-3", lib, "al-2", &["ar-live"], None, false);
    media_row(db, "mf-4", lib, "al-2", &["ar-live"], None, false);
    media_row(db, "mf-5", lib, "al-1", &["ar-gone"], None, true);
    media_row(db, "mf-6", lib, "al-1", &["ar-gone"], None, true);
    media_row(db, "mf-7", lib, "al-2", &["ar-gone"], None, true);
    media_row(db, "mf-8", lib, "al-2", &["ar-gone"], None, true);
    media_row(db, "mf-9", lib, "al-3", &["ar-gone"], None, true);
    media_row(db, "mf-10", other, "al-4", &["ar-other"], None, false);
}

#[test]
fn library_totals_refresh_all_probes_distinct_columns() {
    let (db, path) = fresh_store("lib-probes");
    let lib = put_library(&db, "probes");
    let other = put_library(&db, "probes-other");
    seed_distinct_library(&db, lib, other);

    let totals = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, lib, &refresh_stamp())
    });

    assert_eq!(totals.id, lib);
    assert_eq!(totals.songs, 4, "media_file where missing = false");
    assert_eq!(totals.albums, 2, "album where missing = false");
    assert_eq!(
        totals.artists, 1,
        "library_artist INNER JOIN artist, a.missing = false"
    );
    assert_eq!(
        totals.folders, 3,
        "folder missing = false AND num_audio_files > 0"
    );
    assert_eq!(
        totals.files, 12,
        "ifnull(sum(num_audio_files + num_playlists + json_array_length(image_files)),0) over \
         ALL non-missing folders: fold-1 3+1+2, fold-2 2+0+1, fold-3 1+2+0, fold-0 0+0+0 \
         (included, contributing nothing), fold-9 4+1+1 excluded as missing"
    );
    assert_eq!(totals.missing, 5, "media_file where missing = true");
    assert_eq!(
        totals.size, 300,
        "ifnull(sum(album.size),0) over non-missing albums"
    );
    assert_eq!(
        totals.duration, 30.75,
        "ifnull(sum(album.duration),0) over non-missing albums"
    );

    // Stored row through the production getter, not raw SQL.
    let row = read_library(&db, lib);
    assert_eq!(row.total_songs, totals.songs);
    assert_eq!(row.total_albums, totals.albums);
    assert_eq!(row.total_artists, totals.artists);
    assert_eq!(row.total_folders, totals.folders);
    assert_eq!(row.total_files, totals.files);
    assert_eq!(row.total_missing_files, totals.missing);
    assert_eq!(row.total_size, totals.size);
    assert_eq!(row.total_duration, totals.duration);
    assert_eq!(row.updated_at, refresh_stamp(), "pin 2: the caller's stamp");

    // Pin 7a's precondition: the eight readings are pairwise distinct, so a swapped pair
    // cannot accidentally land on a matching value.
    let ints = [
        totals.songs,
        totals.albums,
        totals.artists,
        totals.folders,
        totals.files,
        totals.missing,
        totals.size,
    ];
    let unique: std::collections::HashSet<i64> = ints.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ints.len(),
        "seven integer probes are distinct: {ints:?}"
    );
    assert_ne!(totals.files, totals.folders, "total_files vs total_folders");
    assert_ne!(
        totals.songs, totals.missing,
        "total_songs vs total_missing_files"
    );
    assert_ne!(
        totals.albums, totals.artists,
        "total_albums vs total_artists"
    );
    // No column is NULL: every one of the eight hydrates into Go's non-pointer field.
    for column in [
        "total_songs",
        "total_albums",
        "total_artists",
        "total_folders",
        "total_files",
        "total_missing_files",
        "total_size",
        "total_duration",
    ] {
        let storage = typeof_column(&db, "library", column, &format!("id = {lib}"));
        assert!(
            storage == "integer" || storage == "real",
            "{column} storage: {storage}"
        );
    }

    // Pin 3, observed consequence: the eight probes ran serially on the transaction's one
    // connection, so a fresh lease after the commit sees exactly the returned struct — the
    // eight values came from one snapshot.
    assert_eq!(read_library(&db, lib).total_duration, totals.duration);

    // The other library is untouched by refreshing `lib`.
    let untouched = read_library(&db, other);
    assert_eq!(
        untouched.total_songs, 11,
        "the seeded value is not replaced"
    );
    assert_eq!(untouched.total_albums, 12);
    assert_eq!(untouched.total_files, 15);
    assert_eq!(
        untouched.updated_at,
        seed_stamp(),
        "`Put` stamps `updated_at` from the caller's clock, so the seed value is the stamp"
    );

    cleanup(db, path);
}

#[test]
fn swapped_probe_assignment_fails_the_stored_row() {
    let (db, path) = fresh_store("probe-swap");
    let lib = put_library(&db, "swap");
    let other = put_library(&db, "swap-other");
    seed_distinct_library(&db, lib, other);

    let totals = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, lib, &refresh_stamp())
    });
    let stored = read_library(&db, lib);

    // The mutation pin 7a names: swap the folder-count and file-count readings, which is
    // what a mis-ordered `read_i64(PROBE_*)` pair would produce.
    let mut swapped = totals.clone();
    swapped.folders = totals.files;
    swapped.files = totals.folders;
    assert_ne!(
        swapped, totals,
        "the fixture must make files and folders differ"
    );
    assert_ne!(
        stored.total_folders, swapped.folders,
        "swapped readings must not match"
    );
    assert_ne!(
        stored.total_files, swapped.files,
        "swapped readings must not match"
    );
    // And the correct pairing does match, so the assertions above are about order.
    assert_eq!(stored.total_folders, totals.folders);
    assert_eq!(stored.total_files, totals.files);

    // A second mutation: songs and missing swapped (the two probes sharing one statement).
    let mut swapped2 = totals.clone();
    swapped2.songs = totals.missing;
    swapped2.missing = totals.songs;
    assert_ne!(stored.total_songs, swapped2.songs);
    assert_ne!(stored.total_missing_files, swapped2.missing);

    cleanup(db, path);
}

#[test]
fn library_totals_equal_stamp_idempotence() {
    let (db, path) = fresh_store("lib-idem");
    let lib = put_library(&db, "idem");
    let other = put_library(&db, "idem-other");
    seed_distinct_library(&db, lib, other);

    let stamp = refresh_stamp();
    let first = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &stamp));
    let after_first = read_library(&db, lib);
    let second = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &stamp));
    let after_second = read_library(&db, lib);

    assert_eq!(first, second, "same data + same stamp → identical totals");
    assert_eq!(after_first, after_second, "the stored row does not move");
    assert_eq!(
        after_second.updated_at, stamp,
        "the stamp is the caller's on both passes"
    );
    assert_eq!(
        time::to_text(&stamp),
        "2025-02-03 04:05:06.789+00:00",
        "canonical DB-02 spelling, not a Rust-typed datetime"
    );

    // A LATER caller stamp DOES move `updated_at`, so the equality above is the data being
    // stable rather than the column being frozen.
    let later = at(2026, 1, 1, 0, 0, 0, 0);
    let third = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &later));
    assert_eq!(
        third.updated_at, later,
        "the returned struct carries the new stamp"
    );
    assert_eq!(read_library(&db, lib).updated_at, later);

    // A changed population changes the totals — again proving the column is live.
    fixture_insert(
        &db,
        "UPDATE media_file SET missing = false WHERE id = 'mf-5'",
        &[],
    );
    let third = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &later));
    assert_eq!(
        third.songs, 5,
        "the newly non-missing media file is counted"
    );
    assert_eq!(third.missing, 4);

    cleanup(db, path);
}

#[test]
fn caller_stamp_binds_canonical_text() {
    let (db, path) = fresh_store("stamp");
    let lib = put_library(&db, "stamp");

    let stamp = at(2024, 9, 8, 7, 6, 5, 123_000_000);
    let totals = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &stamp));

    let row = read_library(&db, lib);
    assert_eq!(
        row.updated_at, stamp,
        "production getter agrees with the caller's stamp"
    );
    assert_eq!(totals.updated_at, stamp);
    assert_eq!(
        typeof_column(&db, "library", "updated_at", &format!("id = {lib}")),
        "text",
        "DB-02: TEXT storage class, not an epoch integer"
    );
    let stored: String = scalar(
        &db,
        &format!("SELECT updated_at FROM library WHERE id = {lib}"),
    );
    assert_eq!(stored, time::to_text(&stamp));
    assert_eq!(stored, "2024-09-08 07:06:05.123+00:00");

    // A stamp with no fractional second renders without a fraction (rusqlite's `%.f`).
    let whole = at(2024, 9, 8, 7, 6, 5, 0);
    let restamped = committed(&db, |tx| refresh_library_totals_in_tx(tx, lib, &whole));
    assert_eq!(restamped.updated_at, whole);
    let stored: String = scalar(
        &db,
        &format!("SELECT updated_at FROM library WHERE id = {lib}"),
    );
    assert_eq!(stored, "2024-09-08 07:06:05+00:00");
    // No wall-clock leak anywhere in the helper.
    assert_ne!(stored, time::to_text(&time::now()));

    cleanup(db, path);
}

#[test]
fn library_totals_empty_population_is_zero_not_null() {
    let (db, path) = fresh_store("lib-empty");
    let lib = put_library(&db, "empty-lib");

    let totals = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, lib, &refresh_stamp())
    });

    // Go's `ifnull(sum(…),0)` arms (`:221-231`) plus `count(*)` over no rows: every total is 0.
    assert_eq!(totals.songs, 0);
    assert_eq!(totals.albums, 0);
    assert_eq!(totals.artists, 0);
    assert_eq!(totals.folders, 0);
    assert_eq!(totals.files, 0);
    assert_eq!(totals.missing, 0);
    assert_eq!(totals.size, 0);
    assert_eq!(totals.duration, 0.0);

    let row = read_library(&db, lib);
    assert_eq!(
        (row.total_songs, row.total_albums, row.total_artists),
        (0, 0, 0)
    );
    assert_eq!(
        (row.total_folders, row.total_files, row.total_missing_files),
        (0, 0, 0)
    );
    assert_eq!(row.total_size, 0);
    assert_eq!(row.total_duration, 0.0);
    // The read itself is the NULL-vs-0 proof: `total_duration` is Go's non-pointer float64
    // and a NULL cell would be a scan error, not a zero (`LibraryError::Sqlite`).
    assert_eq!(
        typeof_column(&db, "library", "total_duration", &format!("id = {lib}")),
        "real",
        "the 0 from ifnull(sum(…),0) is bound as REAL by the update leg"
    );

    // A refresh of a NON-EXISTENT library id is a successful 0-row UPDATE, not an error:
    // Go discards the affected count (`library_repository.go:249`).
    let absent = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, 999, &refresh_stamp())
    });
    assert_eq!(absent.id, 999);
    assert_eq!(absent.songs, 0);
    assert!(
        matches!(read_library_attempt(&db, 999), Err(LibraryError::NotFound)),
        "no row was created by the 0-row UPDATE"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pin 7e — rollback injection mid-sequence, pool stays usable.
// ---------------------------------------------------------------------------

#[test]
fn refresh_sequence_rollback_injection_mid_sequence() {
    let (db, path) = fresh_store("rollback");
    let lib = put_library(&db, "rollback");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.5, false, Some("Rock"), &[]),
    );
    put_folder(&db, "fold-1", lib, 1, 0, 0, false);
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Rock"), false);
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        6,
        Some(play_stamp(10)),
        4,
        true,
    );
    seed_annotation(&db, "u1", "al-1", "album", 1, Some(play_stamp(1)), 2, false);
    let rock = genre_tag("Rock");
    add_tags_committed(&db, lib, std::slice::from_ref(&rock));

    let before_album = annotation_state(&db, "u1", "al-1", "album").expect("pre-seeded row");
    assert_eq!(
        before_album.0, 1,
        "the pre-refresh album count is the stale one"
    );
    let before_tag = link_counts(&db, &rock.id, lib);
    assert_eq!(before_tag, Some((0, 0)));
    let before_library = read_library(&db, lib);

    // Act: album → artist → tag → library, then a REAL SQLite failure after all four have
    // mutated state inside the caller's transaction.
    let error = db
        .try_with_tx_sync::<(usize, usize, usize, RefreshStatsTotals), Infallible>(|tx| {
            let a = refresh_album_play_counts_in_tx(tx)?;
            let r = refresh_artist_play_counts_in_tx(tx)?;
            let t = update_counts_in_tx(tx)?;
            let l = refresh_library_totals_in_tx(tx, lib, &refresh_stamp())?;
            // All four effects ARE visible inside the transaction…
            assert_eq!(
                annotation_state_tx(tx, "u1", "al-1", "album")
                    .expect("in-tx row")
                    .0,
                6,
                "the album refresh took effect inside the transaction"
            );
            assert_eq!(
                annotation_state_tx(tx, "u1", "ar-main", "artist")
                    .expect("in-tx artist row")
                    .0,
                6,
                "the artist refresh took effect inside the transaction"
            );
            // …and nothing of the tuple below survives the rollback.
            Err(inject_fk_failure(tx)).map(|()| (a, r, t, l))
        })
        .expect_err("the injected FK failure must fail the transaction");

    // Pin 6: typed failure, BODY stage, concrete rusqlite cause preserved.
    match &error {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body, "a callback failure is Body");
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                "the concrete cause survived, un-stringified (rule E1)"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    // Nothing partial is durable.
    assert_eq!(
        annotation_state(&db, "u1", "al-1", "album"),
        Some(before_album),
        "the album refresh rolled back"
    );
    assert_eq!(
        annotation_state(&db, "u1", "ar-main", "artist"),
        None,
        "the artist refresh's inserted row rolled back"
    );
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        before_tag,
        "tag counts rolled back to 0/0"
    );
    assert_eq!(
        read_library(&db, lib),
        before_library,
        "library totals rolled back"
    );

    // Pin 7e: the pool is NOT poisoned — every handle still leases, and a retry commits.
    let again = committed(&db, refresh_album_play_counts_in_tx);
    assert_eq!(again, 1, "a later transaction on the same pool succeeds");
    assert_eq!(
        annotation_state(&db, "u1", "al-1", "album")
            .expect("committed after rollback")
            .0,
        6,
        "the retry applies the same effect exactly once"
    );
    assert_eq!(
        read_album(&db, "al-1", "u1").annotation.play_count,
        6,
        "getter agrees"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pin 6f — the play_date condition Go does NOT have.
// ---------------------------------------------------------------------------

#[test]
fn playcount_refresh_replays_the_same_values_without_advancing_play_date() {
    let (db, path) = fresh_store("play-date");
    let lib = put_library(&db, "play-date");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 2, 100, 10.5, false, None, &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], None, false);
    media_row(&db, "mf-2", lib, "al-1", &["ar-main"], None, false);
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        2,
        Some(play_stamp(100)),
        0,
        false,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-2",
        "media_file",
        3,
        Some(play_stamp(50)),
        0,
        false,
    );

    let first = committed(&db, refresh_album_play_counts_in_tx);
    let after_first = annotation_state(&db, "u1", "al-1", "album").expect("inserted");
    assert_eq!(first, 1);
    assert_eq!(after_first.0, 5, "sum = 2+3");
    assert_eq!(
        after_first.1,
        Some(time::to_text(&play_stamp(100))),
        "max = the newer of the two media dates"
    );

    // Re-run with unchanged data: values are STABLE (idempotent), but SQLite still
    // reports one change — Go has no `play_date` comparison to make it a no-op.
    let second = committed(&db, refresh_album_play_counts_in_tx);
    let after_second = annotation_state(&db, "u1", "al-1", "album").expect("still there");
    assert_eq!(
        after_second, after_first,
        "the row does not advance on a replay"
    );
    assert_eq!(
        second, 1,
        "RowsAffected counts the DO UPDATE even when nothing changed"
    );

    // SOURCE-VERIFIED consequence of the missing guard (`album_repository.go:424-425` is a
    // bare assignment, not `max(ifnull(play_date,''), ?)` like IncPlayCount): a NEWER
    // stored play_date is REPLACED by the older aggregate max.
    fixture_insert(
        &db,
        "UPDATE annotation SET play_date = ? WHERE item_id = 'al-1' AND item_type = 'album'",
        &[text(&time::to_text(&play_stamp(900_000)))],
    );
    committed(&db, refresh_album_play_counts_in_tx);
    assert_eq!(
        annotation_state(&db, "u1", "al-1", "album").expect("row").1,
        Some(time::to_text(&play_stamp(100))),
        "Go overwrites play_date with the aggregate max, even backwards"
    );

    assert_eq!(read_album(&db, "al-1", "u1").annotation.play_count, 5);

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// Pin 6 — typed error seam; pin 5 — affected rows are statement-level.
// ---------------------------------------------------------------------------

#[test]
fn refresh_failures_keep_the_typed_sqlite_cause() {
    let (db, path) = fresh_store("typed-error");
    // A malformed JSON `participants` cell makes the artist fold fail with SQLite's hard
    // `malformed JSON` error, which carries NO constraint extended code — so the caller
    // must match on the concrete `rusqlite::Error`, exactly as the DB-14/16 warn arms note.
    let lib = put_library(&db, "bad-json");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.0, false, None, &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], None, false);
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        3,
        Some(play_stamp(1)),
        0,
        false,
    );
    fixture_insert(
        &db,
        "UPDATE media_file SET participants = '{\"artist\":[{\"id\":' WHERE id = 'mf-1'",
        &[],
    );

    let error = db
        .try_with_tx_sync::<usize, Infallible>(refresh_artist_play_counts_in_tx)
        .expect_err("malformed JSON must fail the statement");
    match &error {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            let text = source.to_string();
            assert!(text.contains("malformed JSON"), "unexpected cause: {text}");
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_ERROR),
                "the concrete extended code is preserved: {text}"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    // No partial state durable: the damaged row and its media annotation are as they were,
    // and no artist annotation was created.
    assert_eq!(
        annotation_state(&db, "u1", "mf-1", "media_file").map(|r| r.0),
        Some(3)
    );
    assert_eq!(annotation_state(&db, "u1", "ar-main", "artist"), None);

    cleanup(db, path);
}

#[test]
fn affected_rows_are_statement_level_not_entity_counts() {
    let (db, path) = fresh_store("affected");
    let lib = put_library(&db, "affected");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.0, false, Some("Rock"), &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Rock"), false);
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        2,
        Some(play_stamp(1)),
        0,
        false,
    );
    let rock = genre_tag("Rock");
    add_tags_committed(&db, lib, std::slice::from_ref(&rock));

    // Tag leg: ONE dictionary tag, ONE link row, but TWO writes — one per statement, since
    // both the album and the media_file projection produce a group for (Rock, lib).
    let tag_rows = committed(&db, update_counts_in_tx);
    assert_eq!(
        tag_rows, 2,
        "one write per statement per populated (tag, library) group"
    );
    assert_eq!(link_census(&db).len(), 1, "one link row");

    let album_rows = committed(&db, refresh_album_play_counts_in_tx);
    let artist_rows = committed(&db, refresh_artist_play_counts_in_tx);
    assert_eq!(album_rows, 1, "1 album annotation written");
    assert_eq!(artist_rows, 1, "1 artist annotation written");

    // An empty database: every helper reports 0 changes and no error.
    let (empty_db, empty_path) = fresh_store("affected-empty");
    assert_eq!(committed(&empty_db, refresh_album_play_counts_in_tx), 0);
    assert_eq!(committed(&empty_db, refresh_artist_play_counts_in_tx), 0);
    assert_eq!(committed(&empty_db, update_counts_in_tx), 0);
    let empty_totals = committed(&empty_db, |tx| {
        refresh_library_totals_in_tx(tx, 1, &refresh_stamp())
    });
    assert_eq!(empty_totals.songs, 0);
    cleanup(empty_db, empty_path);

    cleanup(db, path);
}

#[test]
fn refresh_helpers_are_scoped_like_go_and_touch_no_db18_state() {
    let (db, path) = fresh_store("scope");
    let lib1 = put_library(&db, "scope-1");
    let lib2 = put_library(&db, "scope-2");
    seed_user(&db, "u1");
    seed_user(&db, "u2");
    seed_access(&db, "u1", lib1);
    seed_access(&db, "u2", lib2);
    // A shared artist and a shared genre across both libraries.
    put_artist(&db, "ar-shared", &[lib1, lib2], false);
    put_album(
        &db,
        &album_record("al-1", lib1, 1, 100, 10.0, false, Some("Rock"), &[]),
    );
    put_album(
        &db,
        &album_record("al-2", lib2, 1, 200, 20.0, false, Some("Rock"), &[]),
    );
    media_row(
        &db,
        "mf-1",
        lib1,
        "al-1",
        &["ar-shared"],
        Some("Rock"),
        false,
    );
    // A missing media file in lib2: the library probe excludes it, the tag projection does
    // NOT (Go's tag statement has no `missing` predicate) — both facts are asserted below.
    media_row(
        &db,
        "mf-2",
        lib2,
        "al-2",
        &["ar-shared"],
        Some("Rock"),
        true,
    );
    media_row(
        &db,
        "mf-3",
        lib1,
        "al-1",
        &["ar-shared"],
        Some("Rock"),
        false,
    );
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        4,
        Some(play_stamp(1)),
        0,
        false,
    );
    let rock = genre_tag("Rock");
    add_tags_committed(&db, lib1, std::slice::from_ref(&rock));
    add_tags_committed(&db, lib2, std::slice::from_ref(&rock));

    // Library totals ARE library-scoped: every probe binds `library_id`.
    let totals = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, lib1, &refresh_stamp())
    });
    assert_eq!(totals.songs, 2, "only lib1's non-missing media files");
    assert_eq!(totals.size, 100, "only lib1's album sizes");
    assert_eq!(
        totals.artists, 1,
        "the shared artist counts once, via lib1's link row"
    );

    // Play counts are GLOBAL by Go's design — neither statement carries a library
    // predicate — so lib2's user's plays credit the shared artist too.
    seed_annotation(
        &db,
        "u2",
        "mf-2",
        "media_file",
        9,
        Some(play_stamp(5)),
        0,
        false,
    );
    committed(&db, refresh_album_play_counts_in_tx);
    committed(&db, refresh_artist_play_counts_in_tx);
    assert_eq!(
        annotation_state(&db, "u1", "ar-shared", "artist").map(|r| r.0),
        Some(4),
        "u1's plays on lib1's media"
    );
    assert_eq!(
        annotation_state(&db, "u2", "ar-shared", "artist").map(|r| r.0),
        Some(9),
        "u2's plays on lib2's media reach the SAME shared artist row (its own row)"
    );

    // Tag counts are per-library: the shared genre keeps one row per library, and lib2's
    // missing media file IS counted there (no `missing` predicate in Go's tag statement).
    committed(&db, update_counts_in_tx);
    assert_eq!(link_counts(&db, &rock.id, lib1), Some((1, 2)));
    assert_eq!(link_counts(&db, &rock.id, lib2), Some((1, 1)));

    // Cross-library read scope still holds on the getter path: u1 cannot see lib2's album.
    assert!(
        matches!(
            read_album_attempt(&db, "al-2", "u1"),
            Err(AlbumError::NotFound)
        ),
        "the library filter on the read still applies after the refresh"
    );
    assert_eq!(read_album(&db, "al-1", "u1").annotation.play_count, 4);

    // DB-18's junction column is untouched: `library_artist.stats` keeps its DDL default.
    let stats: Vec<String> = lease(&db, |conn| {
        let mut stmt = conn
            .prepare("SELECT stats FROM library_artist ORDER BY library_id, artist_id")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    });
    assert_eq!(
        stats,
        vec!["{}".to_string(), "{}".to_string()],
        "no stats write (DB-18)"
    );

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// F3 — internal helper-failure regressions: the failure is INSIDE a helper, and
// rollback happens only when the caller propagates the error through the DB-00 seam.
// ---------------------------------------------------------------------------

/// Pin F2/F3(a): a library **probe** failure (not an injected post-body failure) stops
/// `refresh_library_totals_in_tx` before its totals UPDATE. Propagated, it rolls back the
/// earlier helpers' writes; swallowed, it commits them while `library` keeps its stale
/// totals — which is exactly why the rustdoc requires propagation instead of claiming the
/// primitive aborts the transaction.
#[test]
fn library_probe_failure_prevents_the_update_and_rolls_back_when_propagated() {
    let (db, path) = fresh_store("probe-fail");
    let lib = put_library(&db, "probe-fail");
    seed_user(&db, "u1");
    seed_access(&db, "u1", lib);
    put_artist(&db, "ar-main", &[lib], false);
    put_album(
        &db,
        &album_record(
            "al-1",
            lib,
            1,
            100,
            10.5,
            false,
            Some("Rock"),
            &["fold-1".into()],
        ),
    );
    put_folder(&db, "fold-1", lib, 1, 0, 1, false);
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Rock"), false);
    seed_annotation(
        &db,
        "u1",
        "mf-1",
        "media_file",
        6,
        Some(play_stamp(10)),
        4,
        true,
    );
    let rock = genre_tag("Rock");
    add_tags_committed(&db, lib, std::slice::from_ref(&rock));

    let before_album = annotation_state(&db, "u1", "al-1", "album");
    assert_eq!(
        before_album, None,
        "no album annotation exists before the refresh"
    );
    let before_tag = link_counts(&db, &rock.id, lib);
    assert_eq!(before_tag, Some((0, 0)));
    let before_library = read_library(&db, lib);

    // A malformed `image_files` cell makes PROBE_FILES (`json_array_length(image_files)`,
    // `library_repository.go:221-222`) fail with SQLite's hard `malformed JSON` error, so
    // the failure is raised by the helper itself, mid-probe, before any UPDATE.
    fixture_insert(
        &db,
        "UPDATE folder SET image_files = '{\"broken\":' WHERE id = 'fold-1'",
        &[],
    );

    // Direction 1 — PROPAGATED: album + tag refreshes run first, then the library helper
    // fails on probe 4 and the error leaves the callback, so the seam rolls back everything.
    let error = db
        .try_with_tx_sync::<(usize, usize), Infallible>(|tx| {
            let a = refresh_album_play_counts_in_tx(tx)?;
            let t = update_counts_in_tx(tx)?;
            // The earlier writes ARE visible inside the tx…
            assert_eq!(
                annotation_state_tx(tx, "u1", "al-1", "album")
                    .expect("in-tx album row")
                    .0,
                6,
                "the album refresh took effect inside the transaction"
            );
            // …and the library helper fails before its totals UPDATE, so its totals value
            // never exists — `?` propagates the probe's error out of the callback.
            let _totals = refresh_library_totals_in_tx(tx, lib, &refresh_stamp())?;
            Ok((a, t))
        })
        .expect_err("the probe failure must fail the transaction when propagated");
    match &error {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body, "a callback failure is Body");
            assert!(
                source.to_string().contains("malformed JSON"),
                "the probe's own cause survived: {source}"
            );
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_ERROR),
                "no constraint code — the caller must match the concrete error"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    // Nothing durable: neither earlier helper's write survived, and the totals are the seed.
    assert_eq!(
        annotation_state(&db, "u1", "al-1", "album"),
        before_album,
        "the album refresh rolled back with the probe failure"
    );
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        before_tag,
        "the tag refresh rolled back with the probe failure"
    );
    assert_eq!(
        read_library(&db, lib),
        before_library,
        "no totals UPDATE ran, so the library row is byte-identical to the seed"
    );

    // Direction 2 — SWALLOWED: the same sequence, but the callback discards the helper's
    // error and returns Ok, so the seam COMMITS. The prior helpers' writes become durable
    // and `library` keeps its stale totals — the hazard the rustdoc pins.
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        let a = refresh_album_play_counts_in_tx(tx)?;
        let t = update_counts_in_tx(tx)?;
        let swallowed = refresh_library_totals_in_tx(tx, lib, &refresh_stamp())
            .expect_err("the probe must still fail");
        // Deliberate swallow: this is the anti-pattern, and it commits.
        assert!(a > 0 && t > 0, "the two successful legs reported writes");
        assert!(
            swallowed.to_string().contains("malformed JSON"),
            "the swallowed error is the probe's own cause"
        );
        Ok(())
    })
    .expect("swallowing the error lets the transaction commit");

    assert_eq!(
        annotation_state(&db, "u1", "al-1", "album").map(|r| r.0),
        Some(6),
        "a swallowed error COMMITS the earlier helper's write"
    );
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        Some((1, 1)),
        "and it commits the tag refresh's counts too"
    );
    assert_eq!(
        read_library(&db, lib),
        before_library,
        "while the totals row keeps its stale seed values: the UPDATE never ran"
    );

    // The pool is un-poisoned and the helper is retryable once the damaged cell is repaired.
    fixture_insert(
        &db,
        "UPDATE folder SET image_files = '[\"cover.jpg\"]' WHERE id = 'fold-1'",
        &[],
    );
    let totals = committed(&db, |tx| {
        refresh_library_totals_in_tx(tx, lib, &refresh_stamp())
    });
    assert_eq!(
        totals.files, 2,
        "repaired cell: 1 audio + 0 playlists + 1 image"
    );
    assert_eq!(read_library(&db, lib).updated_at, refresh_stamp());
    assert_eq!(read_library(&db, lib).total_songs, totals.songs);

    cleanup(db, path);
}

/// Pin F2/F3(b): when tag statement TWO (`media_file`) fails, statement ONE's (`album`)
/// writes are NOT undone by `update_counts_in_tx` — only propagating the error through the
/// seam rolls them back. Both directions are asserted, mirroring Go's requirement that the
/// caller return the error out of the `WithTxRetry` callback (`scanner.go:316-321`).
#[test]
fn tag_statement_two_failure_rolls_back_statement_one_when_propagated() {
    let (db, path) = fresh_store("tag-stmt-two");
    let lib = put_library(&db, "tag-stmt-two");
    put_artist(&db, "ar-main", &[lib], false);
    // The album leg has a real group (al-1 carries genre Rock); the media leg is poisoned
    // by a malformed `tags` cell, so it fails AFTER the album leg already wrote.
    put_album(
        &db,
        &album_record("al-1", lib, 1, 100, 10.0, false, Some("Rock"), &[]),
    );
    media_row(&db, "mf-1", lib, "al-1", &["ar-main"], Some("Rock"), false);
    fixture_insert(
        &db,
        "UPDATE media_file SET tags = '{\"genre\":[\"Rock\"' WHERE id = 'mf-1'",
        &[],
    );
    let rock = genre_tag("Rock");
    add_tags_committed(&db, lib, std::slice::from_ref(&rock));
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        Some((0, 0)),
        "DB-06 seeded the link row at 0, 0"
    );

    // Direction 1 — PROPAGATED: statement one's write is rolled back with statement two's
    // failure, because the error leaves the callback.
    let error = db
        .try_with_tx_sync::<usize, Infallible>(|tx| {
            let failure = update_counts_in_tx(tx)
                .expect_err("statement two must fail on the malformed JSON cell");
            // Statement ONE already wrote: its row is visible IN the transaction, so the
            // helper's own return does not undo it — only the caller's propagation does.
            assert_eq!(
                link_counts_tx(tx, &rock.id, lib),
                Some((1, 0)),
                "statement one's write is visible in-tx while the error is being propagated"
            );
            Err(failure)
        })
        .expect_err("the statement-two failure must fail the transaction when propagated");
    match &error {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            assert!(
                source.to_string().contains("malformed JSON"),
                "statement two's own cause survived: {source}"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        Some((0, 0)),
        "statement one's album_count = 1 rolled back when the error was propagated"
    );

    // Direction 2 — SWALLOWED: the same call with the error discarded commits the
    // half-updated row: album_count is written, media_file_count stays 0.
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        let outcome = update_counts_in_tx(tx);
        match &outcome {
            Err(DbOperationError::Sqlite(source)) => assert!(
                source.to_string().contains("malformed JSON"),
                "the helper's error carries statement two's cause: {source}"
            ),
            other => panic!("expected a Sqlite-arm failure, got {other:?}"),
        }
        assert_eq!(
            link_counts_tx(tx, &rock.id, lib),
            Some((1, 0)),
            "statement one's write IS visible in the transaction when the error is swallowed"
        );
        Ok(())
    })
    .expect("swallowing the error lets the transaction commit");

    assert_eq!(
        link_counts(&db, &rock.id, lib),
        Some((1, 0)),
        "a swallowed error COMMITS the partial counts: album leg written, media leg not"
    );

    // Repairing the cell makes the helper converge on both columns (retry semantics).
    let repaired = {
        let mut tags = DbTags::new();
        tags.insert("genre".to_string(), vec!["Rock".to_string()]);
        marshal_tags(tags)
    };
    fixture_insert(
        &db,
        "UPDATE media_file SET tags = ? WHERE id = 'mf-1'",
        &[text(&repaired)],
    );
    let rows = committed(&db, update_counts_in_tx);
    assert_eq!(rows, 2, "both legs wrote once after the repair");
    assert_eq!(
        link_counts(&db, &rock.id, lib),
        Some((1, 1)),
        "the stale half-updated row is fully rewritten, not left partial"
    );

    cleanup(db, path);
}
