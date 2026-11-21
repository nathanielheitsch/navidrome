//! DB-16 — the two move/reference/copy sequences EXECUTED through the DB-00 typed
//! transaction seam, over a fresh schema.
//!
//! Go truth under test, SOURCE-VERIFIED at the pinned lines and then executed against
//! `nd_dbschema::schema::apply_fresh_schema` (one isolated temp file per invocation,
//! `_foreign_keys=on`, matching Go's own FK enforcement — the DSN parameter at
//! `db/db.go:55` and `PRAGMA foreign_keys=on` after migrations at `db/db.go:87`):
//!
//! * **Scanner move** (`scanner/phase_2_missing_tracks.go:291-335`) runs in
//!   `WithTxRetry` (`persistence.go:207-228`), i.e.
//!   [`nd_db::store::SQLStore::try_with_tx_retry_sync`]: Put of the moved record under the
//!   MISSING id (`:301`) → Delete of the discarded target id (`:306`) → album legs ONLY when
//!   the caller's claim granted them (`:280-289`), both WARN-only (`:314`, `:321`).
//!   **No `ReassignReferences`** — `scanner_move_album_change_runs_reassign_and_copy_only`
//!   is the negative control for exactly that.
//! * **Maintenance remap** (`core/maintenance.go:79-111`) runs in `WithTx` — NO retry —
//!   i.e. [`nd_db::store::SQLStore::try_with_tx_sync`], so a BUSY failure surfaces on the
//!   FIRST occurrence (`persistence.go:218`). Put (`:85`) → `ReassignReferences(discardedID,
//!   missingID)` (`:89`, `mediafile_repository.go:410-432`: annotation WITH average
//!   recalculation, bookmark, playlist_tracks, scrobbles, OR-IGNORE buffer, in that order) →
//!   Delete (`:92`) → album legs ONLY when the old album now holds 0 tracks (`:96-109`),
//!   all FATAL except `CopyAttributes`' `ErrNotFound` (`:105-107`).
//! * The retry-policy split is Go's own and is pinned by
//!   `scanner_move_busy_retry_reruns_from_original_target` versus
//!   `maintenance_remap_does_not_retry_on_busy`.
//! * `created_at` (`sql_base_repository.go:605,608` drop it from the UPDATE set; the Rust
//!   mirror is `marshal::UPDATE_DROP`) makes the move's `moved.CreatedAt =
//!   missing.CreatedAt` assignment INERT on the UPDATE arm — the surviving row keeps its own
//!   stored `created_at`, asserted by `scanner_move_reuses_missing_identity_and_keeps_target_created_at`.
//!
//! Every statement runs through the production primitives and the production renderers; no
//! handwritten UPDATE stands in for a write under test, except the two explicitly-labelled
//! non-vacuity controls and the pre-existing fixture rows (the `insert_mediafile_raw`
//! precedent, `runtime_hydrate_album_mediafile.rs:352`): a row that exists BEFORE the API
//! under test runs is what makes an identity or ordering shortcut observable.
//!
//! Non-vacuity, by named test: dropping the copy arm fails
//! `scanner_move_album_change_runs_reassign_and_copy_only` and
//! `copy_created_at_skips_null_empty_zero_and_preserves_valid_text`; dropping the reference
//! bundle fails `maintenance_remap_reassigns_full_references_in_go_order` (annotations,
//! bookmarks, playlist rows, scrobbles AND the recalculated average all stay on the discarded
//! id); running the bundle in the scanner move fails the same test's negative control in
//! `scanner_move_album_change_runs_reassign_and_copy_only`; reordering delete-before-put
//! fails `maintenance_remap_reassigns_full_references_in_go_order` (the cascaded scrobble
//! rows would match no UPDATE); adding retry to the maintenance remap fails
//! `maintenance_remap_does_not_retry_on_busy`; writing `created_at` on the update arm fails
//! `scanner_move_reuses_missing_identity_and_keeps_target_created_at`; swallowing a
//! transaction-losing warn arm fails `scanner_move_warn_arm_that_lost_the_transaction_is_fatal`.
//!
//! `delete_mediafile_in_tx`'s cascade claims are source-verified from
//! `nd-dbschema/src/schema_ddl.rs` before pinning: `media_file_artists` (:73),
//! `media_file_tags` (:74), `scrobble_buffer` (:77) and `scrobbles` (:78) declare
//! `ON DELETE CASCADE` onto `media_file`, the FTS `media_file_fts_ad` trigger (:83) removes
//! the index entry, while `annotation` (:59) and `bookmark` (:60) key on
//! `(user_id, item_id, item_type)` and reference only `user` — so they SURVIVE a track delete
//! as orphans until `cleanAnnotations`/`cleanBookmarks` run in `GC`
//! (`persistence/persistence.go:269,271`) — and `playlist_tracks` (:76) has a FK on
//! `playlist` only, so its `media_file_id` is not cascaded (Go's `removeOrphans` in `GC`,
//! `:273`).
//!
//! Not exercised here, by scope: `processedAlbumAnnotations` (SC state), candidate selection,
//! `ds.GC()` (`core/maintenance.go:117`), `Artist().RefreshStats` (`:124`), album count
//! refresh (DB-17), playlist cleanup (DB-20), HTTP. No production caller is wired.

use std::convert::Infallible;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    copy_album_created_at_in_tx, put_album_in_tx, AlbumRecord, AlbumWriteError,
    CopyAlbumCreatedAtOutcome,
};
use nd_db::artist_repo::{put_artist_in_tx, ArtistRecord, ArtistWriteError};
use nd_db::library_repo::{put_in_tx as put_library_in_tx, LibraryRecord, LibraryWriteError};
use nd_db::mediafile_repo::{
    delete_all_missing_in_tx, delete_mediafile_in_tx, move_matched_mediafile,
    reassign_references_in_tx, remap_missing_mediafile, AlbumAnnotationTransfer, MediaFileError,
    MediaFileRecord, MediaFileRepo, MissingRemap, MoveWriteStage, SelectedMove,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_annotations::AnnotationEntityKind;
use nd_db::sql_bookmarks::reassign_bookmark_sql;
use nd_db::sqlrepo::Value;
use nd_db::store::{is_busy, SQLStore, TX_RETRY_DELAY};
use nd_db::tag_repo::{self, Tag};
use nd_db::time;
use nd_db::{DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::{apply_fresh_schema, DbOpenOptions, JournalMode, Synchronous};
use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// The target row's own `created_at`: the value the surviving row must keep (pin 1).
fn target_stamp() -> DateTime<Utc> {
    at(2016, 1, 2, 3, 4, 5)
}

/// The MISSING row's own stored `created_at`, distinct from both other clocks. The move's
/// `missing_created_at` input is [`carried_stamp`], so a surviving `created_at` equal to
/// THIS value proves the assignment was inert rather than coincidentally equal.
fn missing_stamp() -> DateTime<Utc> {
    at(2014, 6, 7, 8, 9, 10)
}

/// `missing.CreatedAt` as the caller supplies it — the value an implementation that wrote
/// `created_at` on the UPDATE arm would land. Distinct from every stored stamp.
fn carried_stamp() -> DateTime<Utc> {
    at(2020, 3, 4, 5, 6, 7)
}

/// The caller clock for the INSERT arm only. Never stored on an UPDATE arm.
fn now_stamp() -> DateTime<Utc> {
    at(2022, 8, 9, 10, 11, 12)
}

/// `birth_time`, dropped from every UPDATE set (`sql_base_repository.go:608`).
fn birth_stamp() -> DateTime<Utc> {
    at(2015, 3, 4, 5, 6, 7)
}

/// The OLD album's `created_at` — the copy arm's source value.
fn old_album_stamp() -> DateTime<Utc> {
    at(1972, 7, 8, 9, 10, 11)
}

/// The NEW album's own `created_at`, which must survive unless the copy arm ran.
fn new_album_stamp() -> DateTime<Utc> {
    at(2005, 5, 5, 5, 5, 5)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-move-persistence-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    path
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
/// `SQLITE_BUSY` immediately instead of waiting (`runtime_folder_persistence.rs:110-131`
/// precedent). The sequences themselves use the production driver and its 5s/10s/15s backoff.
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

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Run a raw block on one pooled lease. Never called from inside a transaction.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement. `user`/`user_library`/`playlist` and the PRE-EXISTING
/// `media_file` rows have no production write API reachable from this lane (or ARE the
/// thing under test), so they are fixture INSERTs, as in
/// `runtime_mediafile_writes.rs:498,528` and `runtime_annotation_transfers.rs:170-177`.
fn fixture(store: &SQLStore, sql: &str, args: &[SqlValue]) {
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

/// One scalar probe with no binds.
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

fn count(store: &SQLStore, table: &str) -> i64 {
    scalar(store, &format!("SELECT count(*) FROM {table}"))
}

/// One stored cell rendered through SQLite's own `cast(… as text)` — byte-exact for a TEXT
/// datetime cell (no driver re-formatting), and comparable for REAL/INTEGER cells. `None`
/// when the cell is NULL.
fn cell_text(store: &SQLStore, table: &str, column: &str, id: &str) -> Option<String> {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT cast({column} as text) FROM {table} WHERE id = ?"),
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or_else(|e| panic!("{table}.{column} for {id}: {e}"))
    })
}

/// Row count with the predicate spelled by the caller.
fn count_where(store: &SQLStore, sql: &str) -> i64 {
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// One cell of a caller-spelled projection, also cast to TEXT.
fn cell_by(store: &SQLStore, sql: &str, args: &[SqlValue]) -> Option<String> {
    lease(store, |conn| {
        conn.query_row(sql, rusqlite::params_from_iter(args.iter()), |row| {
            row.get::<_, Option<String>>(0)
        })
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
    })
}

/// DB-05 production library write: the libraries the FK onto `media_file.library_id`
/// (`schema_ddl.rs:64`) needs.
fn seed_library(store: &SQLStore, id: i64, tag: &str) {
    let record = LibraryRecord {
        id,
        name: format!("lib-{tag}-{id}"),
        path: format!("/lib-{tag}-{id}"),
        ..LibraryRecord::default()
    };
    store
        .try_with_tx_sync::<nd_db::library_repo::LibraryWriteResult, LibraryWriteError>(|tx| {
            put_library_in_tx(tx, &record, &now_stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("seed_library({id}) failed: {e}"));
}

/// DB-08 production artist write, needed by `media_file_artists`' FK onto `artist`
/// (`schema_ddl.rs:73`).
fn seed_artist(store: &SQLStore, id: &str) {
    let record = ArtistRecord {
        id: id.to_string(),
        name: format!("Name-{id}"),
        order_artist_name: format!("Order-{id}"),
        ..ArtistRecord::default()
    };
    store
        .try_with_tx_sync::<nd_db::artist_repo::ArtistWriteResult, ArtistWriteError>(|tx| {
            put_artist_in_tx(tx, &record, &now_stamp(), &[])
        })
        .unwrap_or_else(|e| panic!("seed_artist({id}) failed: {e}"));
}

/// DB-06 production tag write with a real `nd_id::new_tag_id`, for `media_file_tags`' FK.
fn seed_tag(store: &SQLStore, lib: i64, name: &str, value: &str) -> String {
    let id = nd_id::new_tag_id(name, value);
    let tags: Vec<Tag> = vec![Tag {
        id: id.clone(),
        tag_name: name.to_string(),
        tag_value: value.to_string(),
        album_count: 0,
        song_count: 0,
    }];
    store
        .try_with_tx_sync::<(), Infallible>(|tx| tag_repo::add_in_tx(tx, lib, &tags))
        .unwrap_or_else(|e| panic!("seed_tag({name}={value}) failed: {e}"));
    id
}

/// `user` rows: `annotation.user_id` (`schema_ddl.rs:59`) and `bookmark.user_id` (:60)
/// reference them, so the annotation/bookmark assertions depend on real FK-enforced rows.
fn seed_user(store: &SQLStore, id: &str) {
    fixture(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(id),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            integer(0),
            text(&time::to_text(&target_stamp())),
            text(&time::to_text(&target_stamp())),
        ],
    );
}

/// A DB-09 album row with an explicit `created_at`, which is the copy arm's whole subject.
fn seed_album(store: &SQLStore, id: &str, created_at: Option<DateTime<Utc>>, stale_avg: f64) {
    let record = AlbumRecord {
        id: id.to_string(),
        library_id: 1,
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
        song_count: 2,
        duration: 120.5,
        size: 4096,
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
        folder_ids: None,
        explicit_status: format!("exp-{id}"),
        rg_album_gain: Some(-8.25),
        rg_album_peak: Some(0.75),
        description: format!("desc-{id}"),
        small_image_url: format!("small-{id}"),
        medium_image_url: format!("medium-{id}"),
        large_image_url: format!("large-{id}"),
        external_url: format!("ext-{id}"),
        external_info_updated_at: None,
        genre: "Rock".to_string(),
        tags: nd_db::sql_tags::DbTags::new(),
        participants: nd_db::sql_participations::Participants::new(),
        participant_names: Vec::new(),
        missing: false,
        imported_at: now_stamp(),
        created_at,
        updated_at: Some(now_stamp()),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
    };
    store
        .try_with_tx_sync::<nd_db::album_repo::AlbumWriteResult, AlbumWriteError>(|tx| {
            put_album_in_tx(tx, &record, &now_stamp())
        })
        .unwrap_or_else(|e| panic!("seed_album({id}) failed: {e}"));
    // A deliberately stale cached average, so "the average was recalculated" cannot pass
    // because the column happened to already hold the right value.
    fixture(
        store,
        "UPDATE album SET average_rating = ? WHERE id = ?",
        &[real(stale_avg), text(id)],
    );
}

/// A PRE-EXISTING `media_file` row, hand-INSERTed (`insert_mediafile_raw` precedent,
/// `runtime_hydrate_album_mediafile.rs:352`): the row exists BEFORE the move under test, so
/// an implementation that inserted rather than updated cannot pass by having written it.
fn media_row(
    store: &SQLStore,
    id: &str,
    lib: i64,
    album_id: &str,
    path: &str,
    created_at: DateTime<Utc>,
    missing: bool,
) {
    fixture(
        store,
        "INSERT INTO media_file (id, library_id, path, title, album, album_id, artist, artist_id, \
         album_artist, created_at, updated_at, birth_time, missing, folder_id, average_rating) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            integer(lib),
            text(path),
            text(&format!("Title-{id}")),
            text(&format!("Album-{id}")),
            text(album_id),
            text(&format!("Artist-{id}")),
            text("ar-main"),
            text(&format!("AlArtist-{id}")),
            text(&time::to_text(&created_at)),
            text(&time::to_text(&created_at)),
            text(&time::to_text(&birth_stamp())),
            integer(i64::from(missing)),
            text(&format!("fold-{id}")),
            real(9.0),
        ],
    );
}

/// The production read (`mediafile_repository.go:212-221` via `MediaFileRepo::get`), which is
/// the caller's input to a move plan — pin 6's write→read-back rule.
fn read_back(store: &SQLStore, id: &str) -> MediaFileRecord {
    lease(store, |conn| {
        MediaFileRepo::new(conn)
            .get(id, None, None, false)
            .unwrap_or_else(|e| panic!("MediaFileRepo::get({id}): {e}"))
    })
}

/// The production read as a `Result`, for the absence assertions.
fn read_attempt(store: &SQLStore, id: &str) -> Result<MediaFileRecord, MediaFileError> {
    lease(store, |conn| {
        MediaFileRepo::new(conn).get(id, None, None, false)
    })
}

/// One annotation row of a registered kind. `media_file` rows carry a stale cached average
/// through `media_row`'s `average_rating` bind; album rows through `seed_album`.
fn seed_annotation(
    store: &SQLStore,
    user_id: &str,
    entity: AnnotationEntityKind,
    item_id: &str,
    rating: i64,
) {
    fixture(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating, starred) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            text(user_id),
            text(item_id),
            text(entity.table()),
            integer(3),
            integer(rating),
            integer(1),
        ],
    );
}

/// One bookmark row, with a POSITION that identifies which user's row survived an
/// `OR IGNORE` conflict.
fn seed_bookmark(
    store: &SQLStore,
    user_id: &str,
    entity: AnnotationEntityKind,
    item_id: &str,
    position: i64,
) {
    fixture(
        store,
        "INSERT INTO bookmark (user_id, item_id, item_type, position, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            text(user_id),
            text(item_id),
            text(entity.table()),
            integer(position),
            text(&time::to_text(&target_stamp())),
            text(&time::to_text(&target_stamp())),
        ],
    );
}

/// A `playlist` row + one `playlist_tracks` entry pointing at `media_file_id`. The playlist
/// FK is onto `playlist` only (`schema_ddl.rs:76`), which is exactly why a media-file delete
/// does NOT cascade it.
fn seed_playlist_track(store: &SQLStore, playlist_id: &str, owner: &str, media_file_id: &str) {
    fixture(
        store,
        "INSERT INTO playlist (id, name, owner_id) VALUES (?, ?, ?)",
        &[
            text(playlist_id),
            text(&format!("PL-{playlist_id}")),
            text(owner),
        ],
    );
    fixture(
        store,
        "INSERT INTO playlist_tracks (id, playlist_id, media_file_id) VALUES (?, ?, ?)",
        &[integer(1), text(playlist_id), text(media_file_id)],
    );
}

/// One historical scrobble. `scrobbles.media_file_id` cascades onto `media_file`
/// (`schema_ddl.rs:78`), so its survival pattern is delete-order evidence.
fn seed_scrobble(store: &SQLStore, user_id: &str, media_file_id: &str, submitted: i64) {
    fixture(
        store,
        "INSERT INTO scrobbles (media_file_id, user_id, submission_time) VALUES (?, ?, ?)",
        &[text(media_file_id), text(user_id), integer(submitted)],
    );
}

/// One buffered scrobble. Its unique key is `(user_id, service, media_file_id, play_time)`
/// (`schema_ddl.rs:77`), which is what makes the bundle's `OR IGNORE` leg observable.
/// `scrobble_buffer.id` defaults to `''` and carries a UNIQUE index (`schema_ddl.rs:77` plus
/// `scrobble_buffer_id_ix` at :160), so each seeded row gets its own distinct id.
fn seed_buffer(store: &SQLStore, user_id: &str, media_file_id: &str, play_time: &str) {
    fixture(
        store,
        "INSERT INTO scrobble_buffer (user_id, service, media_file_id, play_time, enqueue_time, id) \
         VALUES (?, 'lastfm', ?, ?, ?, ?)",
        &[
            text(user_id),
            text(media_file_id),
            text(play_time),
            text(&time::to_text(&target_stamp())),
            text(&format!("buf-{user_id}-{media_file_id}")),
        ],
    );
}

/// A media-file row's full user-reference fingerprint: annotation, bookmark, playlist and
/// scrobble rows keyed by `item_id`/`media_file_id`, as a stable vector.
fn reference_fingerprint(store: &SQLStore, item_id: &str) -> Vec<&'static str> {
    let exists = |sql: &str| -> bool {
        lease(store, |conn| {
            conn.query_row(sql, params![item_id], |row| row.get::<_, i64>(0))
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                > 0
        })
    };
    let mut rows = Vec::new();
    if exists("SELECT count(*) FROM annotation WHERE item_id = ? AND item_type = 'media_file'") {
        rows.push("annotation");
    }
    if exists("SELECT count(*) FROM bookmark WHERE item_id = ? AND item_type = 'media_file'") {
        rows.push("bookmark");
    }
    if exists("SELECT count(*) FROM playlist_tracks WHERE media_file_id = ?") {
        rows.push("playlist_tracks");
    }
    if exists("SELECT count(*) FROM scrobbles WHERE media_file_id = ?") {
        rows.push("scrobbles");
    }
    if exists("SELECT count(*) FROM scrobble_buffer WHERE media_file_id = ?") {
        rows.push("scrobble_buffer");
    }
    rows
}

/// A `RAISE(...)` trigger for the warn/fatal arm fixtures. `ABORT` undoes only the failing
/// statement (`D-BATCH`); `ROLLBACK` ends the transaction.
fn raise_trigger(store: &SQLStore, ddl: &str) {
    fixture(store, ddl, &[]);
}

/// The typed Body-stage SQLite failure a sequence must report, with its extended code.
fn assert_body_sqlite_failure<E: std::error::Error + 'static>(
    err: &DbRuntimeError<E>,
    want_extended: Option<i32>,
) -> &SqliteError {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body, "{err}");
            if let Some(want) = want_extended {
                let extended = match source {
                    SqliteError::SqliteFailure(code, _) => code.extended_code,
                    other => panic!("expected a SqliteFailure, got {other:?}"),
                };
                assert_eq!(extended, want, "{err}");
            }
            assert!(
                !is_busy(source),
                "a fixture failure is not busy-class: {source:?}"
            );
            source
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// Row counts over the tables the sequences touch, so a rollback test can prove NOTHING
/// partial survived rather than asserting one column.
fn state(store: &SQLStore) -> Vec<(&'static str, i64)> {
    [
        "media_file",
        "media_file_artists",
        "media_file_tags",
        "annotation",
        "bookmark",
        "playlist",
        "playlist_tracks",
        "scrobbles",
        "scrobble_buffer",
        "album",
        "album_tags",
        "artist",
        "user",
    ]
    .iter()
    .map(|t| (*t, count(store, t)))
    .collect()
}

/// The same fingerprint with owned table names, which is the form a snapshot taken BEFORE an
/// operation is compared against.
fn state_snapshot(store: &SQLStore) -> Vec<(String, i64)> {
    state(store)
        .iter()
        .map(|(t, c)| (t.to_string(), *c))
        .collect()
}

fn assert_state_unchanged(store: &SQLStore, before: &[(String, i64)], note: &str) {
    assert_eq!(&state_snapshot(store), before, "{note}");
}

// ---------------------------------------------------------------------------
// Scanner move (scanner/phase_2_missing_tracks.go:291-335)
// ---------------------------------------------------------------------------

/// Pin 1 + identity: the move Puts the target's content under the MISSING id (Go's
/// `moved.ID = missing.ID`, `:300`) — which takes the UPDATE arm, because the missing row
/// exists — then deletes the discarded target (`:306`). The surviving row keeps its OWN
/// `created_at` and `birth_time` (`sql_base_repository.go:605,608`), NOT the plan's
/// `missing_created_at`, which is exactly why that assignment is inert here and must not be
/// "fixed".
#[test]
fn scanner_move_reuses_missing_identity_and_keeps_target_created_at() {
    let (db, path) = fresh_store("move-identity");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    media_row(
        &db,
        "mf-target",
        1,
        "al-1",
        "/music/real-flac.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-1",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );

    let plan = SelectedMove {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        // Go's `missing.CreatedAt` (`:296`). It reaches the record but the UPDATE arm drops
        // the column, so nothing may land it.
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        album_transfer: None,
    };

    let outcome = move_matched_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("move_matched_mediafile failed: {e}"));

    assert_eq!(outcome.id, "mf-missing", "Go's moved.ID = missing.ID");
    assert!(
        !outcome.inserted,
        "the missing row exists, so the UPDATE arm ran, not an INSERT"
    );
    assert_eq!(outcome.discarded_id, "mf-target");
    assert_eq!(outcome.discarded_rows_deleted, 1);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert!(
        outcome.copy_outcome.is_none(),
        "no album change was granted"
    );
    assert!(outcome.album_transfer.is_none());

    // The surviving row is the FORMER missing row: target content, missing identity.
    let moved = read_back(&db, "mf-missing");
    assert_eq!(moved.path, "/music/real-flac.flac");
    assert!(
        !moved.missing,
        "the target's missing flag came across: the row is no longer missing"
    );
    assert_eq!(moved.album_id, "al-1");
    assert_eq!(
        cell_text(&db, "media_file", "created_at", "mf-missing"),
        Some(time::to_text(&missing_stamp())),
        "created_at is dropped from the UPDATE set (sql_base_repository.go:605): the row keeps \
         its OWN value, NOT the plan's carried missing.CreatedAt"
    );
    assert_ne!(
        cell_text(&db, "media_file", "created_at", "mf-missing"),
        Some(time::to_text(&carried_stamp())),
        "pin 1: writing missing.CreatedAt on the update arm would land the carried stamp"
    );
    assert_eq!(
        cell_text(&db, "media_file", "birth_time", "mf-missing"),
        Some(time::to_text(&birth_stamp())),
        "birth_time is dropped too (`:608`)"
    );

    // The discarded row is gone, and its id is unreachable.
    assert_eq!(count(&db, "media_file"), 1);
    assert!(matches!(
        read_attempt(&db, "mf-target"),
        Err(MediaFileError::NotFound)
    ));

    // The plan is the caller's immutable snapshot: nothing mutated it (a retry must re-derive
    // the ORIGINAL target, `:293-294`).
    assert_eq!(plan.target.id, "mf-target");
    assert_eq!(plan.target.path, "/music/real-flac.flac");
    assert_eq!(plan.missing_created_at, Some(carried_stamp()));

    cleanup(db, path);
}

/// The album-change arm pair (`:313-325`) runs `Album().ReassignAnnotation` and
/// `CopyAttributes("created_at")` and NOTHING ELSE: the negative control for `pin 3`. The
/// media-file reference bundle must NOT run, so the target's own annotations and bookmarks
/// stay on the discarded id (and survive as orphans), and no scrobble row ends up under the
/// missing id.
#[test]
fn scanner_move_album_change_runs_reassign_and_copy_only() {
    let (db, path) = fresh_store("move-album-change");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
    seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
    media_row(
        &db,
        "mf-target",
        1,
        "al-new",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-old",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    seed_annotation(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        5,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 4);
    seed_bookmark(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        111,
    );
    seed_playlist_track(&db, "pl-1", "u-one", "mf-target");
    seed_scrobble(&db, "u-one", "mf-target", 1_700_000_000);
    seed_buffer(&db, "u-one", "mf-target", "2024-01-01 00:00:00+00:00");

    let plan = SelectedMove {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        album_transfer: Some(AlbumAnnotationTransfer {
            from_id: "al-old".to_string(),
            to_id: "al-new".to_string(),
        }),
    };

    let outcome = move_matched_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("move_matched_mediafile failed: {e}"));
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(
        outcome.copy_outcome,
        Some(CopyAlbumCreatedAtOutcome::Copied),
        "the copy arm ran on a valid source"
    );
    assert_eq!(
        outcome.album_transfer,
        Some(AlbumAnnotationTransfer {
            from_id: "al-old".to_string(),
            to_id: "al-new".to_string()
        })
    );

    // Album legs DID run: annotation moved, target average recalculated from {4} → 4.0
    // (stale 7.7 proves the recalculation, not a coincidence), created_at copied.
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM annotation WHERE item_type = 'album'",
            &[]
        ),
        Some("al-new".to_string())
    );
    assert_eq!(
        cell_text(&db, "album", "average_rating", "al-new"),
        Some("4.0".to_string()),
        "ReassignAnnotation's updateAvgRating leg ran on the TARGET (sql_annotations.go:195-196)"
    );
    assert_eq!(
        cell_text(&db, "album", "created_at", "al-new"),
        Some(time::to_text(&old_album_stamp())),
        "CopyAttributes('created_at') copied the old album's stored text byte-for-byte"
    );

    // The media-file bundle did NOT run (Go never calls ReassignReferences here). The
    // target's annotation, bookmark and playlist row all stay on the DISCARDED id and
    // survive its delete — annotation/bookmark reference `user` only (schema_ddl.rs:59,60)
    // and playlist_tracks references `playlist` only (:76).
    assert_eq!(
        reference_fingerprint(&db, "mf-target"),
        vec!["annotation", "bookmark", "playlist_tracks"],
        "annotation/bookmark (:59,:60) and playlist_tracks (:76) all survive the delete; scrobbles
         and the buffer cascade. Running the bundle would have moved ALL of them to mf-missing
         and left nothing here"
    );
    assert_eq!(
        reference_fingerprint(&db, "mf-missing"),
        Vec::<&'static str>::new(),
        "negative control: a blanket ReassignReferences in the scanner move would land \
         annotation, bookmark and scrobble rows on the missing id"
    );
    assert_eq!(
        count(&db, "playlist_tracks"),
        1,
        "playlist_tracks is not cascaded (schema_ddl.rs:76) and still points at the deleted id"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM playlist_tracks WHERE id = 1",
            &[]
        ),
        Some("mf-target".to_string())
    );
    assert_eq!(
        count(&db, "scrobbles"),
        0,
        "scrobbles cascade with the deleted row (:78)"
    );
    assert_eq!(
        count(&db, "scrobble_buffer"),
        0,
        "scrobble_buffer cascades with the deleted row (:77)"
    );

    cleanup(db, path);
}

/// Warn-vs-fatal (`D-BATCH` applied to Go's own policy, `:314`/`:321`): a warn-only arm that
/// fails WITHOUT ending the transaction (`RAISE(ABORT)`) is captured as a warning and the
/// transaction still commits the media move. Each arm is injected independently, and the arm
/// that runs AFTER it is asserted to have run.
///
/// The copy case also pins the outcome contract: a copy arm that FAILED publishes
/// `copy_outcome == None` plus the `CopyAlbumCreatedAt` warning, never a
/// `SkippedPoisonedSource` classification. The source album there holds a VALID
/// `created_at` — asserted directly — so `SkippedPoisonedSource` would be provably false:
/// only the destination UPDATE aborted. `None` plus the warning is the honest pair.
#[test]
fn scanner_move_warn_arms_degrade_and_still_commit() {
    struct Case {
        label: &'static str,
        ddl: &'static str,
        stage: MoveWriteStage,
        /// The copy arm's published classification: `Some` when it ran and was
        /// classified, `None` when it was never attempted OR it failed (told apart by
        /// whether a `CopyAlbumCreatedAt` warning is present, asserted below).
        copy_expected: Option<CopyAlbumCreatedAtOutcome>,
        album_created_at: Option<DateTime<Utc>>,
    }
    let cases = [
        Case {
            label: "warn-annotation",
            ddl: "CREATE TRIGGER fixture_warn BEFORE UPDATE ON annotation FOR EACH ROW \
                  WHEN OLD.item_type = 'album' AND OLD.item_id = 'al-old' \
                  BEGIN SELECT RAISE(ABORT, 'fixture: album annotation aborted'); END",
            stage: MoveWriteStage::ReassignAlbumAnnotation,
            // Go continues into the copy arm after the warn (`:319`).
            copy_expected: Some(CopyAlbumCreatedAtOutcome::Copied),
            album_created_at: Some(old_album_stamp()),
        },
        Case {
            label: "warn-copy",
            ddl: "CREATE TRIGGER fixture_warn BEFORE UPDATE ON album FOR EACH ROW \
                  WHEN NEW.id = 'al-new' AND NEW.created_at IS NOT OLD.created_at \
                  BEGIN SELECT RAISE(ABORT, 'fixture: album copy aborted'); END",
            stage: MoveWriteStage::CopyAlbumCreatedAt,
            // The arm ran and FAILED, so it publishes no classification at all.
            copy_expected: None,
            album_created_at: Some(new_album_stamp()),
        },
    ];

    for case in cases {
        let (db, path) = fresh_store(case.label);
        seed_library(&db, 1, "main");
        seed_user(&db, "u-one");
        seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
        seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
        media_row(
            &db,
            "mf-target",
            1,
            "al-new",
            "/music/real.flac",
            target_stamp(),
            false,
        );
        media_row(
            &db,
            "mf-missing",
            1,
            "al-old",
            "/music/gone.mp3",
            missing_stamp(),
            true,
        );
        seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 4);
        raise_trigger(&db, case.ddl);

        let plan = SelectedMove {
            target: read_back(&db, "mf-target"),
            missing_id: "mf-missing".to_string(),
            missing_created_at: Some(carried_stamp()),
            now: now_stamp(),
            append_subtitle: false,
            album_transfer: Some(AlbumAnnotationTransfer {
                from_id: "al-old".to_string(),
                to_id: "al-new".to_string(),
            }),
        };

        let outcome = move_matched_mediafile(&db, &plan)
            .unwrap_or_else(|e| panic!("{}: warn arms must not lose the move: {e}", case.label));

        assert_eq!(
            outcome.warnings.len(),
            1,
            "{}: exactly one degraded arm: {:?}",
            case.label,
            outcome.warnings
        );
        let warning = &outcome.warnings[0];
        assert_eq!(warning.stage, case.stage, "{}", case.label);
        assert_eq!(warning.item_id, "al-old");
        assert_eq!(warning.related_id, "al-new");
        assert!(
            !is_busy(&warning.source),
            "{}: a fixture failure is not busy-class: {:?}",
            case.label,
            warning.source
        );
        assert_eq!(warning.stage.as_str(), case.stage.as_str());
        assert_eq!(outcome.copy_outcome, case.copy_expected, "{}", case.label);
        if case.stage == MoveWriteStage::CopyAlbumCreatedAt {
            assert_eq!(
                cell_text(&db, "album", "created_at", "al-old"),
                Some(time::to_text(&old_album_stamp())),
                "{}: the SOURCE holds a valid created_at, so a `SkippedPoisonedSource` \
                 classification would be false — the failure is the aborted destination UPDATE",
                case.label
            );
            assert!(
                outcome.album_transfer.is_some(),
                "{}: the arm was ATTEMPTED (the transfer was granted), so `None` means \
                 FAILED, not unattempted",
                case.label
            );
        }
        assert_eq!(
            cell_text(&db, "album", "created_at", "al-new"),
            case.album_created_at.map(|v| time::to_text(&v)),
            "{}",
            case.label
        );

        // The fatal arms committed: the move is durable despite the warning.
        assert_eq!(count(&db, "media_file"), 1, "{}", case.label);
        assert_eq!(outcome.id, "mf-missing");
        assert!(read_attempt(&db, "mf-target").is_err(), "{}", case.label);
        assert!(
            read_attempt(&db, "mf-missing").is_ok(),
            "{}: the committed move survived the warning",
            case.label
        );

        cleanup(db, path);
    }
}

/// A warn-only arm whose failure ENDED the transaction (`RAISE(ROLLBACK)`) is fatal: the
/// remaining arms would run in AUTOCOMMIT and stay durable even though the executor's `COMMIT`
/// fails, so the port returns the original failure instead of degrading it
/// (`warn_move_arm`'s `is_autocommit` check, same reasoning as
/// `scanner_writes::warn_arm`). No outcome is produced at all, and nothing leaked.
#[test]
fn scanner_move_warn_arm_that_lost_the_transaction_is_fatal() {
    let (db, path) = fresh_store("warn-rollback");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
    seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
    media_row(
        &db,
        "mf-target",
        1,
        "al-new",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-old",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 4);
    // A track OUTSIDE the move, so an autocommitted later arm would be visible.
    media_row(
        &db,
        "mf-unrelated",
        1,
        "al-new",
        "/music/other.flac",
        target_stamp(),
        false,
    );
    raise_trigger(
        &db,
        "CREATE TRIGGER fixture_lost BEFORE UPDATE ON annotation FOR EACH ROW \
         WHEN OLD.item_type = 'album' AND OLD.item_id = 'al-old' \
         BEGIN SELECT RAISE(ROLLBACK, 'fixture: transaction lost in album reassign'); END",
    );

    let before = state_snapshot(&db);
    let album_created_before = cell_text(&db, "album", "created_at", "al-new");

    let plan = SelectedMove {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        album_transfer: Some(AlbumAnnotationTransfer {
            from_id: "al-old".to_string(),
            to_id: "al-new".to_string(),
        }),
    };

    let err = move_matched_mediafile(&db, &plan)
        .expect_err("a warn arm that lost the transaction must be fatal, not a warning");
    assert_body_sqlite_failure(&err, Some(rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER));

    // Both fatal arms ran BEFORE this point and were rolled back with it; nothing after it
    // autocommitted.
    assert_state_unchanged(
        &db,
        &before,
        "the move, the discard delete and the copy arm all rolled back",
    );
    assert_eq!(
        cell_text(&db, "album", "created_at", "al-new"),
        album_created_before,
        "the copy arm leaked no created_at"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM annotation WHERE item_type = 'album'",
            &[]
        ),
        Some("al-old".to_string()),
        "no annotation moved in autocommit mode"
    );
    // The pool stays usable after the lost transaction.
    let probe: Result<u8, DbRuntimeError<Infallible>> =
        db.try_with_tx_sync::<u8, Infallible>(|tx| {
            Ok(tx
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap() as u8)
        });
    assert_eq!(probe.unwrap(), 1, "the pool is un-poisoned");

    cleanup(db, path);
}

/// Pin 2, scanner side: `WithTxRetry` (`:291`) really reruns on a REAL `SQLITE_BUSY` and the
/// rerun starts from the ORIGINAL target — Go's own comment at `:293-294`. The lock is a real
/// second connection holding an uncommitted write, released after ~1.5s, so the first attempt
//  fails at its first write and the production backoff spaces the rerun by `TX_RETRY_DELAY`.
#[test]
fn scanner_move_busy_retry_reruns_from_original_target() {
    let (db, path) = fresh_busy_store("move-busy-retry");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    media_row(
        &db,
        "mf-target",
        1,
        "al-1",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-1",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );

    let plan = SelectedMove {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        album_transfer: None,
    };

    let locker_path = dsn(&path);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let locker = std::thread::spawn(move || {
        let mut conn = nd_dbschema::schema::open(&locker_path).unwrap();
        conn.busy_timeout(Duration::ZERO)
            .expect("no busy wait on the locker");
        let tx = conn.transaction().expect("locker transaction");
        tx.execute(
            "INSERT INTO property(id, value) VALUES(?, ?)",
            params!["Db16BusyLock", "held"],
        )
        .expect("the locker must write inside its transaction");
        ready_tx
            .send(())
            .expect("the locker must signal that it holds the write lock");
        std::thread::sleep(Duration::from_millis(1_500));
        drop(tx);
        drop(conn);
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the locker holds the write lock before the move starts");

    let started = std::time::Instant::now();
    let outcome = move_matched_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("the retry must commit the move: {e}"));
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
    assert_eq!(
        count(&db, "property"),
        0,
        "the locker rolled back rather than committed"
    );

    // Attempt 2 re-derived the ORIGINAL target from the same immutable plan.
    assert_eq!(plan.target.id, "mf-target");
    assert_eq!(plan.target.path, "/music/real.flac");
    assert_eq!(outcome.id, "mf-missing");
    assert!(!outcome.inserted);
    assert_eq!(outcome.discarded_rows_deleted, 1);
    assert_eq!(
        count(&db, "media_file"),
        1,
        "one retry, one row, no double move"
    );
    assert_eq!(
        cell_text(&db, "media_file", "path", "mf-missing"),
        Some("/music/real.flac".to_string())
    );
    assert_eq!(
        cell_text(&db, "media_file", "created_at", "mf-missing"),
        Some(time::to_text(&missing_stamp())),
        "pin 1 holds across a retried attempt too"
    );

    cleanup(db, path);
}

/// Pin 2, maintenance side: `core/maintenance.go:79` uses `ds.WithTx`, NOT `WithTxRetry`, so
//  the remap goes through `try_with_tx_sync` and a BUSY failure surfaces on the FIRST
/// occurrence (`persistence.go:218`) — never as an exhausted retry, and with no waiting.
#[test]
fn maintenance_remap_does_not_retry_on_busy() {
    let (db, path) = fresh_busy_store("remap-no-retry");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    media_row(
        &db,
        "mf-target",
        1,
        "al-1",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-1",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );

    let plan = MissingRemap {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        old_album_id: "al-1".to_string(),
    };

    let locker_path = dsn(&path);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let locker = std::thread::spawn(move || {
        let mut conn = nd_dbschema::schema::open(&locker_path).unwrap();
        conn.busy_timeout(Duration::ZERO)
            .expect("no busy wait on the locker");
        let tx = conn.transaction().expect("locker transaction");
        tx.execute(
            "INSERT INTO property(id, value) VALUES(?, ?)",
            params!["Db16RemapBusyLock", "held"],
        )
        .expect("the locker must write inside its transaction");
        ready_tx.send(()).expect("lock held signal");
        // Held for the whole remap: a retrying implementation would rerun and eventually
        // succeed, which is exactly the behavior this test forbids.
        std::thread::sleep(Duration::from_millis(2_500));
        drop(tx);
        drop(conn);
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the locker holds the write lock");

    let started = std::time::Instant::now();
    let err = remap_missing_mediafile(&db, &plan)
        .expect_err("the remap must fail on the busy occurrence, not retry");
    let elapsed = started.elapsed();
    locker.join().expect("the locker thread must finish");

    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body, "{err}");
            assert!(
                is_busy(source),
                "the failure is the real busy code: {source:?}"
            );
        }
        DbRuntimeError::BusyExhausted { attempts, .. } => panic!(
            "pin 2: WithTx does not retry, so BusyExhausted (attempts={attempts}) is \
             unreachable on this path"
        ),
        other => panic!("expected a busy Sqlite failure, got {other:?}"),
    }
    assert!(
        elapsed < TX_RETRY_DELAY,
        "no backoff was waited: {elapsed:?}"
    );

    // Nothing partial survived, and the locker's own row rolled back.
    assert_eq!(count(&db, "media_file"), 2, "both rows still present");
    assert_eq!(count(&db, "property"), 0);
    assert!(read_attempt(&db, "mf-target").is_ok());

    cleanup(db, path);
}

/// Go's pre-checks and the album-change contrast: the album legs run ONLY when the old album
/// holds 0 tracks AFTER the Put+delete (`core/maintenance.go:96-109`. The 1-track contrast
/// leaves the album annotation, the cached average and the target album's `created_at`
/// untouched, and reports `old_album_empty == false`.
#[test]
fn maintenance_remap_album_reassign_only_when_old_album_empty() {
    // Case 1: the old album's only track is the missing row → 0 tracks after the remap.
    let (db, path) = fresh_store("remap-album-empty");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
    seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
    media_row(
        &db,
        "mf-target",
        1,
        "al-new",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-old",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 6);

    let plan = MissingRemap {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        old_album_id: "al-old".to_string(),
    };
    let outcome = remap_missing_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("remap_missing_mediafile failed: {e}"));
    assert!(outcome.old_album_empty);
    assert_eq!(
        outcome.album_transfer,
        Some(AlbumAnnotationTransfer {
            from_id: "al-old".to_string(),
            to_id: "al-new".to_string()
        })
    );
    assert_eq!(
        outcome.copy_outcome,
        Some(CopyAlbumCreatedAtOutcome::Copied)
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM annotation WHERE item_type = 'album'",
            &[]
        ),
        Some("al-new".to_string())
    );
    assert_eq!(
        cell_text(&db, "album", "average_rating", "al-new"),
        Some("6.0".to_string()),
        "the album average leg ran inside the album legs"
    );
    assert_eq!(
        cell_text(&db, "album", "created_at", "al-new"),
        Some(time::to_text(&old_album_stamp()))
    );
    cleanup(db, path);

    // Case 2: another track stays on the old album → 1 track → the legs are skipped.
    let (db, path) = fresh_store("remap-album-nonempty");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
    seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
    media_row(
        &db,
        "mf-target",
        1,
        "al-new",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-old",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    media_row(
        &db,
        "mf-other",
        1,
        "al-old",
        "/music/keeper.flac",
        target_stamp(),
        false,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 6);

    let outcome = remap_missing_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("remap_missing_mediafile failed: {e}"));
    assert!(
        !outcome.old_album_empty,
        "the old album still holds mf-other"
    );
    assert!(
        outcome.album_transfer.is_none(),
        "no album legs were authorized"
    );
    assert!(outcome.copy_outcome.is_none());
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM annotation WHERE item_type = 'album'",
            &[]
        ),
        Some("al-old".to_string()),
        "the album annotation stayed on the old album"
    );
    assert_eq!(
        cell_text(&db, "album", "average_rating", "al-new"),
        Some("7.7".to_string()),
        "no average recalculation ran"
    );
    assert_eq!(
        cell_text(&db, "album", "created_at", "al-new"),
        Some(time::to_text(&new_album_stamp())),
        "the copy arm did not run"
    );
    assert_eq!(
        count(&db, "media_file"),
        2,
        "mf-target discarded, mf-other kept"
    );
    cleanup(db, path);
}

/// The copy arm's poison rule (Go's `album_repository.go:343-350`, executed through the
/// SAME DB-14 helper `copy_album_created_at_in_tx`, pin 3 — not a second implementation):
/// NULL, empty and `0001-`-prefixed sources copy NOTHING and the target keeps its own
/// `created_at`; valid text (including a datetime cell) copies byte-for-byte. A missing
/// source row is `SourceNotFound`, which maintenance ignores (`:105-107`) rather than failing.
#[test]
fn copy_created_at_skips_null_empty_zero_and_preserves_valid_text() {
    struct Case {
        label: &'static str,
        /// `None` means no source album row at all.
        source: Option<&'static str>,
        source_created_at: Option<&'static str>,
        want: CopyAlbumCreatedAtOutcome,
        /// The new album's `created_at` after the remap.
        after: Option<&'static str>,
    }
    let cases = [
        Case {
            label: "valid",
            source: Some("al-src-1"),
            source_created_at: Some("1972-07-08 09:10:11+00:00"),
            want: CopyAlbumCreatedAtOutcome::Copied,
            after: Some("1972-07-08 09:10:11+00:00"),
        },
        Case {
            label: "null",
            source: Some("al-src-2"),
            source_created_at: None,
            want: CopyAlbumCreatedAtOutcome::SkippedPoisonedSource,
            after: None,
        },
        Case {
            label: "empty",
            source: Some("al-src-3"),
            source_created_at: Some(""),
            want: CopyAlbumCreatedAtOutcome::SkippedPoisonedSource,
            after: None,
        },
        Case {
            label: "zero",
            source: Some("al-src-4"),
            source_created_at: Some("0001-01-01 00:00:00+00:00"),
            want: CopyAlbumCreatedAtOutcome::SkippedPoisonedSource,
            after: None,
        },
        Case {
            label: "absent",
            source: None,
            source_created_at: None,
            want: CopyAlbumCreatedAtOutcome::SourceNotFound,
            after: None,
        },
    ];

    for case in cases {
        let (db, path) = fresh_store(&format!("copy-{label}", label = case.label));
        seed_library(&db, 1, "main");
        seed_user(&db, "u-one");
        if let Some(src) = case.source {
            seed_album(&db, src, Some(new_album_stamp()), 0.0);
            fixture(
                &db,
                "UPDATE album SET created_at = ? WHERE id = ?",
                &[
                    case.source_created_at.map(text).unwrap_or(SqlValue::Null),
                    text(src),
                ],
            );
        }
        seed_album(&db, "al-new", Some(new_album_stamp()), 0.0);
        media_row(
            &db,
            "mf-target",
            1,
            "al-new",
            "/music/real.flac",
            target_stamp(),
            false,
        );
        media_row(
            &db,
            "mf-missing",
            1,
            case.source.unwrap_or("al-absent"),
            "/music/gone.mp3",
            missing_stamp(),
            true,
        );

        let plan = MissingRemap {
            target: read_back(&db, "mf-target"),
            missing_id: "mf-missing".to_string(),
            missing_created_at: Some(carried_stamp()),
            now: now_stamp(),
            append_subtitle: false,
            old_album_id: case.source.unwrap_or("al-absent").to_string(),
        };

        let outcome = remap_missing_mediafile(&db, &plan)
            .unwrap_or_else(|e| panic!("{}: remap failed: {e}", case.label));
        assert_eq!(outcome.copy_outcome, Some(case.want), "{}", case.label);
        assert_eq!(
            cell_text(&db, "album", "created_at", "al-new"),
            Some(
                case.after
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| time::to_text(&new_album_stamp()))
            ),
            "{}: target created_at",
            case.label
        );
        cleanup(db, path);
    }
}

/// The maintenance remap's defining half: the FULL reference bundle executes in Go's order
/// (`mediafile_repository.go:411-430`) and the annotation leg runs the DB-15 method, so the
/// TARGET's cached average reflects the moved annotations — the observable that the old
/// render-only bundle could never produce. The delete runs AFTER the bundle, which is what
/// keeps the cascaded scrobble/buffer rows reachable.
#[test]
fn maintenance_remap_reassigns_full_references_in_go_order() {
    let (db, path) = fresh_store("remap-references");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_user(&db, "u-two");
    seed_album(&db, "al-1", Some(old_album_stamp()), 0.0);
    media_row(
        &db,
        "mf-target",
        1,
        "al-1",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-1",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    // Two users, one per id, so the recalculated average is 3.5 and not a coincidence of a
    // single rating or the stale 9.0 both rows start with.
    seed_annotation(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        5,
    );
    seed_annotation(
        &db,
        "u-two",
        AnnotationEntityKind::MediaFile,
        "mf-missing",
        2,
    );
    seed_bookmark(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        42,
    );
    seed_playlist_track(&db, "pl-1", "u-one", "mf-target");
    seed_scrobble(&db, "u-one", "mf-target", 1_700_000_000);
    seed_buffer(&db, "u-two", "mf-target", "2024-01-01 00:00:00+00:00");

    let plan = MissingRemap {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        // Same album on both sides, so the album legs (and CountAll) do not run.
        old_album_id: "al-1".to_string(),
    };

    let outcome = remap_missing_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("remap_missing_mediafile failed: {e}"));
    assert_eq!(outcome.id, "mf-missing");
    assert!(!outcome.inserted);
    assert_eq!(outcome.discarded_id, "mf-target");
    assert_eq!(outcome.discarded_rows_deleted, 1);
    assert!(
        !outcome.old_album_empty,
        "oldAlbumID == newAlbumID skips CountAll"
    );
    assert!(outcome.album_transfer.is_none());
    assert!(outcome.copy_outcome.is_none());

    // annotation: both rows now on mf-missing, and the average recalculated from {5, 2}.
    let moved = read_back(&db, "mf-missing");
    assert_eq!(moved.path, "/music/real.flac");
    assert_eq!(moved.annotation.average_rating, 3.5);
    assert_eq!(
        count(&db, "annotation"),
        2,
        "both users' rows moved to the missing id, none lost"
    );
    assert_eq!(
        count_where(
            &db,
            "SELECT count(*) FROM annotation WHERE item_id = 'mf-missing' AND item_type = 'media_file'"
        ),
        2
    );
    assert_eq!(
        count_where(
            &db,
            "SELECT count(*) FROM media_file WHERE id = 'mf-target'"
        ),
        0,
        "the discarded row is gone (its stale cached average went with it)"
    );

    // bookmark, playlist_tracks, scrobbles and the buffer all moved to the missing id.
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(position as text) FROM bookmark WHERE user_id = 'u-one' AND item_type = 'media_file'",
            &[]
        ),
        Some("42".to_string()),
        "the bookmark moved (mediafile_repository.go:414) with its position intact"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM playlist_tracks WHERE id = 1",
            &[]
        ),
        Some("mf-missing".to_string()),
        "playlist_tracks moved (:417)"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM scrobbles WHERE user_id = 'u-one'",
            &[]
        ),
        Some("mf-missing".to_string()),
        "scrobbles moved (:420) — only possible because the bundle ran BEFORE the delete, \
         which cascades them"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM scrobble_buffer WHERE user_id = 'u-two'",
            &[]
        ),
        Some("mf-missing".to_string()),
        "the OR-IGNORE buffer leg moved the pending submission (:425)"
    );
    assert_eq!(count(&db, "media_file"), 1);

    cleanup(db, path);
}

/// Go's `OR IGNORE` asymmetry inside the bundle (`sql_annotations.go:189-191`,
/// `sql_bookmarks.go:150`, and the buffer's unique key `schema_ddl.rs:77`): a user who
/// annotated/bookmarked/buffered BOTH ids keeps their OWN row on the target, the conflicting
/// source row is SKIPPED (never merged, never deleted), and no `OR REPLACE` shortcut
/// overwrites the target's value.
#[test]
fn reference_reassign_duplicate_target_annotations_bookmarks_buffer() {
    let (db, path) = fresh_store("reassign-conflicts");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    media_row(
        &db,
        "mf-prev",
        1,
        "al-1",
        "/music/prev.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-next",
        1,
        "al-1",
        "/music/next.flac",
        missing_stamp(),
        false,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::MediaFile, "mf-prev", 8);
    seed_annotation(&db, "u-one", AnnotationEntityKind::MediaFile, "mf-next", 2);
    seed_bookmark(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-prev",
        100,
    );
    seed_bookmark(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-next",
        200,
    );
    let play_time = "2024-02-02 02:02:02+00:00";
    seed_buffer(&db, "u-one", "mf-prev", play_time);
    seed_buffer(&db, "u-one", "mf-next", play_time);

    store_tx(&db, |tx| {
        reassign_references_in_tx(tx, AnnotationEntityKind::MediaFile, "mf-prev", "mf-next")
    });

    // Target keeps its OWN rating; the conflicting source row STAYS on prev_id.
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(rating as text) FROM annotation WHERE user_id = 'u-one' AND item_type = 'media_file' \
             AND item_id = 'mf-next'",
            &[]
        ),
        Some("2".to_string()),
        "OR IGNORE kept the target's own row (an OR REPLACE shortcut would land 8)"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(rating as text) FROM annotation WHERE user_id = 'u-one' AND item_type = 'media_file' \
             AND item_id = 'mf-prev'",
            &[]
        ),
        Some("8".to_string()),
        "the conflicting source row is skipped, not deleted"
    );
    assert_eq!(count(&db, "annotation"), 2, "no annotation was merged away");
    assert_eq!(
        cell_text(&db, "media_file", "average_rating", "mf-next"),
        Some("2.0".to_string()),
        "updateAvgRating ran on the TARGET over its own population {{2}}"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(position as text) FROM bookmark WHERE user_id = 'u-one' AND item_id = 'mf-next'",
            &[]
        ),
        Some("200".to_string()),
        "the target's own bookmark position survived"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(position as text) FROM bookmark WHERE user_id = 'u-one' AND item_id = 'mf-prev'",
            &[]
        ),
        Some("100".to_string()),
        "the conflicting bookmark stayed on prev_id"
    );
    assert_eq!(
        count(&db, "scrobble_buffer"),
        2,
        "the buffer's unique key made the second update an IGNORE, so no row was replaced \
         and none was inserted"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM scrobble_buffer WHERE media_file_id = 'mf-next'",
            &[]
        ),
        Some("mf-next".to_string())
    );

    // Go's ANNOTATION guard, executed through the same bundle: empty/equal ids run NEITHER
    // annotation statement and change nothing (`sql_annotations.go:186-188`). The bookmark leg
    // has no such guard (`sql_bookmarks.go:149-153`), so it still binds the empty id — it just
    // matches no row, since no real bookmark stores `item_id = ''`.
    let before = state_snapshot(&db);
    store_tx(&db, |tx| {
        reassign_references_in_tx(tx, AnnotationEntityKind::MediaFile, "mf-next", "mf-next")
    });
    assert_state_unchanged(&db, &before, "equal ids are a successful no-op");
    store_tx(&db, |tx| {
        reassign_references_in_tx(tx, AnnotationEntityKind::MediaFile, "", "mf-next")
    });
    assert_state_unchanged(
        &db,
        &before,
        "an empty prev_id changes nothing: the annotation leg short-circuits, the unguarded \
         bookmark leg binds '' and matches no row",
    );

    cleanup(db, path);
}

/// Commit one raw block through the typed seam exactly as a caller would.
fn store_tx<T>(
    store: &SQLStore,
    block: impl FnOnce(
        &rusqlite::Transaction<'_>,
    ) -> Result<T, nd_db::DbOperationError<std::convert::Infallible>>,
) -> T {
    store
        .try_with_tx_sync::<T, Infallible>(block)
        .unwrap_or_else(|e| panic!("transaction failed: {e}"))
}

/// Rollback injection INSIDE the bundle: a real `RAISE(ABORT)` trigger on the bookmark leg —
/// i.e. AFTER the annotation leg ran — aborts the transaction, so the remap fails with the
/// concrete trigger code and NO partial reference move survives. The pool comes back reusable.
#[test]
fn reassign_references_rollback_injection_leaves_no_partial_move() {
    let (db, path) = fresh_store("reassign-rollback");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_album(&db, "al-old", Some(old_album_stamp()), 7.7);
    seed_album(&db, "al-new", Some(new_album_stamp()), 7.7);
    media_row(
        &db,
        "mf-target",
        1,
        "al-new",
        "/music/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-old",
        "/music/gone.mp3",
        missing_stamp(),
        true,
    );
    seed_annotation(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        5,
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::Album, "al-old", 4);
    seed_bookmark(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-target",
        42,
    );
    seed_playlist_track(&db, "pl-1", "u-one", "mf-target");
    seed_scrobble(&db, "u-one", "mf-target", 1_700_000_000);
    seed_buffer(&db, "u-one", "mf-target", "2024-01-01 00:00:00+00:00");
    // Fires on the SECOND leg, so the annotation leg provably ran first and then rolled back.
    raise_trigger(
        &db,
        "CREATE TRIGGER fixture_poison BEFORE UPDATE ON bookmark FOR EACH ROW \
         WHEN OLD.item_type = 'media_file' AND OLD.item_id = 'mf-target' \
         BEGIN SELECT RAISE(ABORT, 'fixture: bookmark leg aborted'); END",
    );

    let before = state_snapshot(&db);
    let plan = MissingRemap {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        old_album_id: "al-old".to_string(),
    };

    let err = remap_missing_mediafile(&db, &plan)
        .expect_err("a driver failure in the bundle is fatal (core/maintenance.go:89-91)");
    assert_body_sqlite_failure(&err, Some(rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER));

    // Nothing moved: the annotation leg's effect rolled back with the transaction.
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM annotation WHERE item_type = 'media_file'",
            &[]
        ),
        Some("mf-target".to_string()),
        "the annotation leg ran but rolled back"
    );
    assert_eq!(
        reference_fingerprint(&db, "mf-target"),
        vec![
            "annotation",
            "bookmark",
            "playlist_tracks",
            "scrobbles",
            "scrobble_buffer"
        ],
        "every source row is intact"
    );
    assert_eq!(
        reference_fingerprint(&db, "mf-missing"),
        Vec::<&'static str>::new(),
        "nothing partial landed on the missing id"
    );
    assert_eq!(
        cell_text(&db, "media_file", "path", "mf-missing"),
        Some("/music/gone.mp3".to_string()),
        "the Put leg rolled back too"
    );
    assert_state_unchanged(&db, &before, "no table gained or lost a row");
    assert_eq!(count(&db, "playlist_tracks"), 1);
    assert_eq!(count(&db, "media_file"), 2);

    // Pool un-poisoned and reusable (`D-BATCH`'s post-failure contract).
    let probe: Result<u8, DbRuntimeError<Infallible>> =
        db.try_with_tx_sync::<u8, Infallible>(|tx| {
            Ok(tx
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .unwrap() as u8)
        });
    assert_eq!(probe.unwrap(), 1);
    assert!(
        lease(&db, |conn| conn.is_autocommit()),
        "the leased handle is idle"
    );

    cleanup(db, path);
}

/// `delete_mediafile_in_tx`'s cascade, source-verified from `nd-dbschema/src/schema_ddl.rs`
/// before pinning: the statement deletes ONLY the base row; `media_file_artists` (:73),
/// `media_file_tags` (:74), `scrobble_buffer` (:77) and `scrobbles` (:78) cascade via their
/// `ON DELETE CASCADE` FKs, and the `media_file_fts_ad` trigger (:83) drops the index entry.
/// `annotation` (:59) and `bookmark` (:60) reference `user` only, so they SURVIVE as orphans
/// until `GC`'s `cleanAnnotations`/`cleanBookmarks` (`persistence/persistence.go:269,271`),
/// and `playlist_tracks` (:76) is not cascaded. A zero-row delete is `Ok(0)`, not
/// `ErrNotFound` (`mediafile_repository.go:406-408` discards the count).
#[test]
fn delete_mediafile_removes_rows_and_junctions() {
    let (db, path) = fresh_store("delete-cascade");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    seed_artist(&db, "ar-main");
    let genre_tag = seed_tag(&db, 1, "genre", "Rock");

    // The junction rows come from the PRODUCTION DB-10 write, so they really exist under this
    // id before the delete (a fixture that never wrote them would make the cascade vacuous).
    media_row(
        &db,
        "mf-del",
        1,
        "al-1",
        "/music/del.flac",
        target_stamp(),
        false,
    );
    fixture(
        &db,
        "INSERT INTO media_file_artists (media_file_id, artist_id, role, sub_role) VALUES (?, ?, 'artist', '')",
        &[text("mf-del"), text("ar-main")],
    );
    fixture(
        &db,
        "INSERT INTO media_file_tags (media_file_id, tag_id) VALUES (?, ?)",
        &[text("mf-del"), text(&genre_tag)],
    );
    seed_annotation(&db, "u-one", AnnotationEntityKind::MediaFile, "mf-del", 5);
    seed_bookmark(&db, "u-one", AnnotationEntityKind::MediaFile, "mf-del", 12);
    seed_playlist_track(&db, "pl-1", "u-one", "mf-del");
    seed_scrobble(&db, "u-one", "mf-del", 1_700_000_000);
    seed_buffer(&db, "u-one", "mf-del", "2024-01-01 00:00:00+00:00");

    assert_eq!(
        lease(&db, |conn| conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap()),
        1,
        "FKs must be ON for the cascade to be observable, as in Go (db/db.go:55, :87)"
    );
    // The FTS index holds the row before the delete (contentless index: observable via MATCH).
    assert_eq!(
        lease(&db, |conn| conn
            .query_row(
                "SELECT count(*) FROM media_file_fts WHERE media_file_fts MATCH ?",
                params!["\"Title-mf-del\""],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()),
        1,
        "the `_ai` trigger indexed the row"
    );

    let deleted = store_tx(&db, |tx| delete_mediafile_in_tx(tx, "mf-del"));
    assert_eq!(deleted, 1, "advisory count of the base delete");

    assert!(read_attempt(&db, "mf-del").is_err(), "the base row is gone");
    assert_eq!(count(&db, "media_file_artists"), 0, "cascaded (:73)");
    assert_eq!(count(&db, "media_file_tags"), 0, "cascaded (:74)");
    assert_eq!(count(&db, "scrobbles"), 0, "cascaded (:78)");
    assert_eq!(count(&db, "scrobble_buffer"), 0, "cascaded (:77)");
    assert_eq!(count(&db, "artist"), 1, "the artist row is untouched");
    assert_eq!(count(&db, "tag"), 1, "the tag dictionary row is untouched");
    assert_eq!(
        count(&db, "annotation"),
        1,
        "annotation references user only (:59) and survives as an orphan until GC's \
         cleanAnnotations — that is Go's behavior and the reason ReassignReferences exists"
    );
    assert_eq!(
        count(&db, "bookmark"),
        1,
        "bookmark references user only (:60) and survives until cleanBookmarks (GC)"
    );
    assert_eq!(
        count(&db, "playlist_tracks"),
        1,
        "playlist_tracks FKs on playlist only (:76); Go's GC removeOrphans owns it"
    );
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(media_file_id as text) FROM playlist_tracks WHERE id = 1",
            &[]
        ),
        Some("mf-del".to_string())
    );
    assert_eq!(
        lease(&db, |conn| conn
            .query_row(
                "SELECT count(*) FROM media_file_fts WHERE media_file_fts MATCH ?",
                params!["\"Title-mf-del\""],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()),
        0,
        "the `_ad` trigger removed the index entry (:83)"
    );

    // A missing id is Go's successful no-op, not ErrNotFound.
    let again = store_tx(&db, |tx| delete_mediafile_in_tx(tx, "mf-never-existed"));
    assert_eq!(again, 0);

    cleanup(db, path);
}

/// Pin 4: `DeleteAllMissing` (`mediafile_repository.go:433-440`) is an OPT-IN primitive — its
//  admin gate reads Go's ambient logged user and cannot be enforced in this crate, so the only
/// caller-side guarantee a DB lane can prove is predicate correctness plus rollback.
#[test]
fn purge_missing_requires_system_policy_and_is_rollbackable() {
    let (db, path) = fresh_store("purge-missing");
    seed_library(&db, 1, "main");
    media_row(
        &db,
        "mf-m1",
        1,
        "al-1",
        "/music/gone-1.mp3",
        missing_stamp(),
        true,
    );
    media_row(
        &db,
        "mf-m2",
        1,
        "al-1",
        "/music/gone-2.mp3",
        missing_stamp(),
        true,
    );
    media_row(
        &db,
        "mf-keep",
        1,
        "al-1",
        "/music/keeper.flac",
        target_stamp(),
        false,
    );

    // Rolled back by a callback that fails AFTER the purge ran: nothing durable, so a caller
    // that aborts cannot leak an unauthorized purge. The admin gate itself
    // (`mediafile_repository.go:434-437`, `rest.ErrPermissionDenied`) reads Go's ambient
    // logged user and stays the caller's policy — this crate has no equivalent.
    let rolled_back = db.try_with_tx_sync::<usize, Infallible>(|tx| {
        let n = delete_all_missing_in_tx(tx)?;
        Err(DbOperationError::Sqlite(SqliteError::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER),
            Some("fixture: purge rolled back".to_string()),
        )))
        .map(|()| n)
    });
    match &rolled_back {
        Err(err) => {
            assert_body_sqlite_failure(err, Some(rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER));
        }
        Ok(n) => panic!("the injected failure must abort the purge, got Ok({n})"),
    }
    assert_eq!(
        count(&db, "media_file"),
        3,
        "the rolled-back purge left every row"
    );

    let deleted = store_tx(&db, delete_all_missing_in_tx);
    assert_eq!(deleted, 2, "exactly the two missing rows");
    assert_eq!(count(&db, "media_file"), 1);
    assert!(
        read_attempt(&db, "mf-keep").is_ok(),
        "a non-missing row survives"
    );
    assert!(read_attempt(&db, "mf-m1").is_err());
    assert!(read_attempt(&db, "mf-m2").is_err());

    // Zero matching rows is Ok(0).
    assert_eq!(store_tx(&db, delete_all_missing_in_tx), 0);

    cleanup(db, path);
}

/// Cross-library scope and unrelated data: the move takes the TARGET's `library_id` with it
/// (`media_file.library_id` is a plain column in the moved record, no library guard in Go's
//  `Put`) and touches nothing else — no other track, album, annotation or playlist row changes.
#[test]
fn cross_library_move_scope_and_unrelated_data() {
    let (db, path) = fresh_store("cross-library");
    seed_library(&db, 1, "main");
    seed_library(&db, 2, "decoy");
    seed_user(&db, "u-one");
    seed_album(&db, "al-1", Some(old_album_stamp()), 0.0);
    seed_album(&db, "al-decoy", Some(old_album_stamp()), 0.0);
    media_row(
        &db,
        "mf-target",
        2,
        "al-1",
        "/lib2/real.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-missing",
        1,
        "al-1",
        "/lib1/gone.mp3",
        missing_stamp(),
        true,
    );
    media_row(
        &db,
        "mf-unrelated",
        1,
        "al-decoy",
        "/lib1/other.flac",
        target_stamp(),
        false,
    );
    seed_annotation(
        &db,
        "u-one",
        AnnotationEntityKind::MediaFile,
        "mf-unrelated",
        3,
    );
    seed_playlist_track(&db, "pl-1", "u-one", "mf-unrelated");

    let before = state_snapshot(&db);
    let unrelated_before = read_back(&db, "mf-unrelated");
    let decoy_before = cell_text(&db, "album", "created_at", "al-decoy");

    let plan = SelectedMove {
        target: read_back(&db, "mf-target"),
        missing_id: "mf-missing".to_string(),
        missing_created_at: Some(carried_stamp()),
        now: now_stamp(),
        append_subtitle: false,
        album_transfer: None,
    };
    let outcome = move_matched_mediafile(&db, &plan)
        .unwrap_or_else(|e| panic!("move_matched_mediafile failed: {e}"));
    assert_eq!(outcome.id, "mf-missing");

    assert_eq!(
        cell_text(&db, "media_file", "library_id", "mf-missing"),
        Some("2".to_string()),
        "the moved row carries the TARGET's library (Go's Put writes the record's library_id)"
    );
    assert_eq!(count(&db, "media_file"), 2);
    assert_eq!(
        read_back(&db, "mf-unrelated").path,
        unrelated_before.path,
        "an unrelated track is untouched"
    );
    assert_eq!(
        cell_text(&db, "album", "created_at", "al-decoy"),
        decoy_before,
        "an unrelated album is untouched"
    );
    let after = state_snapshot(&db);
    let mut want = before.clone();
    // Exactly one media_file row disappears (the discard); everything else is unchanged.
    if let Some(slot) = want.iter_mut().find(|(t, _)| t == "media_file") {
        slot.1 -= 1;
    }
    assert_eq!(after, want, "only the move's own rows changed");

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// A8 / D-SHARE — the share empty-column guard
// ---------------------------------------------------------------------------

/// A8 (`decisions.md`, parent-decided): an empty SET-column list is a TYPED rejection raised
/// BEFORE any SQL text exists, so the `UPDATE share SET  WHERE id = ?` shape SQLite would
/// reject with `near "WHERE": syntax error` is unreachable. Go cannot produce that shape at
/// all — `filterUpdateValues` re-adds `id` (`sql_base_repository.go:612-616`) — and its only
/// wrapper discards the incoming columns and always sends 2-3 (`core/share.go:141-148`).
#[test]
fn share_empty_cols_typed_rejection() {
    let (db, path) = fresh_store("share-empty");
    seed_user(&db, "u-share");
    let err = lease(&db, |conn| {
        let repo = nd_db::share_repo::ShareRepo::new(conn);
        repo.update("tok-1", &[], None)
            .expect_err("an empty column list is a typed rejection")
    });
    assert!(matches!(
        err,
        nd_db::share_repo::ShareError::EmptyUpdateColumns
    ));
    assert_eq!(
        err.to_string(),
        "share update requires at least one column",
        "the rejection text is part of the typed surface"
    );
    let owned = nd_db::share_repo::update_sql("tok-1", &[], Some("u-share".to_string()));
    assert!(matches!(
        owned,
        Err(nd_db::share_repo::ShareError::EmptyUpdateColumns)
    ));
    let plain = nd_db::share_repo::update_sql("tok-1", &[], None);
    assert!(matches!(
        plain,
        Err(nd_db::share_repo::ShareError::EmptyUpdateColumns)
    ));

    cleanup(db, path);
}

/// Regression: the production 2-3 column paths still render and execute exactly as before —
/// the visit bump's `last_visited_at`/`visit_count` (`core/share.go:51`) plus `updated_at`,
/// which Go appends only when `cols` is non-empty (`share_repository.go:162-164`). `created_at`
/// and `user_id` stay untouched (`:605`, `sql_base_repository.go:512`).
#[test]
fn share_nonempty_update_unchanged_behavior() {
    let (db, path) = fresh_store("share-nonempty");
    seed_user(&db, "u-share");
    let created = at(2024, 1, 1, 0, 0, 0);
    let updated = at(2024, 1, 2, 0, 0, 0);
    let visited = at(2024, 3, 3, 3, 3, 3);
    let record = nd_db::share_repo::ShareRecord {
        id: "tok-1".to_string(),
        user_id: "u-share".to_string(),
        expires_at: None,
        last_visited_at: None,
        created_at: created,
        updated_at: updated,
        resource_ids: "mf-1".to_string(),
        resource_type: "media_file".to_string(),
        contents: String::new(),
        format: "zip".to_string(),
        downloadable: false,
        description: String::new(),
        max_bit_rate: 0,
        visit_count: 0,
    };
    lease(&db, |conn| {
        nd_db::share_repo::ShareRepo::new(conn)
            .save(&record)
            .expect("row seeds");
    });

    let cols = [
        nd_db::share_repo::Col::timestamp("last_visited_at", &visited),
        nd_db::share_repo::Col::integer("visit_count", 7),
        nd_db::share_repo::Col::timestamp("updated_at", &at(2024, 4, 4, 4, 4, 4)),
    ];
    lease(&db, |conn| {
        nd_db::share_repo::ShareRepo::new(conn)
            .update("tok-1", &cols, Some("u-share".to_string()))
            .expect("the production 3-column update still works")
    });

    let found = lease(&db, |conn| {
        nd_db::share_repo::ShareRepo::new(conn)
            .get("tok-1", Some("u-share".to_string()))
            .expect("the row is readable")
    });
    assert_eq!(found.share.visit_count, 7);
    assert_eq!(found.share.last_visited_at, Some(visited));
    assert_eq!(found.share.updated_at, at(2024, 4, 4, 4, 4, 4));
    assert_eq!(
        found.share.created_at, created,
        "created_at is never updated (:605)"
    );
    assert_eq!(
        found.share.user_id, "u-share",
        "user_id is never a SET column (sql_base_repository.go:512)"
    );
    assert_eq!(found.username, "u-share");

    // The renderer's own shape, unchanged by the guard: SET pairs in order, then id, then the
    // owner restriction.
    let (sql, args) = nd_db::share_repo::update_sql("tok-1", &cols, Some("u-share".to_string()))
        .expect("a non-empty list renders");
    assert_eq!(
        sql,
        "UPDATE share SET last_visited_at = ?, visit_count = ?, updated_at = ? WHERE id = ? AND user_id = ?"
    );
    assert_eq!(args.len(), 5);
    assert!(!sql.contains("SET  WHERE"), "{sql}");

    // An owner mismatch restricts the UPDATE, so the row is not touched (Go's owned-write
    // miss; the port pins it as NotFound).
    let foreign = lease(&db, |conn| {
        nd_db::share_repo::ShareRepo::new(conn).update("tok-1", &cols, Some("u-other".to_string()))
    });
    assert!(matches!(
        foreign,
        Err(nd_db::share_repo::ShareError::NotFound)
    ));

    cleanup(db, path);
}

// ---------------------------------------------------------------------------
// D-PLUGIN — module deleted, class still probed with literal SQL
// ---------------------------------------------------------------------------

/// `plugin_repo.rs` and its `lib.rs` export are DELETED (`decisions.md` `D-PLUGIN`,
/// "Deletion assignment (2026-11-21)"). Two executable proofs: (a) no `plugin_repo`
/// identifier and no `plugin_repo.rs` file remain anywhere under `src/` — compilation would
/// also fail on any lingering reference, but this asserts the census directly; (b) the
/// deferred-gate class stays real as a LITERAL statement: the `plugin` table is dropped, so
/// anything naming it fails on prepare with `no such table: plugin`.
#[test]
fn plugin_module_deleted_and_plugin_table_still_absent() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let plugin_module = src.join("plugin_repo.rs");
    let mut sources: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{dir:?}: {e}")) {
            let path = entry
                .unwrap_or_else(|e| panic!("{dir:?} entry: {e}"))
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                sources.push(path);
            }
        }
    }
    assert!(
        !plugin_module.exists(),
        "{plugin_module:?} must be deleted (D-PLUGIN)"
    );
    for path in &sources {
        let body = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        assert!(
            !body.contains("plugin_repo"),
            "{path:?} still mentions `plugin_repo`, which no longer exists (D-PLUGIN)"
        );
    }
    // The lib.rs export is gone too.
    let lib = std::fs::read_to_string(manifest.join("src").join("lib.rs")).expect("lib.rs");
    assert!(
        !lib.contains("pub mod plugin_repo"),
        "lib.rs still exports plugin_repo"
    );

    // The class itself, probed with a literal statement in Go's shape.
    let (db, path) = fresh_store("plugin-absent");
    let err = lease(&db, |conn| {
        conn.prepare("SELECT * FROM plugin WHERE id = ?")
            .err()
            .map(|e| e.to_string())
            .expect("the plugin table must not exist")
    });
    assert!(err.contains("no such table: plugin"), "{err}");
    // `share`/`user` prove the schema really was applied, so the failure is the dropped
    // table and not an unmigrated database.
    assert_eq!(count(&db, "user"), 0);
    assert_eq!(count(&db, "share"), 0);

    // The remaining copy-helper assertion: `copy_album_created_at_in_tx` is DB-14's single
    // helper (pin 3) and is reachable from this crate, so a second copy implementation is
    // unnecessary — assert the symbol exists rather than re-deriving its behavior here.
    let (db2, path2) = fresh_store("copy-helper-reachable");
    seed_library(&db2, 1, "main");
    seed_album(&db2, "al-src", Some(old_album_stamp()), 0.0);
    seed_album(&db2, "al-dst", Some(new_album_stamp()), 0.0);
    store_tx(&db2, |tx| {
        copy_album_created_at_in_tx(tx, "al-src", "al-dst").inspect(|o| {
            assert_eq!(*o, CopyAlbumCreatedAtOutcome::Copied);
        })
    });
    assert_eq!(
        cell_text(&db2, "album", "created_at", "al-dst"),
        Some(time::to_text(&old_album_stamp()))
    );

    cleanup(db, path);
    cleanup(db2, path2);
}

/// `reassign_bookmark_sql` is the ONLY bookmark statement the bundle executes, and the
/// in-tx leg runs it verbatim on the caller's transaction. Go has NO guard here
/// (`sql_bookmarks.go:149-153`) and neither does this port, so an id pair is never refused:
/// an empty `prev_id` simply matches no row (real ids are never `''`), equal ids re-write the
/// same value, and an empty `new_id` WOULD move matched rows to `''` exactly as Go's
/// `update or ignore` would — that shape is not exercised here, because the production bundle
/// only ever passes ids read back from real rows. A rerun moves nothing new.
#[test]
fn bookmark_in_tx_leg_executes_the_renderer_and_is_retryable() {
    let (db, path) = fresh_store("bookmark-leg");
    seed_library(&db, 1, "main");
    seed_user(&db, "u-one");
    media_row(
        &db,
        "mf-a",
        1,
        "al-1",
        "/music/a.flac",
        target_stamp(),
        false,
    );
    media_row(
        &db,
        "mf-b",
        1,
        "al-1",
        "/music/b.flac",
        missing_stamp(),
        false,
    );
    seed_bookmark(&db, "u-one", AnnotationEntityKind::MediaFile, "mf-a", 5);

    store_tx(&db, |tx| {
        nd_db::sql_bookmarks::reassign_bookmark_in_tx(
            tx,
            AnnotationEntityKind::MediaFile,
            "mf-a",
            "mf-b",
        )
    });
    assert_eq!(
        cell_by(
            &db,
            "SELECT cast(item_id as text) FROM bookmark WHERE user_id = 'u-one' AND item_type = 'media_file'",
            &[]
        ),
        Some("mf-b".to_string())
    );

    // Retryable: a rerun converges and moves nothing further.
    let before = state_snapshot(&db);
    store_tx(&db, |tx| {
        nd_db::sql_bookmarks::reassign_bookmark_in_tx(
            tx,
            AnnotationEntityKind::MediaFile,
            "mf-a",
            "mf-b",
        )
    });
    assert_state_unchanged(&db, &before, "the rerun is a no-op");

    // An empty `prev_id` matches no row — no guard, just statement semantics — pinned against
    // the renderer's own text: the empty id is BOUND, never short-circuited.
    let (sql, args) = reassign_bookmark_sql("media_file", "", "mf-b");
    assert_eq!(
        sql,
        "update or ignore bookmark set item_id = ? where item_type = ? and item_id = ?"
    );
    assert_eq!(
        args,
        vec![
            Value::from("mf-b"),
            Value::from("media_file"),
            Value::from(""),
        ]
    );
    store_tx(&db, |tx| {
        nd_db::sql_bookmarks::reassign_bookmark_in_tx(
            tx,
            AnnotationEntityKind::MediaFile,
            "",
            "mf-b",
        )
    });
    assert_state_unchanged(
        &db,
        &before,
        "an empty prev_id matches no row (there is no guard) and is not an error",
    );

    cleanup(db, path);
}
