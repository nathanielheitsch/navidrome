//! DB-14 — the folder-delta persistence transaction (`scanner/phase_1_folders.go:348-506`)
//! EXECUTED through `nd_db::scanner_writes::persist_folder_delta` over the DB-00 typed
//! retry seam.
//!
//! Every claim below is EXECUTED against a real SQLite DB built from the fresh DDL
//! (`nd_dbschema::schema::apply_fresh_schema`, one temp file per invocation, FKs on) and read
//! back through real rows — never through rendered SQL text. The Go oracle is the source
//! sequence itself (there is no injectable Go scanner export here, per the subplan's item 8),
//! so expectations are pinned to Go source lines and labeled EXECUTED-RUST; the phase-order
//! manifest each test asserts is the `D-BATCH` warn-vs-fatal table with its Go line numbers.
//!
//! Batch inputs are synthetic but structurally valid DB records built through the production
//! write API where a row must pre-exist (library, artist, album, media, annotation), plus
//! SC-style pre-generated ids. Fixture INSERTs are used only where no production write API
//! exists (`user`, `item_artwork`) and for the test-local poison triggers — same precedent as
//! `runtime_album_writes.rs:113-129,768-776`.
//!
//! Non-vacuity discipline used throughout: (a) a dropped or reordered arm changes an ASSERTED
//! row, not a statement count — e.g. the artwork-invalidation arm is pinned by deleting
//! seeded `item_artwork` rows while a decoy row of the same kind survives; (b) every stamp is
//! a distinct named instant, so a write that stamped the commit clock stores a value no
//! assertion can name; (c) fatal-arm cases assert the pre-seeded sibling state is unchanged
//! AND that the same write succeeds after the trigger is dropped, so "nothing committed"
//! cannot mean "nothing ever ran"; (d) the retry test asserts a BUSY attempt really happened
//! by measuring the production backoff gap.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    copy_album_created_at_in_tx, participant_names_from_participants, put_album_in_tx, AlbumRecord,
    AlbumWriteError, CopyAlbumCreatedAtOutcome, COPY_CREATED_AT_COLUMN,
};
use nd_db::artist_repo::ArtistRecord;
use nd_db::artwork_queue_repo::{ArtworkQueueRepo, IMAGE_TYPE_PRIMARY};
use nd_db::library_repo::{put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError};
use nd_db::mediafile_repo::{
    mark_missing_sql, put_mediafile_in_tx, MediaFileRecord, MediaIdentity,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::scanner_writes::{
    persist_folder_delta, scan_artwork_item, AlbumTransition, CommittedFolderOutcome,
    FolderRunError, FolderStamps, FolderWriteBatch, FolderWriteStage, MissingMedia, ScopeRejection,
    SCAN_ARTWORK_PRIORITY,
};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sql_tags::DbTags;
use nd_db::sqlrepo::Value;
use nd_db::store::{is_busy, SQLStore, TX_RETRY_DELAY};
use nd_db::tag_repo::{add_sql, Tag};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::{apply_fresh_schema, DbOpenOptions, JournalMode, Synchronous};
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// The scanned library.
const LIB: i64 = 1;
/// A second configured library, used only as the decoy a mismatched record is measured
/// against (`ScopeRejection::*LibraryMismatch`).
const DECOY_LIB: i64 = 2;

/// Deterministic caller clocks — one per Go `time.Now()` site, all far in the past so a
/// commit-clock implementation could never satisfy an assertion by coincidence.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// Everything a batch stamps, each field a distinct instant.
#[must_use]
fn stamps() -> FolderStamps {
    FolderStamps {
        folder_put: at(2019, 1, 2, 3, 4, 5),
        artist_put: at(2019, 1, 2, 3, 4, 6),
        album_put: at(2019, 1, 2, 3, 4, 7),
        mediafile_put: at(2019, 1, 2, 3, 4, 8),
        mark_missing: at(2019, 1, 2, 3, 4, 9),
        album_touch: at(2019, 1, 2, 3, 4, 10),
        artwork: at(2019, 1, 2, 3, 4, 11),
    }
}

/// An older instant for pre-seeded rows, distinct from every batch stamp.
#[must_use]
fn seed_stamp() -> DateTime<Utc> {
    at(2011, 5, 6, 7, 8, 9)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled (size 2 so a lease is
/// never taken recursively).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = temp_path(label);
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// A pool whose EVERY handle has `busy_timeout = 0`, so a contended write reports
/// `SQLITE_BUSY` immediately instead of waiting (`runtime_mediafile_writes.rs:3236-3247`
/// precedent). `persist_folder_delta` itself uses the production driver and the production
/// 5s/10s/15s backoff (`store.rs:429-439`); this lane may not change that, so the retry test
/// really waits one backoff gap.
fn fresh_busy_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = temp_path(label);
    let config = PoolConfig {
        path: dsn(&path),
        size: 2,
        options: Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Delete),
            busy_timeout: 0,
            synchronous: Synchronous::Normal,
            foreign_keys: Some(true),
            cache_size: None,
        }),
    };
    let (pool, first) = Pool::new(&config).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-folder-persistence-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    path
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// One raw block on a pooled lease. Panics on a lease failure; never called inside a
/// transaction (that would re-check out the same pool).
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (seed rows and test-local trigger DDL).
fn fixture(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: impl std::fmt::Display) -> SqlValue {
    SqlValue::Text(value.to_string())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
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

/// Cell text of one row, for stamp and value assertions.
fn cell(store: &SQLStore, table: &str, column: &str, id: &str) -> Option<String> {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} for {id}: {e}"))
    })
}

/// Cell text of one row, tolerating a MISSING row: `None` = no such row,
/// `Some(None)` = the row exists and the column is NULL. Used where a rolled-back delta must
/// leave no row at all.
fn cell_row(store: &SQLStore, table: &str, column: &str, id: &str) -> Option<Option<String>> {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM {table} WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
    })
}

/// Rows of a junction table as a sorted set (Go's `ON CONFLICT DO NOTHING` over a
/// `json_each` iteration has no row order either).
fn junction_set(store: &SQLStore, sql: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut statement = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        let mut out: Vec<String> = rows
            .map(|r| r.unwrap_or_else(|e| panic!("{sql}: {e}")))
            .collect();
        out.sort();
        out
    })
}

/// `MATCH` hit count against a contentless FTS table (`content=''` makes the row count and
/// the MATCH answers the only observable).
fn fts_match(store: &SQLStore, table: &str, query: &str) -> i64 {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?"),
            [text(query)],
            |row| row.get(0),
        )
        .unwrap_or_else(|e| panic!("{table} MATCH {query}: {e}"))
    })
}

/// `MATCH` hits resolved back to entity ids through the contentless table's rowid.
fn fts_ids(store: &SQLStore, table: &str, base: &str, query: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut statement = conn
            .prepare(&format!(
                "SELECT b.id FROM {table} f JOIN {base} b ON b.rowid = f.rowid WHERE {table} MATCH ? ORDER BY b.id"
            ))
            .unwrap_or_else(|e| panic!("{table} join for {query}: {e}"));
        let rows = statement
            .query_map([text(query)], |row| row.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("{table} join for {query}: {e}"));
        rows.map(|r| r.unwrap_or_else(|e| panic!("{table} join: {e}")))
            .collect()
    })
}

/// Library 1 plus a decoy library 2, through the PRODUCTION DB-05 write.
fn seed_libraries(store: &SQLStore) {
    for (id, tag) in [(LIB, "main"), (DECOY_LIB, "decoy")] {
        let record = LibraryRecord {
            id,
            name: format!("lib-{tag}-{id}"),
            path: format!("/lib-{tag}-{id}"),
            remote_path: String::new(),
            last_scan_at: seed_stamp(),
            last_scan_started_at: seed_stamp(),
            full_scan_in_progress: false,
            updated_at: seed_stamp(),
            created_at: seed_stamp(),
            total_songs: 0,
            total_albums: 0,
            total_artists: 0,
            total_folders: 0,
            total_files: 0,
            total_missing_files: 0,
            total_size: 0,
            total_duration: 0.0,
            default_new_users: false,
            pid_album: String::new(),
            pid_track: String::new(),
            scanned_pid_album: String::new(),
            scanned_pid_track: String::new(),
        };
        store
            .try_with_tx_sync::<nd_db::library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
                put_in_tx(tx, &record, &seed_stamp(), &[] as &[LibraryColumn])
            })
            .unwrap_or_else(|e| panic!("seed_libraries({id}) failed: {e}"));
    }
}

/// A `user` row (no production write API yet — fixture INSERT, as in
/// `runtime_album_writes.rs:473`), needed for the `annotation` FK.
fn seed_user(store: &SQLStore, id: &str) {
    fixture(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(format!("{id}@x.y")),
            text("CIPHER"),
            integer(0),
            text(time::to_text(&seed_stamp())),
            text(time::to_text(&seed_stamp())),
        ],
    );
}

/// An annotation row on a specific item (`annotation` has no production write API in this
/// lane; `sql_annotations` renderers exist but the folder tx only MOVES rows, so seeding one
/// is the fixture, same as `runtime_annotation_transfers.rs`).
fn seed_annotation(store: &SQLStore, user_id: &str, item_id: &str, play_count: i64) {
    fixture(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, ?, 'album', ?, 3, 1)",
        &[text(user_id), text(item_id), integer(play_count)],
    );
}

/// An `item_artwork` row of a kind (`hash <> ''` = resolved). Deleting these is what the
/// warn-only artwork arm must do; a decoy row proves the arm is scoped to the batch's ids.
fn seed_item_artwork(store: &SQLStore, kind: &str, id: &str) {
    fixture(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, source_path, \
         ref_mtime, attempted_at, updated_at, trace, last_failure) \
         VALUES (?, ?, 'primary', 'hash-1', 'cover', '', 0, ?, ?, '[]', '[]')",
        &[
            text(kind),
            text(id),
            text(time::to_text(&seed_stamp())),
            text(time::to_text(&seed_stamp())),
        ],
    );
}

/// Participants referencing the batch's own artists.
fn fixture_participants(primary: &str, second: &str) -> Participants {
    let mut participants = Participants::new();
    participants.insert(
        Role::from("albumartist"),
        vec![Participant {
            id: primary.to_string(),
            name: "Main Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants.insert(
        Role::from("composer"),
        vec![Participant {
            id: second.to_string(),
            name: "Second Act".to_string(),
            sub_role: String::new(),
        }],
    );
    participants
}

/// Genre (the only indexed name, `sql_tags::INDEXED_TAG_NAMES`) plus a JSON-only name.
fn fixture_tags() -> DbTags {
    let mut tags = DbTags::new();
    tags.insert("genre".to_string(), vec!["Rock".to_string()]);
    tags.insert("albumversion".to_string(), vec!["Deluxe".to_string()]);
    tags
}

/// A folder-delta artist record with SC-supplied id.
fn artist_record(id: &str, name: &str) -> ArtistRecord {
    ArtistRecord {
        id: id.to_string(),
        name: name.to_string(),
        sort_artist_name: format!("sort-{id}"),
        order_artist_name: format!("order-{id}"),
        mbz_artist_id: format!("mbz-{id}"),
        similar_artists: "[]".to_string(),
        ..ArtistRecord::default()
    }
}

/// An album record for the delta. `created_at`/`updated_at` are the batch's own values and
/// the UPDATE leg never writes `created_at` (`marshal.rs` UPDATE_DROP), which is what makes
/// the copy arm's effect observable.
fn album_record(id: &str, primary: &str, second: &str, folder_id: &str) -> AlbumRecord {
    let participants = fixture_participants(primary, second);
    let mut discs = BTreeMap::new();
    discs.insert(1, format!("disc-one-{id}"));
    AlbumRecord {
        id: id.to_string(),
        library_id: LIB,
        name: format!("Album-{id}"),
        embed_art_path: format!("embed-{id}"),
        album_artist: "Main Act".to_string(),
        album_artist_id: primary.to_string(),
        min_year: 1979,
        max_year: 1981,
        date: format!("date-{id}"),
        min_original_year: 1978,
        max_original_year: 1977,
        original_date: format!("orig-{id}"),
        release_date: format!("rel-{id}"),
        compilation: false,
        comment: format!("cmt-{id}"),
        song_count: 2,
        duration: 120.5,
        size: 4096,
        discs: Some(discs),
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
        folder_ids: Some(vec![folder_id.to_string()]),
        explicit_status: format!("exp-{id}"),
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        description: format!("desc-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        external_info_updated_at: Some(seed_stamp()),
        genre: "Rock".to_string(),
        tags: fixture_tags(),
        participants: participants.clone(),
        participant_names: participant_names_from_participants(&participants),
        missing: false,
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

/// A media record for the delta (Go's `Put` derives the match identity from these two
/// fields, `mediafile_repository.go:189`).
/// An FTS-safe single token: `unicode61` splits on `-`, so an unquoted `mf-1` query would be
/// parsed as column syntax (`no such column: mf`) rather than a term.
#[must_use]
fn token(id: &str) -> String {
    id.replace('-', "")
}

fn media_record(id: &str, album_id: &str, folder_id: &str, path: &str) -> MediaFileRecord {
    let participants = fixture_participants("ar-main", "ar-second");
    MediaFileRecord {
        id: id.to_string(),
        album: format!("Album-{album_id}"),
        album_artist: "Main Act".to_string(),
        album_artist_id: "ar-main".to_string(),
        album_id: album_id.to_string(),
        artist: "Main Act".to_string(),
        artist_id: "ar-main".to_string(),
        birth_time: seed_stamp(),
        bit_depth: None,
        bit_rate: 320,
        bpm: None,
        catalog_num: format!("cat-{id}"),
        channels: 2,
        codec: format!("codec-{id}"),
        comment: format!("cmt-{id}"),
        compilation: false,
        created_at: Some(seed_stamp()),
        date: format!("date-{id}"),
        disc_number: 1,
        disc_subtitle: format!("dsub-{id}"),
        duration: 60.25,
        explicit_status: format!("exp-{id}"),
        folder_id: folder_id.to_string(),
        genre: "Rock".to_string(),
        has_cover_art: false,
        library_id: LIB,
        lyrics: format!("lyr-{id}"),
        mbz_recording_id: format!("mr-{id}"),
        mbz_release_track_id: format!("mrt-{id}"),
        mbz_album_id: format!("mal-{id}"),
        mbz_release_group_id: format!("mrg-{id}"),
        mbz_artist_id: format!("mar-{id}"),
        mbz_album_artist_id: format!("maa-{id}"),
        mbz_album_type: format!("mat-{id}"),
        mbz_album_comment: format!("mac-{id}"),
        missing: false,
        order_album_artist_name: format!("oaa-{id}"),
        order_album_name: format!("oal-{id}"),
        order_artist_name: format!("oar-{id}"),
        order_title: format!("oti-{id}"),
        original_date: format!("orig-{id}"),
        original_year: 1978,
        path: path.to_string(),
        pid: format!("pid-{id}"),
        probe_data: format!(r#"{{"codec":"mp3-{id}"}}"#),
        release_date: format!("rel-{id}"),
        release_year: 1980,
        rg_album_gain: Some(0.5),
        rg_album_peak: Some(0.9),
        rg_track_gain: Some(-0.3),
        rg_track_peak: Some(0.7),
        sample_rate: 44_100,
        size: 2048,
        sort_album_artist_name: format!("saa-{id}"),
        sort_album_name: format!("sal-{id}"),
        sort_artist_name: format!("sar-{id}"),
        sort_title: format!("sti-{id}"),
        suffix: "mp3".to_string(),
        tags: fixture_tags(),
        title: format!("Zbrfldr{}", token(id)),
        track_number: 1,
        year: 1980,
        participants: participants.clone(),
        participant_names: participant_names_from_participants(&participants),
        updated_at: Some(seed_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
        album_image: nd_db::artwork_hydration::ItemImage::default(),
    }
}

/// Commit one album through the production write API (pre-seeded rows the delta must
/// UPDATE rather than insert).
fn seed_album(store: &SQLStore, record: &AlbumRecord) {
    store
        .try_with_tx_sync::<nd_db::album_repo::AlbumWriteResult, AlbumWriteError>(|tx| {
            put_album_in_tx(tx, record, &seed_stamp())
        })
        .unwrap_or_else(|e| panic!("seed_album({}) failed: {e}", record.id));
}

/// Commit one media row through the production write API with an explicit id.
fn seed_media(store: &SQLStore, record: &MediaFileRecord) {
    let identity = MediaIdentity::Id(record.id.clone());
    store
        .try_with_tx_sync::<nd_db::mediafile_repo::MediaFileWriteResult, nd_db::mediafile_repo::MediaFileWriteError>(
            |tx| put_mediafile_in_tx(tx, &identity, record, &seed_stamp(), false),
        )
        .unwrap_or_else(|e| panic!("seed_media({}) failed: {e}", record.id));
}

/// The canonical one-folder delta: folder `fo-1`, artists `ar-main`/`ar-second` (+ the two
/// names Go excludes from the queue), album `al-1`, one track, tags, and one queue-worthy set.
fn base_batch() -> FolderWriteBatch {
    FolderWriteBatch {
        library_id: LIB,
        folder: nd_db::folder_repo::FolderRecord {
            id: "fo-1".to_string(),
            library_id: LIB,
            path: "album-one".to_string(),
            name: "album-one".to_string(),
            parent_id: String::new(),
            num_audio_files: 1,
            num_playlists: 0,
            image_files: Some(vec!["cover.jpg".to_string()]),
            images_updated_at: seed_stamp(),
            hash: "hash-fo-1".to_string(),
            missing: false,
            updated_at: seed_stamp(),
            created_at: Some(seed_stamp()),
            library_path: String::new(),
        },
        tags: vec![
            Tag {
                id: nd_id::new_tag_id("genre", "Rock"),
                tag_name: "genre".to_string(),
                tag_value: "Rock".to_string(),
                ..Tag::default()
            },
            Tag {
                id: nd_id::new_tag_id("albumversion", "Deluxe"),
                tag_name: "albumversion".to_string(),
                tag_value: "Deluxe".to_string(),
                ..Tag::default()
            },
        ],
        artists: vec![
            artist_record("ar-main", "Main Act"),
            artist_record("ar-second", "Second Act"),
            artist_record("ar-unknown", nd_conf::consts::UNKNOWN_ARTIST),
            artist_record("ar-va", nd_conf::consts::VARIOUS_ARTISTS),
        ],
        albums: vec![album_record("al-1", "ar-main", "ar-second", "fo-1")],
        media: vec![media_record(
            "mf-1",
            "al-1",
            "fo-1",
            "album-one/track-01.mp3",
        )],
        missing: Vec::new(),
        album_transitions: Vec::new(),
        full_scan_mode: false,
        append_subtitle: false,
        stamps: stamps(),
    }
}

/// Run the delta and panic on any failure — the shape most read-back tests need.
fn run_ok(store: &SQLStore, batch: &FolderWriteBatch) -> CommittedFolderOutcome {
    persist_folder_delta(store, batch)
        .unwrap_or_else(|e| panic!("persist_folder_delta failed: {e}"))
}

/// Assert the executor's Body-stage shape with the concrete extended code retained.
fn assert_body_failure(err: &FolderRunError, expected_extended: Option<i32>) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            match source {
                SqliteError::SqliteFailure(code, _) => {
                    assert_eq!(
                        Some(code.extended_code),
                        expected_extended,
                        "the raw SQLite cause survives the seam unwrapped: {source:?}"
                    );
                }
                other => panic!("expected a SqliteFailure, got {other:?}"),
            }
            assert!(!is_busy(source), "a trigger failure is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// The whole observable state of the fixture graph, so a rollback test compares against a
/// real baseline instead of against "no rows".
fn state(store: &SQLStore) -> Vec<(String, i64)> {
    let tables = [
        "folder",
        "tag",
        "library_tag",
        "artist",
        "library_artist",
        "album",
        "album_artists",
        "album_tags",
        "media_file",
        "media_file_artists",
        "media_file_tags",
        "artwork_queue",
        "item_artwork",
        "annotation",
    ];
    tables
        .iter()
        .map(|t| {
            (
                t.to_string(),
                scalar::<i64>(store, &format!("SELECT count(*) FROM {t}")),
            )
        })
        .collect()
}

/// Assert the state is exactly the baseline: nothing from the attempt survived.
fn assert_state_unchanged(store: &SQLStore, baseline: &[(String, i64)], note: &str) {
    assert_eq!(
        &state(store),
        baseline,
        "{note}: state changed after a rolled-back delta"
    );
}

/// Every table this transaction touches must be EMPTY — the fatal-arm assertion. An FTS
/// ghost is caught by the MATCH probes below, not by a table count.
fn assert_everything_empty(store: &SQLStore, note: &str) {
    let counts = state(store);
    for (table, count) in &counts {
        assert_eq!(
            *count, 0,
            "{note}: table {table} has {count} rows after a rolled-back delta"
        );
    }
}

#[test]
fn full_folder_delta_commits_in_go_order_with_read_back() {
    let (db, path) = fresh_store("full");
    seed_libraries(&db);
    // Seeded artwork for BOTH tracks of the batch and a decoy of the same kind: the
    // invalidation arm must delete exactly the batch's ids and leave the decoy alone, which is
    // what makes "the arm ran, in order, against the COMMITTED ids" observable rather than a
    // count coincidence.
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-2");
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-decoy");

    let mut batch = base_batch();
    // A second track, so the media loop and the artwork chunk arm are not single-row.
    batch.media.push(media_record(
        "mf-2",
        "al-1",
        "fo-1",
        "album-one/track-02.mp3",
    ));
    batch.folder.num_audio_files = 2;
    // A missing track from an album that already exists: the MarkMissing + Touch arms.
    seed_album(
        &db,
        &album_record("al-old", "ar-main", "ar-second", "fo-old"),
    );
    seed_media(
        &db,
        &media_record("mf-old", "al-old", "fo-old", "old/track-01.mp3"),
    );
    batch.missing = vec![MissingMedia {
        id: "mf-old".to_string(),
        album_id: "al-old".to_string(),
    }];

    let outcome = run_ok(&db, &batch);

    // --- outcome identity (published only from the committed attempt) ---
    assert_eq!(outcome.folder_id, "fo-1");
    assert!(outcome.folder_inserted, "fresh folder takes the INSERT leg");
    assert_eq!(outcome.album_ids, vec!["al-1".to_string()]);
    assert_eq!(
        outcome.artist_ids,
        vec![
            "ar-main".to_string(),
            "ar-second".to_string(),
            "ar-unknown".to_string(),
            "ar-va".to_string()
        ]
    );
    assert_eq!(
        outcome.media_ids,
        vec!["mf-1".to_string(), "mf-2".to_string()]
    );
    assert_eq!(outcome.inserted_media_ids, outcome.media_ids);
    assert_eq!(outcome.touched_album_ids, vec!["al-old".to_string()]);
    assert_eq!(outcome.missing_rows_marked, 1, "one missing row matched");
    assert_eq!(
        outcome.artwork_rows_deleted, 2,
        "both batch track ids were invalidated"
    );
    assert!(
        !outcome.enqueued_if_missing,
        "full_scan_mode is false → Enqueue"
    );
    assert!(
        outcome.warnings.is_empty(),
        "clean run warns about nothing: {:?}",
        outcome.warnings
    );

    // Go's queue derivation (`:412-425`): 2 queue-worthy artists + 1 album; the Unknown and
    // Various-Artists artists are excluded.
    assert_eq!(
        outcome.queued_item_count, 3,
        "artists minus Unknown/VA, plus the album"
    );

    // --- folder row (`:390`) ---
    assert_eq!(
        cell(&db, "folder", "path", "fo-1").as_deref(),
        Some("album-one")
    );
    assert_eq!(
        cell(&db, "folder", "updated_at", "fo-1").as_deref(),
        Some(time::to_text(&stamps().folder_put).as_str()),
        "the folder stamp is the batch's, not the commit clock"
    );
    assert_eq!(
        cell(&db, "folder", "created_at", "fo-1").as_deref(),
        Some(time::to_text(&seed_stamp()).as_str()),
        "the record's own created_at survives: `put_sql` stamps it only when unset \
         (`folder_repo.rs:859`'s `created_at.or(Some(now))`, Go's put-at-:604 rule)"
    );

    // --- tags (`:396`) : dictionary + library link ---
    let rock = nd_id::new_tag_id("genre", "Rock");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM tag WHERE id = 'tg-whatever'"),
        0
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            &format!("SELECT count(*) FROM tag WHERE id = '{}'", rock)
        ),
        1
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            &format!(
                "SELECT count(*) FROM library_tag WHERE tag_id = '{}' AND library_id = 1",
                rock
            )
        ),
        1,
        "the library_tag link is the second statement of each Add chunk"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            &format!(
                "SELECT count(*) FROM library_tag WHERE tag_id = '{}' AND library_id = 2",
                rock
            ),
        ),
        0,
        "no cross-library link"
    );

    // --- artists (`:403`) + library link (`:408`) ---
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artist"), 4);
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM library_artist WHERE library_id = 1"
        ),
        4
    );
    assert_eq!(
        cell(&db, "artist", "updated_at", "ar-main").as_deref(),
        Some(time::to_text(&stamps().artist_put).as_str()),
        "Artist Put stamps updated_at from the batch"
    );
    assert_eq!(
        cell(&db, "artist", "created_at", "ar-main").as_deref(),
        Some(time::to_text(&stamps().artist_put).as_str()),
        "Go's Put stamps created_at too (artist_repository.go:240)"
    );
    // The restricted column list is Go's; a column OUTSIDE it must keep its stored value.
    assert_eq!(
        cell(&db, "artist", "biography", "ar-main").as_deref(),
        Some(""),
        "biography is not in the Go column list, so an existing value would be preserved"
    );

    // --- album (`:481`) + participants + tags + FTS ---
    assert_eq!(
        cell(&db, "album", "name", "al-1").as_deref(),
        Some("Album-al-1")
    );
    assert_eq!(
        cell(&db, "album", "imported_at", "al-1").as_deref(),
        Some(time::to_text(&stamps().album_put).as_str())
    );
    let album_links = junction_set(
        &db,
        "SELECT artist_id || '/' || role || '/' || sub_role FROM album_artists WHERE album_id = 'al-1'",
    );
    assert_eq!(
        album_links,
        vec![
            "ar-main/albumartist/".to_string(),
            "ar-second/composer/".to_string()
        ]
    );
    assert_eq!(
        junction_set(&db, "SELECT tag_id FROM album_tags WHERE album_id = 'al-1'"),
        vec![rock.clone()],
        "only the indexed name (genre) is junction-indexed"
    );
    assert_eq!(fts_match(&db, "album_fts", "\"Album-al-1\""), 1);
    assert_eq!(
        fts_ids(&db, "album_fts", "album", "\"Album-al-1\""),
        vec!["al-1"]
    );

    // --- media (`:430`) + junctions + FTS ---
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        3,
        "2 new + 1 pre-seeded"
    );
    assert_eq!(
        cell(&db, "media_file", "album_id", "mf-1").as_deref(),
        Some("al-1")
    );
    assert_eq!(
        junction_set(
            &db,
            "SELECT artist_id || '/' || role || '/' || sub_role FROM media_file_artists WHERE media_file_id = 'mf-1'"
        ),
        vec!["ar-main/albumartist/".to_string(), "ar-second/composer/".to_string()]
    );
    assert_eq!(
        junction_set(
            &db,
            "SELECT tag_id FROM media_file_tags WHERE media_file_id = 'mf-2'"
        ),
        vec![rock.clone()]
    );
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf1"), 1);
    assert_eq!(
        fts_ids(&db, "media_file_fts", "media_file", "Zbrfldrmf2"),
        vec!["mf-2"]
    );

    // --- artwork invalidation (`:439`), warn-only arm that SUCCEEDED here ---
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf' AND item_id IN ('mf-1','mf-2')"
        ),
        0,
        "re-imported tracks return to unresolved artwork"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM item_artwork WHERE item_id = 'mf-decoy'"
        ),
        1,
        "non-vacuity: a same-kind row outside the batch survives, so the delete was scoped"
    );

    // --- missing + touch (`:446`, `:456`) ---
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-old'"),
        1
    );
    assert_eq!(
        cell(&db, "media_file", "updated_at", "mf-old").as_deref(),
        Some(time::to_text(&stamps().mark_missing).as_str())
    );
    assert_eq!(
        cell(&db, "album", "imported_at", "al-old").as_deref(),
        Some(time::to_text(&stamps().album_touch).as_str()),
        "Touch uses its OWN stamp, distinct from Album Put's"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-1'"),
        0,
        "the newly imported tracks are not marked missing"
    );

    // --- enqueue (`:466-470`) ---
    let rows = lease(&db, |conn| {
        let queue = ArtworkQueueRepo::new(conn);
        [
            (nd_id::ARTWORK_KIND_ARTIST_PREFIX, "ar-main"),
            (nd_id::ARTWORK_KIND_ARTIST_PREFIX, "ar-second"),
            (nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1"),
        ]
        .iter()
        .map(|(kind, id)| {
            queue
                .get(kind, id, IMAGE_TYPE_PRIMARY)
                .unwrap_or_else(|e| panic!("queue row {kind}/{id}: {e}"))
        })
        .collect::<Vec<_>>()
    });
    assert_eq!(rows.len(), 3);
    for item in &rows {
        assert_eq!(item.priority, SCAN_ARTWORK_PRIORITY);
        assert_eq!(item.attempts, 0);
        assert_eq!(item.retry_at, Some(stamps().artwork));
        assert_eq!(item.enqueued_at, Some(stamps().artwork));
    }
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_kind = 'ar' AND item_id IN ('ar-unknown','ar-va')"
        ),
        0,
        "UnknownArtist/VariousArtists are excluded (`:412`)"
    );

    // A second identical pass is idempotent (Add is ON CONFLICT DO NOTHING, Put takes the
    // UPDATE leg), which is what a retried or repeated scan relies on.
    let second = run_ok(&db, &batch);
    assert!(
        !second.folder_inserted,
        "the second pass takes the UPDATE leg"
    );
    assert_eq!(second.media_ids, outcome.media_ids);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 3);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artist"), 4);

    cleanup(db, path);
}

#[test]
fn folder_retry_reuses_original_album_transition_and_ids() {
    let (db, path) = fresh_busy_store("busy-retry");
    seed_libraries(&db);
    seed_user(&db, "u-one");
    // A real annotation population on the PREVIOUS album id: a transition that failed to be
    // re-applied on the retry attempt would leave this row on `al-prev`.
    seed_annotation(&db, "u-one", "al-prev", 7);

    let mut batch = base_batch();
    batch.album_transitions = vec![AlbumTransition {
        new_id: "al-1".to_string(),
        prev_id: "al-prev".to_string(),
    }];

    // A REAL writer lock, released after ~1.5s so the SECOND attempt succeeds. SQLite is
    // single-writer in delete-journal mode, so the whole first attempt fails at its first
    // write; the production backoff then spaces the rerun by `TX_RETRY_DELAY` (`store.rs:305`).
    let locker_path = dsn(&path);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let locker = std::thread::spawn(move || {
        let mut conn = nd_dbschema::schema::open(&locker_path).unwrap();
        conn.busy_timeout(Duration::ZERO)
            .expect("no busy wait on the locker");
        let tx = conn.transaction().expect("locker transaction");
        tx.execute(
            "INSERT INTO property(id, value) VALUES(?, ?)",
            rusqlite::params!["Db14BusyLock", "held"],
        )
        .expect("the locker must write inside its transaction");
        ready_tx
            .send(())
            .expect("the locker must signal that it holds the write lock");
        std::thread::sleep(Duration::from_millis(1_500));
        drop(tx);
        drop(conn);
    });

    // The lock is really held before the delta starts, so the first attempt's BUSY failure is
    // ordered rather than a race.
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the locker holds the write lock");

    let started = Instant::now();
    let outcome = run_ok(&db, &batch);
    let elapsed = started.elapsed();
    locker.join().expect("the locker thread must finish");

    assert!(
        elapsed >= TX_RETRY_DELAY,
        "the first attempt hit BUSY and the rerun waited the production backoff: {elapsed:?}"
    );
    assert!(
        elapsed < TX_RETRY_DELAY * 2,
        "exactly ONE retry, not two: {elapsed:?}"
    );
    assert!(
        scalar::<i64>(&db, "SELECT count(*) FROM property") == 0,
        "the locker rolled back rather than committed"
    );

    // The batch is immutable, so the transitions are still the originals; the consumed set is
    // derived from THAT copy, not from a map a failed attempt had already drained.
    assert_eq!(
        batch.album_transitions,
        vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-prev".to_string()
        }],
        "the batch itself is unmutated across attempts"
    );
    assert_eq!(
        outcome.consumed_transitions, batch.album_transitions,
        "attempt 2 consumed exactly the ORIGINAL transitions"
    );
    assert_eq!(
        outcome.album_ids,
        vec!["al-1".to_string()],
        "committed album id is SC's"
    );
    assert_eq!(outcome.folder_id, "fo-1");
    assert!(
        outcome.warnings.is_empty(),
        "the retry committed a clean run: {:?}",
        outcome.warnings
    );

    // The annotation really moved to the committed album id — a dropped transition on the
    // rerun would leave it on `al-prev`.
    assert_eq!(
        junction_set(
            &db,
            "SELECT item_id FROM annotation WHERE item_type = 'album' ORDER BY item_id"
        ),
        vec!["al-1".to_string()]
    );
    assert_eq!(
        scalar::<f64>(&db, "SELECT average_rating FROM album WHERE id = 'al-1'"),
        3.0,
        "Go recalculates the TARGET's cached average after the move (sql_annotations.go:195)"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM album"),
        1,
        "no duplicate album row"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 3);

    // The pool survived the contended attempt: another delta on the same store works.
    let (db2_path, db2) = (path.clone(), &db);
    let mut probe = base_batch();
    probe.folder.id = "fo-2".to_string();
    probe.folder.path = "album-two".to_string();
    probe.media = Vec::new();
    probe.albums = Vec::new();
    probe.tags = Vec::new();
    let ok = run_ok(db2, &probe);
    assert!(
        ok.folder_inserted,
        "the pool is un-poisoned and a fresh folder inserts"
    );
    assert_eq!(
        ok.artist_ids.len(),
        4,
        "artist writes still work on the same pool"
    );
    assert_eq!(
        ok.queued_item_count, 2,
        "the two queue-worthy artists, as always"
    );
    let _ = db2_path;

    cleanup(db, path);
}

#[test]
fn full_scan_mode_selects_enqueue_if_missing() {
    let (db, path) = fresh_store("if-missing");
    seed_libraries(&db);
    // `EnqueueIfMissing` excludes any item with an `item_artwork` row of that kind+image_type
    // (`artwork_queue_repository.go:66`), whether resolved or settled-absent.
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1");

    let mut batch = base_batch();
    batch.full_scan_mode = true;
    let outcome = run_ok(&db, &batch);

    assert!(
        outcome.enqueued_if_missing,
        "lib.FullScanInProgress → EnqueueIfMissing (`:467`)"
    );
    assert_eq!(outcome.queued_item_count, 3);
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_kind = 'al' AND item_id = 'al-1'"
        ),
        0,
        "the album already has artwork, so it is NOT queued"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM artwork_queue WHERE item_kind = 'ar'"
        ),
        2,
        "the two queue-worthy artists have no artwork row and ARE queued"
    );

    // Same batch with the flag off: `Enqueue` queues everything, including the album whose
    // artwork already exists, and RESETS the lifecycle.
    let (db2, path2) = fresh_store("if-missing-off");
    seed_libraries(&db2);
    seed_item_artwork(&db2, nd_id::ARTWORK_KIND_ALBUM_PREFIX, "al-1");
    let mut quick = base_batch();
    quick.full_scan_mode = false;
    let quick_outcome = run_ok(&db2, &quick);
    assert!(!quick_outcome.enqueued_if_missing);
    assert_eq!(
        scalar::<i64>(
            &db2,
            "SELECT count(*) FROM artwork_queue WHERE item_kind = 'al' AND item_id = 'al-1'"
        ),
        1,
        "a quick scan re-queues the album regardless of resolved artwork"
    );

    cleanup(db, path);
    cleanup(db2, path2);
}

/// Each warn-only arm poisoned independently: the warning is captured with its stage and
/// cause, and everything ELSE in the same transaction commits.
#[test]
fn warn_only_arms_do_not_abort_the_tx() {
    // Artwork invalidation (`:439`) — Go logs and continues.
    {
        let (db, path) = fresh_store("warn-artwork");
        seed_libraries(&db);
        seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");
        fixture(
            &db,
            "CREATE TRIGGER fixture_item_artwork_delete_fails BEFORE DELETE ON item_artwork \
             FOR EACH ROW WHEN OLD.item_kind = 'mf' \
             BEGIN SELECT RAISE(ABORT, 'fixture: media artwork invalidation fails'); END",
            &[],
        );
        let outcome = run_ok(&db, &base_batch());
        assert_eq!(
            outcome.warnings.len(),
            1,
            "one warning: {:?}",
            outcome.warnings
        );
        let warning = &outcome.warnings[0];
        assert_eq!(warning.stage, FolderWriteStage::ArtworkInvalidate);
        assert_eq!(warning.stage_str(), "artwork_invalidate");
        assert_eq!(
            warning.item_id, "fo-1",
            "the folder-wide arm names the folder"
        );
        assert!(
            matches!(warning.source, SqliteError::SqliteFailure(code, _)
                if code.extended_code == ffi::SQLITE_CONSTRAINT_TRIGGER),
            "the concrete driver cause is retained: {:?}",
            warning.source
        );
        assert_eq!(outcome.artwork_rows_deleted, 0);
        // Everything else committed.
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM album"), 1);
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 3);
        assert_eq!(
            scalar::<i64>(
                &db,
                "SELECT count(*) FROM item_artwork WHERE item_id = 'mf-1'"
            ),
            1,
            "ABORT rolled back only the failing statement, so the row is still there"
        );
        cleanup(db, path);
    }

    // Enqueue (`:470`) — Go's comment says "Never fails the scan."
    {
        let (db, path) = fresh_store("warn-enqueue");
        seed_libraries(&db);
        fixture(
            &db,
            "CREATE TRIGGER fixture_queue_insert_fails BEFORE INSERT ON artwork_queue \
             FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'fixture: artwork enqueue fails'); END",
            &[],
        );
        let outcome = run_ok(&db, &base_batch());
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(outcome.warnings[0].stage, FolderWriteStage::Enqueue);
        assert_eq!(outcome.warnings[0].item_id, "fo-1");
        assert_eq!(
            outcome.queued_item_count, 3,
            "what was handed to the queue is still reported"
        );
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 0);
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM folder WHERE id = 'fo-1'"),
            1
        );
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
        cleanup(db, path);
    }

    // ReassignAnnotation (`:490`).
    {
        let (db, path) = fresh_store("warn-reassign");
        seed_libraries(&db);
        seed_user(&db, "u-one");
        seed_annotation(&db, "u-one", "al-prev", 7);
        fixture(
            &db,
            "CREATE TRIGGER fixture_annotation_update_fails BEFORE UPDATE ON annotation \
             FOR EACH ROW WHEN OLD.item_id = 'al-prev' \
             BEGIN SELECT RAISE(ABORT, 'fixture: annotation reassignment fails'); END",
            &[],
        );
        let mut batch = base_batch();
        batch.album_transitions = vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-prev".to_string(),
        }];
        let outcome = run_ok(&db, &batch);
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        assert_eq!(
            outcome.warnings[0].stage,
            FolderWriteStage::ReassignAnnotation
        );
        assert_eq!(outcome.warnings[0].item_id, "al-1");
        assert_eq!(
            outcome.warnings[0].related_id, "al-prev",
            "the Go log line's from-id"
        );
        assert_eq!(
            junction_set(
                &db,
                "SELECT item_id FROM annotation WHERE item_type = 'album'"
            ),
            vec!["al-prev".to_string()],
            "the move rolled back, so the annotation stayed on the previous id"
        );
        // The rest of the folder still committed, and the transition was still consumed.
        assert_eq!(outcome.consumed_transitions.len(), 1);
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM album WHERE id = 'al-1'"),
            1
        );
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
        cleanup(db, path);
    }

    // CopyAttributes, non-NotFound (`:496`). The trigger fires ONLY on the copy's statement:
    // Album Put's UPDATE leg changes `imported_at` but never `created_at`
    // (`marshal.rs` UPDATE_DROP), and Touch changes `imported_at`, so the guard
    // "created_at changed while imported_at did not" isolates the copy arm — otherwise this
    // would abort the FATAL Album Put arm instead and prove nothing.
    {
        let (db, path) = fresh_store("warn-copy");
        seed_libraries(&db);
        let source_stamp = at(1972, 7, 8, 9, 10, 11);
        seed_album(
            &db,
            &album_record("al-prev", "ar-main", "ar-second", "fo-prev"),
        );
        fixture(
            &db,
            "UPDATE album SET created_at = ? WHERE id = 'al-prev'",
            &[text(time::to_text(&source_stamp))],
        );
        seed_album(&db, &album_record("al-1", "ar-main", "ar-second", "fo-1"));
        let target_before = cell(&db, "album", "created_at", "al-1");
        fixture(
            &db,
            "CREATE TRIGGER fixture_copy_update_fails BEFORE UPDATE ON album \
             FOR EACH ROW WHEN NEW.created_at IS NOT OLD.created_at \
             AND NEW.imported_at IS OLD.imported_at \
             BEGIN SELECT RAISE(ABORT, 'fixture: copy attributes fails'); END",
            &[],
        );
        let mut batch = base_batch();
        batch.album_transitions = vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-prev".to_string(),
        }];
        let outcome = run_ok(&db, &batch);
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        assert_eq!(outcome.warnings[0].stage, FolderWriteStage::CopyAttributes);
        assert_eq!(outcome.warnings[0].item_id, "al-1");
        assert_eq!(outcome.warnings[0].related_id, "al-prev");
        assert_eq!(
            cell(&db, "album", "created_at", "al-1"),
            target_before,
            "the target kept its own created_at"
        );
        // The rest of the folder committed, and the album's OWN stamp survived.
        assert_eq!(
            cell(&db, "album", "imported_at", "al-1").as_deref(),
            Some(time::to_text(&stamps().album_put).as_str()),
            "Album Put ran and committed despite the copy warning"
        );
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 3);
        cleanup(db, path);
    }
}

/// A source row that does not exist is `model.ErrNotFound` and Go ignores it SILENTLY
/// (`:497-500`): no warning at all, and no copy.
#[test]
fn missing_transition_source_produces_no_warning() {
    let (db, path) = fresh_store("copy-notfound");
    seed_libraries(&db);
    let mut batch = base_batch();
    batch.album_transitions = vec![AlbumTransition {
        new_id: "al-1".to_string(),
        prev_id: "al-never-existed".to_string(),
    }];
    let outcome = run_ok(&db, &batch);
    assert!(
        outcome.warnings.is_empty(),
        "ErrNotFound is not a warning (`phase_1_folders.go:497-500`): {:?}",
        outcome.warnings
    );
    assert_eq!(
        outcome.consumed_transitions.len(),
        1,
        "the mapping was still consumed (`:504`)"
    );
    cleanup(db, path);
}

/// Every fatal arm: the whole folder transaction rolls back and NOTHING is committed; the
/// error is typed, Body-stage, and not busy; the same write succeeds once the trigger is
/// dropped (so "nothing committed" cannot mean "nothing ran"), and the pool is un-poisoned.
#[test]
fn fatal_arm_rolls_back_the_whole_folder() {
    struct Case {
        label: &'static str,
        stage: FolderWriteStage,
        ddl: &'static str,
    }
    let cases = [
        Case {
            label: "folder-put",
            stage: FolderWriteStage::FolderPut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON folder FOR EACH ROW \
                  WHEN NEW.id = 'fo-1' BEGIN SELECT RAISE(ABORT, 'fixture: folder put'); END",
        },
        Case {
            label: "tag-add",
            stage: FolderWriteStage::TagAdd,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON tag FOR EACH ROW \
                  BEGIN SELECT RAISE(ABORT, 'fixture: tag add'); END",
        },
        Case {
            label: "library-tag",
            stage: FolderWriteStage::TagAdd,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON library_tag FOR EACH ROW \
                  BEGIN SELECT RAISE(ABORT, 'fixture: library_tag link'); END",
        },
        Case {
            label: "artist-put",
            stage: FolderWriteStage::ArtistPut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON artist FOR EACH ROW \
                  WHEN NEW.id = 'ar-main' BEGIN SELECT RAISE(ABORT, 'fixture: artist put'); END",
        },
        Case {
            label: "add-artist",
            stage: FolderWriteStage::AddArtist,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON library_artist FOR EACH ROW \
                  BEGIN SELECT RAISE(ABORT, 'fixture: add artist'); END",
        },
        Case {
            label: "album-put",
            stage: FolderWriteStage::AlbumPut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON album FOR EACH ROW \
                  WHEN NEW.id = 'al-1' BEGIN SELECT RAISE(ABORT, 'fixture: album put'); END",
        },
        Case {
            label: "album-participants",
            stage: FolderWriteStage::AlbumPut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON album_artists FOR EACH ROW \
                  WHEN NEW.album_id = 'al-1' BEGIN SELECT RAISE(ABORT, 'fixture: participants'); END",
        },
        Case {
            label: "album-tags",
            stage: FolderWriteStage::AlbumPut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON album_tags FOR EACH ROW \
                  WHEN NEW.album_id = 'al-1' BEGIN SELECT RAISE(ABORT, 'fixture: album tags'); END",
        },
        Case {
            label: "mediafile-put",
            stage: FolderWriteStage::MediaFilePut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON media_file FOR EACH ROW \
                  WHEN NEW.id = 'mf-1' BEGIN SELECT RAISE(ABORT, 'fixture: media put'); END",
        },
        Case {
            label: "media-junction",
            stage: FolderWriteStage::MediaFilePut,
            ddl: "CREATE TRIGGER fixture_poison BEFORE INSERT ON media_file_artists FOR EACH ROW \
                  WHEN NEW.media_file_id = 'mf-1' BEGIN SELECT RAISE(ABORT, 'fixture: media junction'); END",
        },
        Case {
            label: "mark-missing",
            stage: FolderWriteStage::MarkMissing,
            ddl: "CREATE TRIGGER fixture_poison BEFORE UPDATE ON media_file FOR EACH ROW \
                  WHEN NEW.missing = 1 BEGIN SELECT RAISE(ABORT, 'fixture: mark missing'); END",
        },
        Case {
            label: "album-touch",
            stage: FolderWriteStage::AlbumTouch,
            ddl: "CREATE TRIGGER fixture_poison BEFORE UPDATE ON album FOR EACH ROW \
                  WHEN NEW.imported_at = '2019-01-02 03:04:10+00:00' \
                  BEGIN SELECT RAISE(ABORT, 'fixture: album touch'); END",
        },
    ];

    for case in cases {
        let (db, path) = fresh_store(case.label);
        seed_libraries(&db);
        assert!(
            case.stage.is_fatal(),
            "{} is a fatal arm per D-BATCH",
            case.label
        );
        fixture(&db, case.ddl, &[]);

        let mut batch = base_batch();
        batch.missing = vec![MissingMedia {
            id: "mf-1".to_string(),
            album_id: "al-1".to_string(),
        }];

        let failed = persist_folder_delta(&db, &batch);
        let err = failed.expect_err(&format!("{} must abort the folder tx", case.label)[..]);
        assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

        assert_everything_empty(&db, case.label);
        assert_eq!(
            fts_match(&db, "album_fts", "\"Album-al-1\""),
            0,
            "no FTS ghost"
        );
        assert_eq!(
            fts_match(&db, "media_file_fts", "Zbrfldrmf1"),
            0,
            "no FTS ghost"
        );
        assert_eq!(
            fts_match(&db, "artist_fts", "\"Main Act\""),
            0,
            "no FTS ghost"
        );
        lease(&db, |conn| {
            assert!(
                conn.is_autocommit(),
                "the lease came back reusable after rollback"
            )
        });

        // Non-vacuity: with the trigger gone the SAME batch commits, so the rollback above is
        // a rollback of work that really ran.
        fixture(&db, "DROP TRIGGER fixture_poison", &[]);
        let ok = run_ok(&db, &batch);
        assert_eq!(
            ok.folder_id, "fo-1",
            "{} reruns clean after the trigger drop",
            case.label
        );
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM folder"), 1);
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM media_file"), 1);

        cleanup(db, path);
    }
}

/// Validation runs BEFORE the pool checkout (`D-BATCH`, pin 4). Proof: a size-1 pool whose
/// only handle is held by another thread, so a checkout-first implementation would block on
/// the pool instead of answering. The rejection arrives immediately, and no row is written.
#[test]
fn empty_id_rejected_before_checkout() {
    // ONE pooled handle, held by another thread for the whole assertion: an implementation
    // that validated AFTER `pool.checkout()` would block on the pool here, so the prompt
    // rejection is the proof that scope validation precedes the checkout (`D-BATCH` pin 4).
    let path = temp_path("pre-checkout");
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 1)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    let db = std::sync::Arc::new(SQLStore::new(pool));
    seed_libraries(&db);

    // Each pair is (flag, condvar). The holder RELEASES its `held` guard before waiting on
    // `release`, otherwise the main thread could never re-acquire it after `Condvar::wait`.
    let held = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let release = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (held_pair, release_pair) = (
        std::sync::Arc::clone(&held),
        std::sync::Arc::clone(&release),
    );

    std::thread::scope(|scope| {
        let holder_db = std::sync::Arc::clone(&db);
        let holder = std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                holder_db
                    .with_connection_sync(|conn| {
                        assert!(conn.is_autocommit(), "the leased handle is idle");
                        {
                            let mut guard = held_pair.0.lock().unwrap();
                            *guard = true;
                            held_pair.1.notify_one();
                        }
                        let mut waiter = release_pair.0.lock().unwrap();
                        while !*waiter {
                            waiter = release_pair.1.wait(waiter).unwrap();
                        }
                        Ok(())
                    })
                    .unwrap_or_else(|e| panic!("holder lease failed: {e}"));
            })
            .expect("spawn the lease holder");

        {
            let mut guard = held.0.lock().unwrap();
            while !*guard {
                guard = held.1.wait(guard).unwrap();
            }
        }

        let mut batch = base_batch();
        batch.folder.id = String::new();
        let answered = std::sync::Arc::new(std::sync::Mutex::new(None::<FolderRunError>));
        let slot = std::sync::Arc::clone(&answered);
        let refused = std::sync::Arc::clone(&db);
        std::thread::Builder::new()
            .spawn_scoped(scope, move || {
                let err = persist_folder_delta(&refused, &batch)
                    .expect_err("an empty folder id is refused");
                *slot.lock().unwrap() = Some(err);
            })
            .expect("spawn the refused call");

        // Far longer than the rejection needs, and shorter than a pool wait would be
        // (busy_timeout is 5s): if validation came after checkout this stays None.
        std::thread::sleep(Duration::from_millis(400));
        let err = answered.lock().unwrap().take().expect(
            "no answer while the sole lease is held: validation would have to wait \
                     for the pool, so it must run BEFORE checkout",
        );
        match err {
            DbRuntimeError::Domain(rejection) => {
                assert_eq!(rejection, ScopeRejection::EmptyFolderId)
            }
            other => panic!("expected a Domain rejection, got {other:?}"),
        }

        {
            let mut guard = release.0.lock().unwrap();
            *guard = true;
            release.1.notify_one();
        }
        holder.join().expect("the holder finishes");
    });

    // The pool is usable again and the refused call wrote nothing.
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM folder"), 0);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library"), 2);
    drop(db);
    std::fs::remove_file(&path).unwrap();
}

/// Scope/ID rejections, each with the arm it must produce, plus the ordering fact that the
/// FIRST structural defect wins deterministically.
#[test]
fn structural_batch_defects_are_refused() {
    let (db, path) = fresh_store("validate");
    seed_libraries(&db);

    let cases: Vec<(FolderWriteBatch, ScopeRejection)> = vec![
        {
            let mut b = base_batch();
            b.artists[0].id = String::new();
            (b, ScopeRejection::EmptyArtistId { index: 0 })
        },
        {
            let mut b = base_batch();
            b.albums[0].id = String::new();
            (b, ScopeRejection::EmptyAlbumId { index: 0 })
        },
        {
            let mut b = base_batch();
            b.folder.library_id = DECOY_LIB;
            (
                b,
                ScopeRejection::FolderLibraryMismatch {
                    found: DECOY_LIB,
                    batch_library: LIB,
                },
            )
        },
        {
            let mut b = base_batch();
            b.albums[0].library_id = DECOY_LIB;
            (
                b,
                ScopeRejection::AlbumLibraryMismatch {
                    index: 0,
                    found: DECOY_LIB,
                    batch_library: LIB,
                },
            )
        },
        {
            let mut b = base_batch();
            b.media[0].library_id = DECOY_LIB;
            (
                b,
                ScopeRejection::MediaLibraryMismatch {
                    index: 0,
                    found: DECOY_LIB,
                    batch_library: LIB,
                },
            )
        },
        {
            let mut b = base_batch();
            b.albums
                .push(album_record("al-1", "ar-main", "ar-second", "fo-1"));
            (
                b,
                ScopeRejection::DuplicateAlbumId {
                    id: "al-1".to_string(),
                },
            )
        },
        {
            let mut b = base_batch();
            b.album_transitions = vec![AlbumTransition {
                new_id: "al-1".to_string(),
                prev_id: String::new(),
            }];
            (
                b,
                ScopeRejection::EmptyAlbumTransition {
                    new_id: "al-1".to_string(),
                },
            )
        },
        {
            let mut b = base_batch();
            b.album_transitions = vec![AlbumTransition {
                new_id: "al-1".to_string(),
                prev_id: "al-1".to_string(),
            }];
            (
                b,
                ScopeRejection::SelfAlbumTransition {
                    new_id: "al-1".to_string(),
                },
            )
        },
        {
            let mut b = base_batch();
            b.album_transitions = vec![AlbumTransition {
                new_id: "al-other".to_string(),
                prev_id: "al-prev".to_string(),
            }];
            (
                b,
                ScopeRejection::UnresolvableAlbumTransition {
                    new_id: "al-other".to_string(),
                },
            )
        },
        {
            let mut b = base_batch();
            b.missing = vec![MissingMedia {
                id: String::new(),
                album_id: "al-1".to_string(),
            }];
            (b, ScopeRejection::EmptyMissingMediaId { index: 0 })
        },
    ];

    for (batch, expected) in &cases {
        assert_eq!(batch.validate(), Err(expected.clone()), "{}", expected);
        let err = persist_folder_delta(&db, batch).expect_err("the batch must be refused");
        match err {
            DbRuntimeError::Domain(rejection) => assert_eq!(rejection, expected.clone()),
            other => panic!("expected a Domain rejection for {expected}, got {other:?}"),
        }
    }
    assert_everything_empty(&db, "every refusal ran no statement at all");
    cleanup(db, path);
}

/// Go's `MarkMissing` quirk (`mediafile_repository.go:463-467`): a chunk that matches zero
/// rows returns SUCCESS and breaks the loop, so LATER chunks are never marked. 201 ids whose
/// FIRST 200 do not exist must leave the 201st untouched.
#[test]
fn missing_media_quirk_zero_chunk() {
    let (db, path) = fresh_store("missing-quirk");
    seed_libraries(&db);
    seed_album(
        &db,
        &album_record("al-old", "ar-main", "ar-second", "fo-old"),
    );
    seed_media(
        &db,
        &media_record("mf-real", "al-old", "fo-old", "old/track-99.mp3"),
    );

    let mut missing: Vec<MissingMedia> = (0..200)
        .map(|i| MissingMedia {
            id: format!("mf-absent-{i}"),
            album_id: "al-old".to_string(),
        })
        .collect();
    missing.push(MissingMedia {
        id: "mf-real".to_string(),
        album_id: "al-old".to_string(),
    });
    assert_eq!(missing.len(), 201);

    // The renderer really splits at 200, so the early return has a later chunk to skip.
    let ids: Vec<String> = missing.iter().map(|m| m.id.clone()).collect();
    assert_eq!(
        mark_missing_sql(true, &ids, &stamps().mark_missing).len(),
        2,
        "`slices.CollectChunks(ids, 200)` (mediafile_repository.go:457): two statements"
    );

    let mut batch = base_batch();
    batch.albums = vec![album_record("al-old", "ar-main", "ar-second", "fo-old")];
    batch.media = Vec::new();
    batch.artists = vec![
        artist_record("ar-main", "Main Act"),
        artist_record("ar-second", "Second Act"),
    ];
    batch.missing = missing;

    let outcome = run_ok(&db, &batch);
    assert_eq!(
        outcome.missing_rows_marked, 0,
        "Go returns NIL after a zero-row chunk, so the accumulated count is 0"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-real'"),
        0,
        "the second chunk never ran: the real row is NOT marked"
    );
    assert_eq!(
        cell(&db, "media_file", "updated_at", "mf-real").as_deref(),
        Some(time::to_text(&seed_stamp()).as_str()),
        "not even re-stamped"
    );
    // Touch still ran for the album set derived from the missing entries (`:450-456`).
    assert_eq!(outcome.touched_album_ids, vec!["al-old".to_string()]);
    assert_eq!(
        cell(&db, "album", "imported_at", "al-old").as_deref(),
        Some(time::to_text(&stamps().album_touch).as_str()),
        "Touch runs regardless of MarkMissing's partial application"
    );

    // Contrast: a first chunk that DOES match marks everything, all 201 rows.
    let (db2, path2) = fresh_store("missing-all");
    seed_libraries(&db2);
    seed_album(
        &db2,
        &album_record("al-old", "ar-main", "ar-second", "fo-old"),
    );
    let all: Vec<MissingMedia> = (0..201)
        .map(|i| {
            let id = format!("mf-{i}");
            seed_media(
                &db2,
                &media_record(&id, "al-old", "fo-old", &format!("old/{id}.mp3")),
            );
            MissingMedia {
                id,
                album_id: "al-old".to_string(),
            }
        })
        .collect();
    let mut batch2 = base_batch();
    batch2.albums = vec![album_record("al-old", "ar-main", "ar-second", "fo-old")];
    batch2.media = Vec::new();
    batch2.artists = vec![
        artist_record("ar-main", "Main Act"),
        artist_record("ar-second", "Second Act"),
    ];
    batch2.missing = all;
    let outcome2 = run_ok(&db2, &batch2);
    assert_eq!(outcome2.missing_rows_marked, 201, "both chunks ran");
    assert_eq!(
        scalar::<i64>(&db2, "SELECT count(*) FROM media_file WHERE missing = 1"),
        201
    );

    cleanup(db, path);
    cleanup(db2, path2);
}

/// Chunk boundaries of the two chunked arms, asserted as EXECUTED rows beside the renderer's
/// leg counts (`CHUNK_SIZE` 100 for the queue, `add_sql`'s 200 for tags), including the empty
/// case running on an UNMIGRATED DB where any statement at all would fail.
#[test]
fn queue_and_tag_chunk_boundaries() {
    // Renderer leg counts (SOURCE-VERIFIED chunk constants, EXECUTED renderers).
    let items: Vec<_> = (0..101)
        .map(|i| scan_artwork_item(nd_id::ARTWORK_KIND_ARTIST_PREFIX, &format!("q-{i}"), 50))
        .collect();
    assert_eq!(
        nd_db::artwork_queue_repo::enqueue_sql(
            &items,
            nd_db::artwork_queue_repo::CONFLICT_ENQUEUE,
            &stamps().artwork
        )
        .len(),
        2,
        "enqueueChunkSize 100 (`artwork_queue_repository.go:18,152`)"
    );
    let tags: Vec<Tag> = (0..201)
        .map(|i| Tag {
            id: nd_id::new_tag_id("genre", &format!("G{i}")),
            tag_name: "genre".to_string(),
            tag_value: format!("G{i}"),
            ..Tag::default()
        })
        .collect();
    assert_eq!(
        add_sql(LIB, &tags).len(),
        2,
        "`slices.Chunk(tags, 200)` (tag_repository.go:27)"
    );
    assert_eq!(
        add_sql(LIB, &[]).len(),
        0,
        "an empty tag list renders nothing"
    );

    // Empty input on an UNMIGRATED DB: no statement at all, so no `no such table` error.
    {
        let path = temp_path("unmigrated");
        let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
        pool.put_back(first).expect("return configured handle");
        let db = SQLStore::new(pool);
        let mut batch = base_batch();
        batch.tags = Vec::new();
        batch.artists = Vec::new();
        batch.albums = Vec::new();
        batch.media = Vec::new();
        let err = persist_folder_delta(&db, &batch)
            .expect_err("an unmigrated DB cannot answer even the folder statement");
        assert!(
            matches!(err, DbRuntimeError::Sqlite { .. }),
            "the failure is the missing table, not a validation refusal: {err}"
        );
        drop(db);
        std::fs::remove_file(&path).unwrap();
    }

    // Executed 0 / 1 / 100 / 101 queue rows and 0 / 1 / 200 / 201 tag rows.
    for count in [0usize, 1, 100, 101] {
        let (db, path) = fresh_store(&format!("queue-{count}"));
        seed_libraries(&db);
        let mut batch = base_batch();
        batch.tags = Vec::new();
        batch.albums = Vec::new();
        batch.media = Vec::new();
        batch.artists = (0..count)
            .map(|i| artist_record(&format!("ar-{i}"), &format!("Artist {i}")))
            .collect();
        let outcome = run_ok(&db, &batch);
        assert_eq!(outcome.queued_item_count, count);
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"),
            i64::try_from(count).unwrap(),
            "{count} queue rows committed"
        );
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM artist"),
            i64::try_from(count).unwrap()
        );
        cleanup(db, path);
    }

    for count in [0usize, 1, 200, 201] {
        let (db, path) = fresh_store(&format!("tags-{count}"));
        seed_libraries(&db);
        let mut batch = base_batch();
        batch.tags = (0..count)
            .map(|i| Tag {
                id: nd_id::new_tag_id("genre", &format!("G{i}")),
                tag_name: "genre".to_string(),
                tag_value: format!("G{i}"),
                ..Tag::default()
            })
            .collect();
        batch.albums = Vec::new();
        batch.media = Vec::new();
        batch.artists = Vec::new();
        let outcome = run_ok(&db, &batch);
        assert!(outcome.warnings.is_empty());
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM tag"),
            i64::try_from(count).unwrap()
        );
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM library_tag"),
            i64::try_from(count).unwrap(),
            "each chunk's second statement writes the library_tag links"
        );
        cleanup(db, path);
    }
}

/// The source-labeled phase-order manifest (`D-BATCH`, item 7): the arms this transaction
/// executes, in `scanner/phase_1_folders.go` order, read back from a test-only audit table
/// that AFTER-triggers fill in statement order. This is the ordering oracle — a reordered
/// sequence or a dropped arm changes the audited list, and a rolled-back attempt leaves the
/// audit table empty, which also proves the audit rows are transactional rather than evidence
/// of statements that never committed.
#[test]
fn phase_order_manifest_matches_go_sequence() {
    let (db, path) = fresh_store("phase-order");
    seed_libraries(&db);
    seed_user(&db, "u-one");

    // A real transition source with an annotation population, so the two warn-only album
    // arms actually execute rather than short-circuit.
    seed_album(
        &db,
        &album_record("al-prev", "ar-main", "ar-second", "fo-prev"),
    );
    // A source `created_at` DISTINCT from the target's stored value: `al-1`'s INSERT stores
    // `seed_stamp()`, so an equal value would make the copy's UPDATE a no-op change and the
    // audit trigger below would not fire.
    fixture(
        &db,
        "UPDATE album SET created_at = ? WHERE id = 'al-prev'",
        &[text(time::to_text(&at(1972, 7, 8, 9, 10, 11)))],
    );
    seed_annotation(&db, "u-one", "al-prev", 4);
    // A pre-existing track and album for the MarkMissing/Touch arms, neither of which is in
    // the batch's album list, so `imported_at` changes ONLY through Touch.
    seed_album(
        &db,
        &album_record("al-old", "ar-main", "ar-second", "fo-old"),
    );
    seed_media(
        &db,
        &media_record("mf-old", "al-old", "fo-old", "old/track-01.mp3"),
    );
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");

    fixture(
        &db,
        "CREATE TABLE fixture_order (seq INTEGER PRIMARY KEY, arm TEXT NOT NULL)",
        &[],
    );
    for (sql, arm) in [
        ("INSERT ON folder", "folder_put"),
        ("INSERT ON tag", "tag_add"),
        ("INSERT ON artist", "artist_put"),
        ("INSERT ON library_artist", "add_artist"),
        ("INSERT ON album", "album_put"),
        ("UPDATE ON annotation", "reassign_annotation"),
        (
            "UPDATE ON album FOR EACH ROW WHEN NEW.created_at IS NOT OLD.created_at",
            "copy_attributes",
        ),
        ("INSERT ON media_file", "media_file_put"),
        ("DELETE ON item_artwork", "artwork_invalidate"),
        (
            "UPDATE ON media_file FOR EACH ROW WHEN NEW.missing = 1",
            "mark_missing",
        ),
        (
            "UPDATE ON album FOR EACH ROW WHEN NEW.imported_at IS NOT OLD.imported_at",
            "album_touch",
        ),
        ("INSERT ON artwork_queue", "enqueue"),
    ] {
        fixture(
            &db,
            &format!(
                "CREATE TRIGGER fixture_audit_{arm} AFTER {sql} BEGIN \
                 INSERT INTO fixture_order(arm) VALUES ('{arm}'); END"
            ),
            &[],
        );
    }

    let mut batch = base_batch();
    batch.album_transitions = vec![AlbumTransition {
        new_id: "al-1".to_string(),
        prev_id: "al-prev".to_string(),
    }];
    batch.missing = vec![MissingMedia {
        id: "mf-old".to_string(),
        album_id: "al-old".to_string(),
    }];

    let outcome = run_ok(&db, &batch);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    // First-occurrence order per arm: Go's manifest, arms 1-12.
    let manifest = lease(&db, |conn| {
        let mut statement = conn
            .prepare(
                "SELECT arm FROM fixture_order o WHERE seq = \
                 (SELECT min(seq) FROM fixture_order i WHERE i.arm = o.arm) ORDER BY seq",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<String>>()
    });
    assert_eq!(
        manifest,
        vec![
            "folder_put".to_string(),
            "tag_add".to_string(),
            "artist_put".to_string(),
            "add_artist".to_string(),
            "album_put".to_string(),
            "reassign_annotation".to_string(),
            "copy_attributes".to_string(),
            "media_file_put".to_string(),
            "artwork_invalidate".to_string(),
            "mark_missing".to_string(),
            "album_touch".to_string(),
            "enqueue".to_string(),
        ],
        "the executed arm order is `phase_1_folders.go`'s, including the two warn-only album \
         arms inside the album loop (before the track loop) and artwork invalidation AFTER the \
         track loop"
    );

    // The FK graph would make several of these transitions impossible in a different order
    // (`library_artist.artist_id REFERENCES artist(id)`, `schema_ddl.rs:62`), so the manifest
    // is not merely a convention here.
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM fixture_order WHERE arm = 'enqueue'"
        ),
        3,
        "one audited row per queued item"
    );

    // A rolled-back attempt leaves NO audit rows and changes no state — the arms that ran
    // BEFORE the failure committed nothing.
    let baseline = state(&db);
    fixture(&db, "DELETE FROM fixture_order", &[]);
    fixture(
        &db,
        "CREATE TRIGGER fixture_poison BEFORE UPDATE ON media_file FOR EACH ROW \
         WHEN NEW.id = 'mf-1' BEGIN SELECT RAISE(ABORT, 'fixture: the track arm fails'); END",
        &[],
    );
    let err = persist_folder_delta(&db, &batch).expect_err("the track arm aborts the tx");
    assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM fixture_order"),
        0,
        "the audit rows rolled back with everything else"
    );
    assert_state_unchanged(
        &db,
        &baseline,
        "the folder arms that ran before the failure committed nothing",
    );

    cleanup(db, path);
}

/// The media arm's identity choice, per record: Go's `Put` builds the `Eq{path, library_id}`
/// filter (`mediafile_repository.go:189`) but `putByMatch` short-circuits the probe when the
/// record already carries an id (`sql_base_repository.go:578-580`). So a record WITH an id is
/// written under that id even when another row holds the same `path`+`library_id`, and a
/// record WITHOUT one reuses the found row. Both directions are asserted, because passing
/// `Match` unconditionally would silently merge the first case into the second.
#[test]
fn media_identity_arms_are_chosen_per_record() {
    let (db, path) = fresh_store("media-identity");
    seed_libraries(&db);
    // A row occupying the same path+library as the batch's track, under a different id.
    seed_media(
        &db,
        &media_record("mf-other", "al-1", "fo-1", "album-one/track-01.mp3"),
    );

    // (a) A record that HAS an id: direct put, no probe, so `mf-other` is untouched and a
    // second row exists at the same path.
    let mut with_id = base_batch();
    let outcome = run_ok(&db, &with_id);
    assert_eq!(outcome.media_ids, vec!["mf-1".to_string()]);
    assert_eq!(outcome.inserted_media_ids, vec!["mf-1".to_string()]);
    assert_eq!(
        junction_set(
            &db,
            "SELECT id FROM media_file WHERE path = 'album-one/track-01.mp3' ORDER BY id"
        ),
        vec!["mf-1".to_string(), "mf-other".to_string()],
        "`Id` skipped the probe, so both rows exist at the same path"
    );

    // (b) The same record WITHOUT an id: the probe finds `mf-1` (first match of the same
    // path) and reuses it rather than minting a new id.
    let (db2, path2) = fresh_store("media-identity-probe");
    seed_libraries(&db2);
    seed_media(
        &db2,
        &media_record("mf-existing", "al-1", "fo-1", "album-one/track-01.mp3"),
    );
    with_id.media[0].id = String::new();
    let reused = run_ok(&db2, &with_id);
    assert_eq!(
        reused.media_ids,
        vec!["mf-existing".to_string()],
        "empty id + existing path => the probe's id, not a minted one"
    );
    assert!(
        reused.inserted_media_ids.is_empty(),
        "the reuse took the UPDATE leg"
    );
    assert_eq!(scalar::<i64>(&db2, "SELECT count(*) FROM media_file"), 1);

    cleanup(db, path);
    cleanup(db2, path2);
}

/// A `RAISE(ROLLBACK)` inside a warn-only arm ENDS the transaction, so the remaining arms
/// would run in AUTOCOMMIT mode and stay durable even though the executor's later `COMMIT`
/// fails. `D-BATCH`: a transaction-aborted failure is always fatal, so the arm must return
/// the original typed failure rather than degrade it to a warning. Each of the four warn arms
/// is injected independently, and after each one the FULL state read-back proves that nothing
/// after the injected point changed. A warning-carrying outcome would mean a commit happened,
/// so these cases assert `Err` and never receive a [`CommittedFolderOutcome`] at all.
#[test]
fn lost_transaction_in_a_warn_arm_is_fatal_and_leaks_nothing() {
    struct Case {
        label: &'static str,
        stage: FolderWriteStage,
        ddl: &'static str,
        /// An arm that runs AFTER the injected one, whose effect must be absent.
        after_note: &'static str,
    }
    let cases = [
        Case {
            label: "lost-tx-reassign",
            stage: FolderWriteStage::ReassignAnnotation,
            ddl: "CREATE TRIGGER fixture_lost BEFORE UPDATE ON annotation FOR EACH ROW \
                  WHEN OLD.item_id = 'al-prev' \
                  BEGIN SELECT RAISE(ROLLBACK, 'fixture: transaction lost in reassign'); END",
            after_note:
                "copy attributes, media put, artwork, missing, touch, enqueue all ran later",
        },
        Case {
            label: "lost-tx-copy",
            stage: FolderWriteStage::CopyAttributes,
            ddl: "CREATE TRIGGER fixture_lost BEFORE UPDATE ON album FOR EACH ROW \
                  WHEN NEW.created_at IS NOT OLD.created_at AND NEW.imported_at IS OLD.imported_at \
                  BEGIN SELECT RAISE(ROLLBACK, 'fixture: transaction lost in copy'); END",
            after_note: "media put, artwork, missing, touch and enqueue all ran later",
        },
        Case {
            label: "lost-tx-artwork",
            stage: FolderWriteStage::ArtworkInvalidate,
            ddl: "CREATE TRIGGER fixture_lost BEFORE DELETE ON item_artwork FOR EACH ROW \
                  WHEN OLD.item_kind = 'mf' \
                  BEGIN SELECT RAISE(ROLLBACK, 'fixture: transaction lost in artwork'); END",
            after_note: "MarkMissing, Touch and enqueue all ran later",
        },
        Case {
            label: "lost-tx-enqueue",
            stage: FolderWriteStage::Enqueue,
            ddl: "CREATE TRIGGER fixture_lost BEFORE INSERT ON artwork_queue FOR EACH ROW \
                  BEGIN SELECT RAISE(ROLLBACK, 'fixture: transaction lost in enqueue'); END",
            after_note: "the commit that follows it",
        },
    ];

    for case in cases {
        assert!(
            !case.stage.is_fatal(),
            "{} is a warn-only arm before the loss is detected",
            case.stage.as_str()
        );
        let (db, path) = fresh_store(case.label);
        seed_libraries(&db);
        seed_user(&db, "u-one");
        let source_stamp = at(1972, 7, 8, 9, 10, 11);
        seed_album(
            &db,
            &album_record("al-prev", "ar-main", "ar-second", "fo-prev"),
        );
        fixture(
            &db,
            "UPDATE album SET created_at = ? WHERE id = 'al-prev'",
            &[text(time::to_text(&source_stamp))],
        );
        seed_annotation(&db, "u-one", "al-prev", 4);
        // A pre-existing album + track outside the batch's album list, so the MarkMissing and
        // Touch arms have real rows to leak into if the transaction were gone.
        seed_album(
            &db,
            &album_record("al-old", "ar-main", "ar-second", "fo-old"),
        );
        seed_media(
            &db,
            &media_record("mf-old", "al-old", "fo-old", "old/track-01.mp3"),
        );
        seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");

        fixture(&db, case.ddl, &[]);
        let baseline = state(&db);
        let touch_before = cell(&db, "album", "imported_at", "al-old");
        let missing_before =
            scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-old'");

        let mut batch = base_batch();
        batch.album_transitions = vec![AlbumTransition {
            new_id: "al-1".to_string(),
            prev_id: "al-prev".to_string(),
        }];
        batch.missing = vec![MissingMedia {
            id: "mf-old".to_string(),
            album_id: "al-old".to_string(),
        }];

        let result = persist_folder_delta(&db, &batch);
        let err = match result {
            Ok(outcome) => panic!(
                "{}: a lost transaction must NOT produce an outcome ({} warnings captured) — \
                 a warning-carrying outcome implies the commit succeeded",
                case.label,
                outcome.warnings.len()
            ),
            Err(err) => err,
        };
        assert_body_failure(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));

        // Nothing after the injected point survived, and nothing before it committed either.
        assert_state_unchanged(&db, &baseline, case.after_note);
        assert_eq!(
            cell(&db, "album", "imported_at", "al-old"),
            touch_before,
            "{}: the Touch arm leaked no stamp",
            case.label
        );
        assert_eq!(
            scalar::<i64>(&db, "SELECT missing FROM media_file WHERE id = 'mf-old'"),
            missing_before,
            "{}: the MarkMissing arm leaked no missing flag",
            case.label
        );
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"),
            0,
            "{}: no autocommitted queue rows",
            case.label
        );
        assert_eq!(
            scalar::<i64>(&db, "SELECT count(*) FROM folder WHERE id = 'fo-1'"),
            0,
            "{}: the folder row is rolled back, not autocommitted",
            case.label
        );
        assert_eq!(
            junction_set(
                &db,
                "SELECT item_id FROM annotation WHERE item_type = 'album'"
            ),
            vec!["al-prev".to_string()],
            "{}: no leaked annotation move",
            case.label
        );
        assert_eq!(
            cell_row(&db, "album", "created_at", "al-1"),
            None,
            "{}: no album row at all, so no leaked copy either",
            case.label
        );
        lease(&db, |conn| {
            assert!(
                conn.is_autocommit(),
                "{}: the lease came back reusable (un-poisoned)",
                case.label
            )
        });

        // The pool is usable and the SAME batch commits once the loss trigger is gone, so the
        // fatal answer above is a real rejection of work that ran, not a dead pool.
        fixture(&db, "DROP TRIGGER fixture_lost", &[]);
        let outcome = run_ok(&db, &batch);
        assert!(
            outcome.warnings.is_empty(),
            "{} reruns clean after the trigger drop: {:?}",
            case.label,
            outcome.warnings
        );
        assert_eq!(outcome.missing_rows_marked, 1);
        assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artwork_queue"), 3);
        assert_eq!(
            junction_set(
                &db,
                "SELECT item_id FROM annotation WHERE item_type = 'album'"
            ),
            vec!["al-1".to_string()]
        );

        cleanup(db, path);
    }
}

/// State read-back of the two warn-only album arms' OBSERVABLE effects, used to compare a
/// retry-after-commit-loss run against a clean no-fault run.
fn transition_state(store: &SQLStore) -> Vec<String> {
    let annotations = junction_set(
        store,
        "SELECT item_id || '/' || play_count || '/' || rating || '/' || starred \
         FROM annotation WHERE item_type = 'album' ORDER BY item_id",
    );
    let mut out = vec![format!(
        "album al-1 created_at = {:?}",
        cell_row(store, "album", "created_at", "al-1")
    )];
    out.push(format!(
        "album al-prev created_at = {:?}",
        cell_row(store, "album", "created_at", "al-prev")
    ));
    out.push(format!(
        "al-1 average_rating = {}",
        scalar::<f64>(store, "SELECT average_rating FROM album WHERE id = 'al-1'")
    ));
    out.extend(annotations);
    out.extend(state(store).iter().map(|(t, c)| format!("{t}={c}")));
    out
}

/// A busy failure at the COMMIT point — i.e. AFTER the album loop consumed its transitions —
/// must replay the ORIGINAL map on the retry, not the drained one.
///
/// The injection is a READ transaction on a second connection: a reader holds a shared lock,
/// which permits our writes but blocks the journal upgrade at `COMMIT`, so attempt 1 runs the
/// whole body (including `reassign_annotation_in_tx` and the `created_at` copy, both of which
/// drain the per-attempt clone) and then loses. A writer-lock injection cannot prove this —
/// it fails at the FIRST write, before any consumption, and would also pass if the map were
/// shared and drained. The commit-stage placement is itself EXECUTED as a control below.
#[test]
fn folder_retry_after_commit_loss_reapplies_consumed_transitions() {
    let (db, path) = fresh_busy_store("commit-loss-retry");
    seed_libraries(&db);
    seed_user(&db, "u-one");
    let source_stamp = at(1972, 7, 8, 9, 10, 11);
    seed_album(
        &db,
        &album_record("al-prev", "ar-main", "ar-second", "fo-prev"),
    );
    fixture(
        &db,
        "UPDATE album SET created_at = ? WHERE id = 'al-prev'",
        &[text(time::to_text(&source_stamp))],
    );
    seed_annotation(&db, "u-one", "al-prev", 7);

    let mut batch = base_batch();
    batch.album_transitions = vec![AlbumTransition {
        new_id: "al-1".to_string(),
        prev_id: "al-prev".to_string(),
    }];

    // The reference: the SAME batch and seeds with no lock at all.
    let (clean, clean_path) = fresh_store("commit-loss-clean");
    seed_libraries(&clean);
    seed_user(&clean, "u-one");
    seed_album(
        &clean,
        &album_record("al-prev", "ar-main", "ar-second", "fo-prev"),
    );
    fixture(
        &clean,
        "UPDATE album SET created_at = ? WHERE id = 'al-prev'",
        &[text(time::to_text(&source_stamp))],
    );
    seed_annotation(&clean, "u-one", "al-prev", 7);
    let clean_outcome = run_ok(&clean, &batch);
    let reference = transition_state(&clean);
    assert_eq!(
        clean_outcome.consumed_transitions, batch.album_transitions,
        "the clean run consumed the map, which is what the retried run must match"
    );
    assert!(
        reference.contains(&format!(
            "album al-1 created_at = Some(Some({time:?}))",
            time = time::to_text(&source_stamp)
        )),
        "the clean run really copies the source's created_at: {reference:?}"
    );

    // A reader on a second connection, held until released.
    let reader_path = dsn(&path);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let reader = std::thread::spawn(move || {
        let conn = nd_dbschema::schema::open(&reader_path).unwrap();
        conn.busy_timeout(Duration::ZERO)
            .expect("no busy wait on the reader");
        let mut statement = conn
            .prepare_cached("SELECT count(*) FROM album")
            .expect("read statement");
        conn.execute("BEGIN", [])
            .expect("the reader must open a read transaction");
        statement
            .query_row([], |row| row.get::<_, i64>(0))
            .expect("the read transaction must hold the shared lock");
        ready_tx
            .send(())
            .expect("the reader must announce it holds the shared lock");
        // Released by the timer thread, well before the retry's own commit.
        let _ = release_rx.recv_timeout(Duration::from_secs(30));
        conn.execute("COMMIT", []).expect("release the reader lock");
        drop(statement);
        drop(conn);
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the reader holds its shared lock");

    // CONTROL: with only a reader held, a write transaction's BODY succeeds and the failure
    // lands at COMMIT — this is what makes "attempt 1 died after consuming the map" a proven
    // fact rather than an assumption.
    let control = db.try_with_tx_sync::<i64, Infallible>(|tx| {
        nd_db::execution::execute(
            tx,
            &(
                "INSERT INTO property(id, value) VALUES('db14-ctl', 'x')".to_string(),
                Vec::new(),
            ),
        )
        .map_err(DbOperationError::sqlite)
        .map(|_| 1)
    });
    match control {
        Err(DbRuntimeError::Sqlite { stage, source }) => {
            assert_eq!(
                stage,
                TransactionStage::Commit,
                "a held reader blocks the COMMIT, not the body"
            );
            assert!(
                super_is_busy(&source),
                "the commit failure is busy-class, so the executor retries it: {source:?}"
            );
        }
        Err(other) => panic!("expected a busy Commit-stage failure, got {other:?}"),
        Ok(_) => panic!("a held reader must block the commit"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM property"),
        0,
        "the control's body rolled back rather than autocommitting"
    );

    // Release the reader shortly after the delta starts: attempt 1 fails at its commit
    // immediately (busy_timeout = 0), waits the production 5s, and attempt 2 finds no reader.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_200));
        let _ = release_tx.send(());
    });

    let started = Instant::now();
    let outcome = run_ok(&db, &batch);
    let elapsed = started.elapsed();
    reader.join().expect("the reader thread finishes");

    assert!(
        elapsed >= TX_RETRY_DELAY && elapsed < TX_RETRY_DELAY * 2,
        "exactly ONE retry, spaced by the production backoff: {elapsed:?}"
    );

    // Attempt 1 had already DRAINED its clone before losing, so only a per-attempt clone can
    // produce this: the consumed set is the original map, not an empty or partial one.
    assert_eq!(
        outcome.consumed_transitions, batch.album_transitions,
        "the retry consumed the ORIGINAL transitions, not the drained map"
    );
    assert_eq!(batch.album_transitions.len(), 1, "the batch is unmutated");
    assert!(
        outcome.warnings.is_empty(),
        "the retry committed a clean run: {:?}",
        outcome.warnings
    );

    // Byte-for-byte against the no-fault run: annotation population, both `created_at` values,
    // the recalculated average, and every table's row count.
    assert_eq!(
        transition_state(&db),
        reference,
        "the retried run's transition effects differ from a clean run"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "al-1").as_deref(),
        Some(time::to_text(&source_stamp).as_str()),
        "the created_at copy was re-applied on the retry"
    );
    assert_eq!(
        junction_set(
            &db,
            "SELECT item_id FROM annotation WHERE item_type = 'album'"
        ),
        vec!["al-1".to_string()],
        "the annotation reassignment was re-applied on the retry"
    );

    cleanup(clean, clean_path);
    cleanup(db, path);
}

/// `store::is_busy`, reached through the public alias rather than re-derived here.
#[must_use]
fn super_is_busy(err: &SqliteError) -> bool {
    nd_db::store::is_busy(err)
}

/// The executed `copy_album_created_at_in_tx` helper: the `0001-`/empty/NULL skip rules, the
/// NotFound arm, and a real copy read back byte-identically.
#[test]
fn copy_created_at_helper_skip_rules() {
    let (db, path) = fresh_store("copy-helper");
    seed_libraries(&db);

    // A real source value, stored with the port's exact TEXT spelling.
    let real = at(1974, 3, 1, 2, 3, 4);
    seed_album(
        &db,
        &album_record("src-real", "ar-main", "ar-second", "fo-1"),
    );
    fixture(
        &db,
        "UPDATE album SET created_at = ? WHERE id = 'src-real'",
        &[text(time::to_text(&real))],
    );
    seed_album(&db, &album_record("tgt-1", "ar-main", "ar-second", "fo-1"));
    let copied = db
        .try_with_tx_sync::<CopyAlbumCreatedAtOutcome, Infallible>(|tx| {
            copy_album_created_at_in_tx(tx, "src-real", "tgt-1")
        })
        .unwrap_or_else(|e| panic!("copy from a real source failed: {e}"));
    assert_eq!(copied, CopyAlbumCreatedAtOutcome::Copied);
    assert_eq!(
        cell(&db, "album", "created_at", "tgt-1").as_deref(),
        Some(time::to_text(&real).as_str()),
        "the stored TEXT round-trips unchanged (Go casts to text for exactly this reason)"
    );
    assert_eq!(
        cell(&db, "album", "created_at", "src-real").as_deref(),
        Some(time::to_text(&real).as_str()),
        "the source is never written"
    );

    // Poison spellings, each independently skipped: NULL, empty, `0001-` prefix.
    for (label, stored) in [
        ("null", None::<String>),
        ("empty", Some(String::new())),
        ("zero", Some(time::to_text(&crate_time_zero()))),
    ] {
        let id = format!("src-{label}");
        seed_album(&db, &album_record(&id, "ar-main", "ar-second", "fo-1"));
        if let Some(value) = &stored {
            fixture(
                &db,
                "UPDATE album SET created_at = ? WHERE id = ?",
                &[text(value), text(&id)],
            );
        } else {
            fixture(
                &db,
                "UPDATE album SET created_at = NULL WHERE id = ?",
                &[text(&id)],
            );
        }
        let target = format!("tgt-{label}");
        seed_album(&db, &album_record(&target, "ar-main", "ar-second", "fo-1"));
        let before = cell(&db, "album", "created_at", &target);
        let outcome = db
            .try_with_tx_sync::<CopyAlbumCreatedAtOutcome, Infallible>(|tx| {
                copy_album_created_at_in_tx(tx, &id, &target)
            })
            .unwrap_or_else(|e| panic!("copy from a poisoned source ({label}) failed: {e}"));
        assert_eq!(
            outcome,
            CopyAlbumCreatedAtOutcome::SkippedPoisonedSource,
            "`album_repository.go:343-350` skips {label}"
        );
        assert_eq!(
            cell(&db, "album", "created_at", &target),
            before,
            "no UPDATE ran for {label}: the target keeps its own created_at"
        );
    }

    // Missing source → SourceNotFound (the arm the folder tx ignores), not an error.
    let missing = db
        .try_with_tx_sync::<CopyAlbumCreatedAtOutcome, Infallible>(|tx| {
            copy_album_created_at_in_tx(tx, "no-such-album", "tgt-1")
        })
        .unwrap_or_else(|e| panic!("a missing source must not error: {e}"));
    assert_eq!(missing, CopyAlbumCreatedAtOutcome::SourceNotFound);
    assert_eq!(
        cell(&db, "album", "created_at", "tgt-1").as_deref(),
        Some(time::to_text(&real).as_str()),
        "the earlier copy survived the NotFound call"
    );

    // A missing TARGET is a zero-row UPDATE and still success, exactly like Go (executeSQL's
    // count is discarded for this caller).
    let no_target = db
        .try_with_tx_sync::<CopyAlbumCreatedAtOutcome, Infallible>(|tx| {
            copy_album_created_at_in_tx(tx, "src-real", "no-such-album")
        })
        .unwrap_or_else(|e| panic!("a missing target must not error: {e}"));
    assert_eq!(no_target, CopyAlbumCreatedAtOutcome::Copied);

    // The helper executes the two existing renderers rather than a second statement pair.
    let (read_sql, read_args) = nd_db::album_repo::copy_attributes_read_sql(
        "src-real",
        &[COPY_CREATED_AT_COLUMN.to_string()],
    );
    assert!(read_sql.contains("cast(created_at as text) as created_at"));
    assert_eq!(read_args, vec![Value::Text("src-real".to_string())]);

    cleanup(db, path);
}

/// `crate::time::zero()` — Go's zero `time.Time`, stored as a `0001-`-prefixed TEXT.
#[must_use]
fn crate_time_zero() -> DateTime<Utc> {
    time::zero()
}

/// Nothing is published before commit: a failed attempt returns `Err` with no outcome, leaves
/// no row under a minted id, and the committed outcome carries the full affected sets.
#[test]
fn no_publication_before_commit() {
    let (db, path) = fresh_store("no-publication");
    seed_libraries(&db);
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-1");
    let baseline = state(&db);

    // A media record with an EMPTY id: Go's `putByMatch` miss mints `nd_id::new_random()`
    // INSIDE the transaction (`sql_base_repository.go:635-638`). Poison the participants
    // INSERT so the mint happens and then the transaction dies.
    fixture(
        &db,
        "CREATE TRIGGER fixture_mf_junction_fails BEFORE INSERT ON media_file_artists \
         FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'fixture: minted id never commits'); END",
        &[],
    );
    let mut batch = base_batch();
    batch.media[0].id = String::new();
    let failed = persist_folder_delta(&db, &batch).expect_err("the junction leg aborts the tx");
    assert_body_failure(&failed, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
    assert_state_unchanged(&db, &baseline, "a rolled-back attempt publishes nothing");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        0,
        "the minted id left no row behind"
    );
    assert_eq!(fts_match(&db, "media_file_fts", "Zbrfldrmf1"), 0);

    fixture(&db, "DROP TRIGGER fixture_mf_junction_fails", &[]);
    let outcome = run_ok(&db, &batch);
    assert_eq!(outcome.media_ids.len(), 1, "one committed id is published");
    let committed = &outcome.media_ids[0];
    assert!(!committed.is_empty(), "the committed id is the minted one");
    assert_ne!(committed, &String::new());
    assert_eq!(
        junction_set(
            &db,
            &format!(
                "SELECT id FROM media_file WHERE path = '{}' AND library_id = 1",
                batch.media[0].path
            )
        ),
        vec![committed.clone()],
        "the published id is exactly the row that exists"
    );
    assert_eq!(outcome.inserted_media_ids, vec![committed.clone()]);
    assert_eq!(outcome.artist_ids.len(), 4);
    assert_eq!(outcome.album_ids.len(), 1);
    assert_eq!(outcome.folder_id, "fo-1");

    // A second pass on the same path REUSES the committed id (Go's putByMatch identity), so
    // the published id is durable rather than re-minted per scan.
    let again = run_ok(&db, &batch);
    assert_eq!(
        again.media_ids,
        vec![committed.clone()],
        "stable id across passes"
    );
    assert!(
        again.inserted_media_ids.is_empty(),
        "the second pass took the UPDATE leg"
    );

    cleanup(db, path);
}

/// Non-vacuity for the ORDER of the sequence and for the ids the artwork arm keys on: the
/// arm must run AFTER the media loop and against the COMMITTED ids. A media record with an
/// empty id whose `Match` probe hits an existing row commits THAT row's id
/// (`sql_base_repository.go:577-589`), so the id the arm must invalidate is one the batch
/// never contained — a pre-put arm keyed on the record's own (empty) id would delete nothing.
#[test]
fn artwork_arm_runs_after_media_put_against_committed_ids() {
    let (db, path) = fresh_store("artwork-order");
    seed_libraries(&db);

    // A row that the empty-id record's Match probe will find and reuse.
    seed_media(
        &db,
        &media_record("mf-exists", "al-1", "fo-1", "album-one/track-01.mp3"),
    );
    seed_item_artwork(&db, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-exists");

    let mut batch = base_batch();
    batch.media[0].id = String::new();

    let outcome = run_ok(&db, &batch);
    assert_eq!(
        outcome.media_ids,
        vec!["mf-exists".to_string()],
        "the Match probe reused the existing row's id"
    );
    assert!(
        outcome.inserted_media_ids.is_empty(),
        "the reuse took the UPDATE leg, so nothing was inserted"
    );
    assert_eq!(
        outcome.artwork_rows_deleted, 1,
        "invalidation keyed on the COMMITTED id, which the batch did not name"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        0,
        "the row under the reused id is gone: the arm ran after the Put"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM media_file"),
        1,
        "no duplicate row under a minted id"
    );

    // A folder with NO tracks runs no artwork statement at all (`:437`).
    let (db2, path2) = fresh_store("artwork-empty");
    seed_libraries(&db2);
    seed_item_artwork(&db2, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, "mf-keep");
    let mut no_media = base_batch();
    no_media.media = Vec::new();
    let outcome2 = run_ok(&db2, &no_media);
    assert_eq!(outcome2.artwork_rows_deleted, 0);
    assert_eq!(
        scalar::<i64>(
            &db2,
            "SELECT count(*) FROM item_artwork WHERE item_id = 'mf-keep'"
        ),
        1,
        "the arm is skipped entirely, so the row survives"
    );

    cleanup(db, path);
    cleanup(db2, path2);
}
