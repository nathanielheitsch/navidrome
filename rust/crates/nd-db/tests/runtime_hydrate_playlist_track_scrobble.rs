//! S-D3e — `playlist_track` + `scrobble_buffer` hydration, executed against the
//! real fresh schema (`nd_dbschema::schema::apply_fresh_schema`), never against
//! rendered SQL strings.
//!
//! Every assertion mirrors a Go behavior verified in this checkout:
//! * `persistence/playlist_track_repository.go:27-42` — `dbPlaylistTrack.PostScan`
//!   delegates to `dbMediaFile.PostScan`, copies the hydrated media file onto the
//!   track (`:31`), then OVERWRITES `MediaFile.ID = t.MediaFileID` (`:32`).
//! * `persistence/scrobble_buffer_repository.go:23-30` — the same shape for
//!   `dbScrobbleBuffer` (copy `:27`, overwrite `:28`).
//! * `playlist_track_repository.go:98-121` (`Read`, `queryOne` at `:119`) and
//!   `playlist_repository.go:374-408` (`tracksQuery`/`loadTracks`, with the
//!   `library_path`/`library_name` legs `Read` does not select, and the
//!   `hydratePlaylistTrackArtwork` call at `:406`).
//! * `scrobble_buffer_repository.go:65-89` — `Next`'s `SELECT m.*, s.*` LEFT JOIN,
//!   `ORDER BY play_time, s.rowid LIMIT 1`, and the `(nil, nil)` empty-queue
//!   contract at `:78-80`.
//!
//! The duplicated-name `id` rule — dbx binds every result column named `id` to the
//! SAME field and `sql.Rows.Scan` converts in emitted order, so the last non-NULL
//! cell wins — is pinned in both column orders, so the behavior does not rest on
//! Go's column ordering alone.

use nd_db::album_repo::{self, AlbumRecord};
use nd_db::artwork_hydration::{self, ItemImage};
use nd_db::execution;
use nd_db::hydrate::{self, RowGet};
use nd_db::mediafile_repo::{self, MediaFileRecord};
use nd_db::playlist_repo;
use nd_db::playlist_track_repo::{
    self, PlaylistTrackError, PlaylistTrackRecord, PlaylistTrackRepo,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::scrobble_buffer_repo::{ScrobbleBufferError, ScrobbleBufferRepo, ScrobbleEntryRecord};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
/// A stored offset the driver preserves, so the wire text stays `+03:00`.
const STARRED_AT: &str = "2024-03-03 03:00:00+03:00";
const LIB_PATH: &str = "/music";

// ---------------------------------------------------------------------------
// fixtures — same shape as runtime_hydrate_album_mediafile.rs
// ---------------------------------------------------------------------------

fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-hydrate-ptsb-",
        ".db",
    ));
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

fn bind(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

fn v(s: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(s.to_string())
}
fn i(n: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(n)
}

fn insert(store: &SQLStore, sql: &str, args: &[rusqlite::types::Value]) -> usize {
    store
        .with_connection_sync(|conn| conn.execute(sql, rusqlite::params_from_iter(args.iter())))
        .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"))
}

fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<T, rusqlite::Error> {
    Ok(store
        .with_connection_sync(|conn| call(conn))
        .unwrap_or_else(|e| panic!("store lease failed: {e}")))
}

/// Repo calls return the repo's own error type, so the lease hands back whatever
/// the closure produced.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

fn stamp(text: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|e| panic!("bad stamp {text}: {e}"))
        .with_timezone(&chrono::Utc)
}

fn seed_library(store: &SQLStore, id: i64, name: &str) {
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[
            i(id),
            v(name),
            rusqlite::types::Value::Text(format!("{LIB_PATH}-{name}")),
            v(""),
        ],
    );
}

fn seed_user(store: &SQLStore, id: &str, is_admin: bool) {
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(id),
            v(id),
            v(id),
            rusqlite::types::Value::Text(format!("{id}@x.y")),
            v("CIPHER"),
            i(i64::from(is_admin)),
            v(T0),
            v(T1),
        ],
    );
}

fn grant_library(store: &SQLStore, user_id: &str, lib_id: i64) {
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[v(user_id), i(lib_id)],
    );
}

/// The annotation row a logged-in user's LEFT JOIN picks up. `play_date` stays
/// NULL so the annotation legs exercise both a NULL timestamp and a coalesced
/// count.
fn seed_annotation(store: &SQLStore, user_id: &str, item_type: &str, item_id: &str) {
    insert(
        store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, play_date, rating, \
         starred, starred_at, rated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(user_id),
            v(item_id),
            v(item_type),
            i(7),
            rusqlite::types::Value::Null,
            i(4),
            i(1),
            v(STARRED_AT),
            v(T1),
        ],
    );
}

/// `item_artwork` state row at `image_type = 'primary'` — the only slot
/// `GetInfoForItems` reads (`artwork_repository.go:168-172`, the `And` whose
/// `image_type` leg binds `model.ImageTypePrimary` at `:170`). `hash = ""` is the
/// known-absent state (`model/artwork.go:50`, reported by `ItemArtworkInfo.Absent()`
/// at `:82`).
fn seed_state(store: &SQLStore, kind: &str, id: &str, hash: &str) {
    insert(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, updated_at) \
         VALUES (?, ?, 'primary', ?, 'folder', ?)",
        &[v(kind), v(id), v(hash), v(T1)],
    );
}

/// The `artwork` image row the hydration query's `COALESCE` arms read.
fn seed_image(store: &SQLStore, hash: &str, width: i64, height: i64, blur: &str, thumb: &str) {
    insert(
        store,
        "INSERT INTO artwork (hash, mime, width, height, size_bytes, blur_hash, thumb_hash, \
         dominant_color) VALUES (?, 'image/jpeg', ?, ?, 100, ?, ?, '#aabbcc')",
        &[v(hash), i(width), i(height), v(blur), v(thumb)],
    );
}

fn seed_album(store: &SQLStore, lib_id: i64, id: &str) {
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
        discs: None,
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
        description: "desc".to_string(),
        small_image_url: "http://img/s".to_string(),
        medium_image_url: "http://img/m".to_string(),
        large_image_url: "http://img/l".to_string(),
        external_url: "http://ext".to_string(),
        external_info_updated_at: Some(stamp(T1)),
        genre: "ColumnGenre".to_string(),
        tags: nd_db::sql_tags::DbTags::new(),
        participants: Participants::new(),
        participant_names: Vec::new(),
        missing: false,
        imported_at: stamp(T1),
        created_at: Some(stamp(T0)),
        updated_at: Some(stamp(T1)),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        image: ItemImage::default(),
    };
    let put = album_repo::put_sql(id, &record, stamp(T1), &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(store, &put.insert.0, &args);
}

/// A media file written through the real media-file write map, carrying the
/// `participants`/`tags` shims, the four RG columns and lyrics — the proof that a
/// track/buffer row reaches the FULL media-file post-scan.
/// `library_path` for a seeded library (the `library` table has a unique path).
fn lib_path(lib_id: i64) -> String {
    if lib_id == 1 {
        format!("{LIB_PATH}-Main")
    } else {
        format!("{LIB_PATH}-Extra")
    }
}

fn seed_mediafile(store: &SQLStore, lib_id: i64, id: &str, has_cover_art: bool) {
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
        disc_number: 0,
        disc_subtitle: "d-s".to_string(),
        duration: 24.5,
        explicit_status: "E".to_string(),
        folder_id: "fold-1".to_string(),
        genre: "ColumnGenre".to_string(),
        has_cover_art,
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
        path: format!("{}/{}.flac", lib_path(lib_id), id),
        pid: format!("pid-{id}"),
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
        title: format!("Track {id}"),
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
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: ItemImage::default(),
        album_image: ItemImage::default(),
    };
    let put = mediafile_repo::put_sql(id, &record, stamp(T1), false, &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind).collect();
    insert(store, &put.insert.0, &args);
}

fn seed_playlist(store: &SQLStore, id: &str, owner: &str) {
    insert(
        store,
        "INSERT INTO playlist (id, name, owner_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
        &[v(id), v("Mix"), v(owner), v(T0), v(T1)],
    );
}

/// `playlist_tracks.id` (the position) deliberately DIFFERS from
/// `media_file_id` — the case the ID-overwrite hook exists for.
fn seed_track(store: &SQLStore, playlist_id: &str, position: i64, media_file_id: &str) {
    insert(
        store,
        "INSERT INTO playlist_tracks (id, playlist_id, media_file_id) VALUES (?, ?, ?)",
        &[i(position), v(playlist_id), v(media_file_id)],
    );
}

/// Two libraries, an admin and a limited user, one album, three media files
/// (`m1` lib 1 w/ own art + has_cover_art, `m2` lib 1 without own art, `m3` lib 2
/// w/ own art), a playlist with positions 1/2/3 bound to those ids, and an
/// annotation for `lim` on `m1` only.
fn fixture() -> SQLStore {
    let (store, _path) = fresh_store();
    seed_library(&store, 1, "Main");
    seed_library(&store, 2, "Extra");
    seed_user(&store, "adm", true);
    seed_user(&store, "lim", false);
    grant_library(&store, "lim", 1);
    seed_album(&store, 1, "al-1");
    seed_mediafile(&store, 1, "m1", true);
    seed_mediafile(&store, 1, "m2", false);
    seed_mediafile(&store, 2, "m3", true);
    seed_playlist(&store, "pl-1", "adm");
    seed_track(&store, "pl-1", 1, "m1");
    seed_track(&store, "pl-1", 2, "m2");
    seed_track(&store, "pl-1", 3, "m3");
    seed_annotation(&store, "lim", "media_file", "m1");
    seed_annotation(&store, "lim", "album", "al-1");
    store
}

/// The clause Go's `applyLibraryFilter(ctx, sel, "f")` produces for a user who
/// cannot see every library.
fn lib_filter(user_id: &str) -> (String, Vec<Value>) {
    playlist_track_repo::library_filter_on(Some(user_id), "f").expect("a filter for a limited user")
}

fn read_track(
    store: &SQLStore,
    position: &str,
    user: &str,
    filter: Option<&(String, Vec<Value>)>,
) -> Result<PlaylistTrackRecord, PlaylistTrackError> {
    lease(store, |conn| {
        PlaylistTrackRepo::new(conn, "pl-1").read(position, user, filter)
    })
}

fn get_all(
    store: &SQLStore,
    user: &str,
    filter: Option<&(String, Vec<Value>)>,
) -> Result<nd_db::artwork_hydration::ReadOutcome<PlaylistTrackRecord>, PlaylistTrackError> {
    lease(store, |conn| {
        PlaylistTrackRepo::new(conn, "pl-1").get_all(user, filter)
    })
}

fn enqueue(
    store: &SQLStore,
    id: &str,
    user: &str,
    media_file_id: &str,
    play_time: &str,
    service: &str,
) {
    insert(
        store,
        "INSERT INTO scrobble_buffer (id, user_id, service, media_file_id, play_time, enqueue_time) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &[v(id), v(user), v(service), v(media_file_id), v(play_time), v(T1)],
    );
}

fn next_entry(
    store: &SQLStore,
    service: &str,
    user: &str,
) -> Result<Option<ScrobbleEntryRecord>, ScrobbleBufferError> {
    lease(store, |conn| {
        ScrobbleBufferRepo::new(conn).next(service, user)
    })
}

/// Column names straight out of the fresh DDL — nothing hand-derived.
fn table_columns(store: &SQLStore, table: &str) -> Vec<String> {
    on(store, |conn| {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?) ORDER BY cid")?;
        let rows = stmt.query_map([table], |row| row.get::<usize, String>(0))?;
        rows.collect::<Result<Vec<String>, rusqlite::Error>>()
    })
    .expect("pragma reads")
}

fn columns_of(store: &SQLStore, statement: &(String, Vec<Value>)) -> Vec<String> {
    on(store, |conn| {
        execution::query_optional(conn, statement, |row| {
            Ok(row
                .as_ref()
                .column_names()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>())
        })
    })
    .expect("statement runs")
    .expect("one row")
}

fn scalar(store: &SQLStore, statement: &(String, Vec<Value>)) -> i64 {
    on(store, |conn| {
        execution::query_optional(conn, statement, |row| row.get::<usize, i64>(0))
    })
    .expect("count runs")
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 1. the media_file_id → media_file.id overwrite, both repos
// ---------------------------------------------------------------------------

/// `playlist_track_repository.go:31-32`: the hydrated media file is copied onto
/// the track and its `ID` is OVERWRITTEN with `MediaFileID`. The position lives in
/// `playlist_tracks.id`, so the two ids genuinely differ here.
#[test]
fn playlist_track_media_file_id_is_overwritten_with_media_file_id() {
    let store = fixture();
    let track = read_track(&store, "2", "adm", None).expect("read");
    assert_eq!(track.id, "2", "playlist_tracks.id is the position");
    assert_eq!(track.media_file_id, "m2");
    assert_eq!(
        track.media.id, "m2",
        "PostScan overwrote MediaFile.ID with MediaFileID (not the position)"
    );
    assert_eq!(
        track.media.title, "Track m2",
        "the media file came from the joined row, not the position"
    );

    // Position 1 proves the same for a row whose position/id pair differs too.
    let one = read_track(&store, "1", "adm", None).expect("read");
    assert_eq!(one.id, "1");
    assert_eq!(one.media.id, "m1");
}

/// The same hook for `dbScrobbleBuffer` (`scrobble_buffer_repository.go:27-28`):
/// the entry keeps its own id and the media file gets `MediaFileID`.
#[test]
fn scrobble_entry_media_file_id_is_overwritten_with_media_file_id() {
    let store = fixture();
    enqueue(&store, "sb-1", "lim", "m1", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("one entry");
    assert_eq!(entry.id, "sb-1", "the entry's own id");
    assert_eq!(entry.media_file_id, "m1");
    assert_eq!(entry.media.id, "m1", "the hook's overwrite");
    assert_eq!(entry.media.title, "Track m1");
    assert_eq!(entry.service, "svc");
    assert_eq!(entry.user_id, "lim");
}

// ---------------------------------------------------------------------------
// 2. the duplicated-name `id` field, in both column orders
// ---------------------------------------------------------------------------

/// dbx maps EVERY result column named `id` to the SAME field (one `dbNameMap`
/// entry per name) and `Scan` converts columns in emitted order, so the LAST
/// non-NULL `id` cell wins. Executed in both orders: the repo statement's own
/// order yields the position, and the reversed order yields the media file's id —
/// exactly why Go comments "Put `s.*` last or else `m.id` overrides `s.id`".
#[test]
fn duplicated_id_field_takes_the_last_emitted_id_column() {
    let store = fixture();

    // Go's own order (`f.*, playlist_tracks.*`) through the repo statement.
    let track = read_track(&store, "1", "adm", None).expect("read");
    assert_eq!(track.id, "1");
    assert_eq!(track.media.id, "m1");

    // Reversed order, through the seam helper directly.
    let reversed = on(&store, |conn| {
        execution::query_optional(
            conn,
            &(
                "SELECT playlist_tracks.*, f.* FROM playlist_tracks \
                 INNER JOIN media_file f ON f.id = media_file_id \
                 WHERE playlist_id = 'pl-1' AND playlist_tracks.id = 1"
                    .to_string(),
                Vec::new(),
            ),
            hydrate::id_field_from_row,
        )
    })
    .expect("reversed select runs");
    assert_eq!(
        reversed.expect("one row"),
        "m1",
        "with the media file's id emitted LAST, that value wins the shared field"
    );

    // scrobble Next's own projection: `s.*` last, so the entry id wins.
    enqueue(&store, "sb-id", "lim", "m2", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("entry");
    assert_eq!(
        entry.id, "sb-id",
        "`s.id` is the last `id` column of m.*, s.*"
    );
    assert_eq!(
        entry.media.id, "m2",
        "…while the media file's id comes from the hook"
    );

    // A media-file-only projection binds the track's ID field to the media file's
    // own id (the single `id` cell).
    let media_only = on(&store, |conn| {
        execution::query_optional(
            conn,
            &(
                "SELECT f.* FROM media_file f WHERE f.id = 'm2'".to_string(),
                Vec::new(),
            ),
            hydrate::id_field_from_row,
        )
    })
    .expect("media-only select runs");
    assert_eq!(media_only.expect("one row"), "m2");

    // No `id` column at all leaves Go's zero value: dbx builds a ref only for
    // names it knows, and an unmatched column goes into a throwaway NullString.
    let none = on(&store, |conn| {
        execution::query_optional(
            conn,
            &("SELECT 1 as unrelated".to_string(), Vec::new()),
            hydrate::id_field_from_row,
        )
    })
    .expect("projection with no id runs");
    assert_eq!(
        none.expect("one row"),
        "",
        "no id column ⇒ Go's zero string"
    );
}

/// rusqlite's own name lookup resolves a duplicate to the FIRST index, so a naive
/// `get_text("id")` reads `m.id` — the trap `id_field_from_row` exists to avoid.
#[test]
fn naive_name_lookup_would_take_the_first_id_column() {
    let store = fixture();
    let statement = playlist_repo::load_tracks_sql("pl-1", "adm", None);
    let (first, last) = on(&store, |conn| {
        execution::query_optional(conn, &statement, |row| {
            let stmt = row.as_ref();
            let mut first_idx = None;
            let mut last_idx = None;
            for idx in 0..stmt.column_count() {
                if stmt.column_name(idx)? == "id" {
                    if first_idx.is_none() {
                        first_idx = Some(idx);
                    }
                    last_idx = Some(idx);
                }
            }
            let first = first_idx.expect("the projection emits two id columns");
            let last = last_idx.expect("the projection emits two id columns");
            // Go's `reflect.String` destination renders any non-NULL cell through
            // `asString`, so the INTEGER position column becomes its decimal text.
            Ok((row.get_text_at(first)?, row.get_text_at(last)?))
        })
    })
    .expect("tracks select runs")
    .expect("one row");
    assert_eq!(first, "m1", "`f.*` precedes `playlist_tracks.*`");
    assert_eq!(last, "1", "the position is the LAST id column");

    let naive = on(&store, |conn| {
        execution::query_optional(conn, &statement, |row| row.get_text("id"))
    })
    .expect("tracks select runs")
    .expect("one row");
    assert_eq!(
        naive, first,
        "get_text(\"id\") resolves the FIRST index — not the field dbx binds"
    );
}

// ---------------------------------------------------------------------------
// 3. annotation columns: with and without an annotation row, Read vs tracksQuery
// ---------------------------------------------------------------------------

/// `Read` (`:98-122`) LEFT-joins annotation on `media_file_id`, so a track with no
/// annotation row gets Go's zero values and a track with one gets the coalesced
/// and real cells.
#[test]
fn read_annotation_columns_track_the_annotation_row() {
    let store = fixture();
    let filter = lib_filter("lim");

    let with_row = read_track(&store, "1", "lim", Some(&filter)).expect("read m1");
    assert!(with_row.media.annotation.starred);
    assert_eq!(with_row.media.annotation.play_count, 7);
    assert_eq!(with_row.media.annotation.rating, 4);
    assert_eq!(
        with_row.media.annotation.starred_at.as_deref(),
        Some("2024-03-03T03:00:00+03:00"),
        "the stored offset survives (the port renders Go's RFC3339 marshal form, \
         which rewrites the space as `T` — a documented residual)"
    );
    assert_eq!(
        with_row.media.annotation.rated_at.as_deref(),
        Some("2024-02-02T00:00:00Z"),
        "same RFC3339 rendering as starred_at (documented residual)"
    );
    assert_eq!(
        with_row.media.annotation.play_date, None,
        "a NULL play_date scans as Go's zero time, which the port maps to None"
    );

    let no_row = read_track(&store, "2", "lim", Some(&filter)).expect("read m2");
    assert_eq!(
        no_row.media.annotation,
        nd_model::annotation::Annotation::default(),
        "LEFT-JOIN miss ⇒ coalesce(0)/NULL timestamps ⇒ Go's zero annotation"
    );

    // Another user sees no annotation: the join is keyed on user_id.
    let other = read_track(&store, "1", "adm", None).expect("read as admin");
    assert_eq!(
        other.media.annotation,
        nd_model::annotation::Annotation::default(),
        "the annotation join is bound to the logged-in user"
    );

    // get_all hydrates the same legs (tracksQuery emits them too).
    let all = get_all(&store, "lim", Some(&filter)).expect("get_all");
    assert_eq!(all.items[0].media.annotation.play_count, 7);
    assert_eq!(
        all.items[1].media.annotation,
        nd_model::annotation::Annotation::default()
    );
}

/// `tracksQuery` (`playlist_repository.go:374-397`) differs from `Read`
/// (`playlist_track_repository.go:105-114`) verifiably: different column
/// spellings/order, and the extra `library_path`/`library_name` legs. Asserted
/// against the executed projection, not by string equality.
#[test]
fn read_and_tracks_query_project_different_columns() {
    let store = fixture();

    let read_columns = columns_of(
        &store,
        &playlist_track_repo::read_sql("pl-1", "1", "lim", None),
    );
    let query_columns = columns_of(&store, &playlist_repo::load_tracks_sql("pl-1", "lim", None));

    // Read: coalesce triple first, then the three timestamps
    // (playlist_track_repository.go:106-113).
    assert_eq!(
        &read_columns[..6],
        [
            "starred",
            "play_count",
            "rating",
            "starred_at",
            "play_date",
            "rated_at"
        ]
    );
    // tracksQuery: each coalesce paired with its timestamp
    // (playlist_repository.go:379-388).
    assert_eq!(
        &query_columns[..6],
        [
            "starred",
            "starred_at",
            "play_count",
            "play_date",
            "rating",
            "rated_at"
        ]
    );

    // Both end with playlist_tracks.* (id, playlist_id, media_file_id), but only
    // tracksQuery appends the library legs after it.
    assert_eq!(
        read_columns[read_columns.len() - 3..],
        ["id", "playlist_id", "media_file_id"]
    );
    assert!(
        !read_columns
            .iter()
            .any(|c| c == "library_path" || c == "library_name"),
        "Read selects no library legs at all"
    );
    assert_eq!(
        query_columns[query_columns.len() - 2..],
        ["library_path", "library_name"],
        "the library legs exist only in tracksQuery"
    );

    // Executed consequence: the legs hydrate through get_all and stay empty for
    // read, although both hydrate the same MediaFileRecord type.
    let one = read_track(&store, "1", "lim", Some(&lib_filter("lim"))).expect("read");
    assert_eq!(one.media.library_path, "");
    assert_eq!(one.media.library_name, "");
    let all = get_all(&store, "lim", Some(&lib_filter("lim"))).expect("get_all");
    assert_eq!(
        all.items[0].media.library_path,
        lib_path(1),
        "the join leg hydrates"
    );
    assert_eq!(all.items[0].media.library_name, "Main");
}

/// Go applies the library filter LAST in `Read` (`:120`) and FIRST in
/// `tracksQuery` (`:375`), which changes where the filter's bound arg sits
/// relative to the playlist id. Pinned on the executed arg order.
#[test]
fn library_filter_arg_order_differs_between_read_and_tracks_query() {
    let store = fixture();
    let filter = lib_filter("lim");
    let (_read_sql, read_args) = playlist_track_repo::read_sql("pl-1", "1", "lim", Some(&filter));
    let (_, query_args) = playlist_repo::load_tracks_sql("pl-1", "lim", Some(&filter));

    // Read: WHERE playlist_id = ? AND playlist_tracks.id = ? AND <filter>
    // ⇒ filter arg LAST.
    assert_eq!(
        read_args,
        vec![
            Value::Text("pl-1".to_string()),
            Value::Text("1".to_string()),
            Value::Text("lim".to_string()),
        ]
    );
    // tracksQuery: filter applied before its own Where ⇒ filter arg FIRST.
    assert_eq!(
        query_args,
        vec![
            Value::Text("lim".to_string()),
            Value::Text("pl-1".to_string())
        ],
        "Go applies applyLibraryFilter FIRST (:375), so its arg binds before the playlist id"
    );
    drop(store);
}

// ---------------------------------------------------------------------------
// 4. library-filter exclusion
// ---------------------------------------------------------------------------

/// A track in an unseen library becomes a no-row case, which `queryOne` turns into
/// `model.ErrNotFound` — not an error string.
#[test]
fn library_filter_excludes_tracks_from_unseen_libraries() {
    let store = fixture();
    let filter = lib_filter("lim");

    assert!(read_track(&store, "1", "lim", Some(&filter)).is_ok());
    assert!(read_track(&store, "2", "lim", Some(&filter)).is_ok());
    let excluded = read_track(&store, "3", "lim", Some(&filter));
    assert!(
        matches!(excluded, Err(PlaylistTrackError::NotFound)),
        "a filtered-out track is Go's ErrNotFound, got {excluded:?}"
    );
    assert_eq!(
        excluded.expect_err("NotFound").to_string(),
        "data not found",
        "Go's rest.ErrNotFound text (`model/errors.go:10`)"
    );

    // Without the filter (an admin) all three are visible.
    assert!(read_track(&store, "3", "adm", None).is_ok());

    let limited = get_all(&store, "lim", Some(&filter)).expect("get_all");
    assert_eq!(
        limited
            .items
            .iter()
            .map(|t| t.media_file_id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2"],
        "the library filter drops m3 (library 2)"
    );
    assert_eq!(limited.artwork_error, None);

    // A user granted no library gets an EMPTY Vec — dbx's `All` makes a non-nil
    // empty slice, so Go's queryAll ErrNoRows arm is dead on that path.
    seed_user(&store, "none", false);
    let empty = get_all(&store, "none", Some(&lib_filter("none"))).expect("empty get_all");
    assert!(empty.items.is_empty());
    assert_eq!(get_all(&store, "adm", None).expect("admin").items.len(), 3);
}

/// The count statements keep Go's join shapes: `Count` LEFT-joins `media_file`
/// (`:90-96`) and so still counts a track whose media file is gone, while
/// `CountAll` INNER-joins it (`:82-88`). The port renders INNER for both
/// (`crate::sqlrepo` has no LEFT-JOIN form), which is recorded as a residual.
#[test]
fn count_join_shapes_and_read_over_a_deleted_media_file() {
    let store = fixture();
    seed_mediafile(&store, 1, "ghost", false);
    seed_track(&store, "pl-1", 9, "ghost");
    insert(&store, "PRAGMA foreign_keys = OFF", &[]);
    insert(&store, "DELETE FROM media_file WHERE id = 'ghost'", &[]);

    let count = scalar(
        &store,
        &playlist_track_repo::count_query("pl-1", None)
            .count(playlist_track_repo::TABLE)
            .to_sql()
            .expect("count materializes"),
    );
    let count_all = scalar(
        &store,
        &playlist_track_repo::count_all_query("pl-1", None)
            .count(playlist_track_repo::TABLE)
            .to_sql()
            .expect("count materializes"),
    );
    // The port's INNER form: the ghost drops out of BOTH (Go would keep it in
    // Count). This is the documented residual, pinned so a future change cannot
    // silently drift the two statements further apart.
    assert_eq!(count, 3, "port: INNER JOIN drops the ghost from Count too");
    assert_eq!(count_all, 3);

    // Read hides the ghost behind its INNER JOIN ⇒ NotFound, NOT a scan error —
    // unlike scrobble Next, whose LEFT JOIN keeps the row and fails the scan.
    let ghost = read_track(&store, "9", "adm", None);
    assert!(matches!(ghost, Err(PlaylistTrackError::NotFound)));

    // get_all likewise drops it (INNER JOIN too).
    assert_eq!(
        get_all(&store, "adm", None).expect("get_all").items.len(),
        3
    );
}

// ---------------------------------------------------------------------------
// 5. scrobble Next: ordering, empty queue, deleted media file
// ---------------------------------------------------------------------------

/// `ORDER BY play_time, s.rowid LIMIT 1` (`:75`): earliest play_time first, equal
/// play_time cells broken by rowid ASC (FIFO by insert order), and `Dequeue`
/// surfacing the next head.
#[test]
fn next_orders_by_play_time_then_rowid_and_dequeues_the_head() {
    let store = fixture();
    seed_user(&store, "u2", false);
    enqueue(
        &store,
        "tieA",
        "lim",
        "m1",
        "2024-05-05 00:00:00+00:00",
        "svc",
    );
    enqueue(
        &store,
        "tieB",
        "lim",
        "m2",
        "2024-05-05 00:00:00+00:00",
        "svc",
    );
    enqueue(
        &store,
        "first",
        "lim",
        "m1",
        "2024-04-04 00:00:00+00:00",
        "svc",
    );
    enqueue(
        &store,
        "other-svc",
        "lim",
        "m2",
        "2024-03-03 00:00:00+00:00",
        "other",
    );
    enqueue(
        &store,
        "u2-row",
        "u2",
        "m1",
        "2024-02-02 00:00:00+00:00",
        "svc",
    );

    let mut ids: Vec<String> = Vec::new();
    let mut drain = || {
        let entry = next_entry(&store, "svc", "lim")
            .expect("next")
            .expect("queue not empty");
        ids.push(entry.id.clone());
        let n = lease(&store, |conn| {
            ScrobbleBufferRepo::new(conn).dequeue(&entry.id)
        })
        .expect("dequeue");
        assert_eq!(n, 1, "Dequeue deletes exactly the entry Next returned");
        entry
    };
    let first = drain();
    let second = drain();
    let third = drain();

    assert_eq!(ids, ["first", "tieA", "tieB"], "play_time then rowid ASC");
    assert_eq!(first.id, "first");
    assert_eq!(first.media.id, "m1");
    assert_eq!(second.id, "tieA");
    assert_eq!(third.id, "tieB");
    assert_eq!(third.media_file_id, "m2");
    assert_eq!(third.media.id, "m2", "overwrite, not the entry id");
    assert_eq!(
        third.play_time,
        stamp("2024-05-05 00:00:00+00:00"),
        "the datetime cell decodes as Go's time.Time"
    );
    assert_eq!(third.enqueue_time, stamp(T1));

    // Service/user scoping: the two remaining rows belong to another service and
    // another user, and Length counts them all.
    assert_eq!(
        lease(&store, |conn| ScrobbleBufferRepo::new(conn).length()).expect("length"),
        2
    );
    assert!(
        matches!(next_entry(&store, "svc", "lim"), Ok(None)),
        "lim's svc queue is drained"
    );
    let other = next_entry(&store, "svc", "u2")
        .expect("next")
        .expect("u2 row");
    assert_eq!(other.id, "u2-row");
    assert_eq!(other.user_id, "u2");
    assert_eq!(other.play_time, stamp("2024-02-02 00:00:00+00:00"));
    // Service scoping, done as a real negative proof: `other-svc` has the LATEST
    // play_time, so asking for service 'other' must surface it — and asking for a
    // third service must surface nothing.
    let other = next_entry(&store, "other", "lim")
        .expect("next")
        .expect("other row");
    assert_eq!(other.id, "other-svc");
    assert_eq!(other.service, "other");
    assert!(matches!(next_entry(&store, "third", "lim"), Ok(None)));
}

/// `:78-80` — an empty queue is `(nil, nil)`, NOT `model.ErrNotFound`. The port
/// mirrors it as `Ok(None)`; `length` still reports the truth.
#[test]
fn empty_queue_is_not_an_error() {
    let store = fixture();
    let (entry, length) = lease(&store, |conn| {
        (
            ScrobbleBufferRepo::new(conn).next("svc", "lim"),
            ScrobbleBufferRepo::new(conn).length(),
        )
    });
    assert!(entry.is_ok(), "an empty queue must not error: {entry:?}");
    assert!(entry.expect("no error").is_none(), "empty queue ⇒ None");
    assert_eq!(length.expect("length"), 0);

    // Drain a non-empty queue and confirm the transition to Ok(None).
    enqueue(&store, "only", "lim", "m1", T0, "svc");
    assert!(next_entry(&store, "svc", "lim").expect("next").is_some());
    assert_eq!(
        lease(&store, |conn| ScrobbleBufferRepo::new(conn).dequeue("only")).expect("dequeue"),
        1
    );
    assert!(
        matches!(next_entry(&store, "svc", "lim"), Ok(None)),
        "after draining, Next is Ok(None)"
    );
    // Dequeue of an absent row is not an error either (Go's `delete`).
    assert_eq!(
        lease(&store, |conn| ScrobbleBufferRepo::new(conn).dequeue("only")).expect("dequeue"),
        0
    );
}

/// The contract's required seed (`hydrate-scan-contract.md:208-211`): a LEFT-JOIN
/// row whose `media_file` is gone. Go does NOT get `ErrNotFound` — the row exists,
/// every `m.*` cell is NULL, and `database/sql` aborts on the FIRST NULL cell,
/// `m.id`, which is index 0 of the projection:
/// `sql: Scan error on column index 0, name "id": converting NULL to string is
/// unsupported` (`database/sql/convert.go:535`). So `Next` surfaces an error and
/// never treats the row as an empty queue.
#[test]
fn deleted_media_file_fails_the_scan_instead_of_being_an_empty_queue() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Main");
    seed_user(&store, "lim", false);
    grant_library(&store, "lim", 1);
    seed_album(&store, 1, "al-1");
    seed_mediafile(&store, 1, "m1", true);
    enqueue(&store, "ghosty", "lim", "m1", T0, "svc");
    insert(&store, "PRAGMA foreign_keys = OFF", &[]);
    insert(&store, "DELETE FROM media_file WHERE id = 'm1'", &[]);

    let result = next_entry(&store, "svc", "lim");
    let err = match result {
        Ok(None) => panic!("a ghost row must NOT look like an empty queue"),
        Ok(Some(entry)) => panic!("a ghost row must not hydrate: {entry:?}"),
        Err(ScrobbleBufferError::Sqlite(err)) => err,
    };
    let text = err.to_string();
    assert!(
        text.contains("converting NULL to string is unsupported"),
        "Go's own sentence is required, got {text}"
    );
    assert!(
        text.contains("column `id`"),
        "the failing cell is `id`, naming the FIRST one (`m.id`): {text}"
    );
    assert!(
        text.contains("index 0"),
        "index 0 is `m.id`, the first column of `m.*`: {text}"
    );
    assert_eq!(
        lease(&store, |conn| ScrobbleBufferRepo::new(conn).length()).expect("length"),
        1,
        "the ghost entry is still queued, so the scan error is not NotFound"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The same LEFT-JOIN miss through the playlist-track projection. Go's own `Read`
/// hides it behind an INNER JOIN (tested above), so the NULL behavior is pinned by
/// a hand-written LEFT-JOIN projection — what the contract's seed asks for.
#[test]
fn playlist_track_left_join_over_a_deleted_media_file_fails_the_scan() {
    let store = fixture();
    seed_mediafile(&store, 1, "ghost", false);
    seed_track(&store, "pl-1", 9, "ghost");
    insert(&store, "PRAGMA foreign_keys = OFF", &[]);
    insert(&store, "DELETE FROM media_file WHERE id = 'ghost'", &[]);

    let result = lease(&store, |conn| {
        execution::query_optional(
            conn,
            &(
                "SELECT f.*, playlist_tracks.* FROM playlist_tracks \
                 LEFT JOIN media_file f ON f.id = media_file_id \
                 WHERE playlist_id = 'pl-1' AND playlist_tracks.id = 9"
                    .to_string(),
                Vec::new(),
            ),
            hydrate::row_to_playlist_track,
        )
    });
    let err = match result {
        Err(err) => err,
        Ok(None) => panic!("the LEFT JOIN keeps the row"),
        Ok(Some(row)) => panic!("a NULL media file must not hydrate: {row:?}"),
    };
    let text = err.to_string();
    assert!(
        text.contains("converting NULL to string is unsupported"),
        "Go's NULL-into-string sentence, got {text}"
    );
    assert!(
        text.contains("column `id`") && text.contains("index 0"),
        "{text}"
    );
}

/// Nullable media-file columns must NOT abort an ordinary read — Go only fails on
/// NULL where the destination is a non-pointer `string`. Executed, the seeded row
/// stores NULL in `bit_depth`/`bpm`/`rg_album_*` and reads fine through a track
/// row, which is why the port has no whole-projection NULL pre-walk.
#[test]
fn nullable_columns_read_and_only_string_targets_fail_on_null() {
    let store = fixture();
    let track = read_track(&store, "1", "adm", None).expect("read");
    assert_eq!(track.media.bit_depth, None, "NULL *int is nil");
    assert_eq!(track.media.bpm, None);
    assert_eq!(track.media.rg_album_gain, None, "NULL *float64 is nil");
    assert_eq!(track.media.rg_album_peak, None);
    assert_eq!(track.media.rg_track_gain, Some(-7.0));
    assert_eq!(track.media.rg_track_peak, Some(0.5));
    assert_eq!(track.media.created_at, Some(stamp(T0)));

    // Next over a row with NULL nullable columns succeeds.
    enqueue(&store, "sb-n", "lim", "m1", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("entry");
    assert_eq!(entry.media.bit_depth, None);
    assert_eq!(entry.media.rg_track_peak, Some(0.5));
}

// ---------------------------------------------------------------------------
// 6. mediafile post-scan reuse reached through a track / buffer row
// ---------------------------------------------------------------------------

/// Participants, tags→genres, the four RG columns and lyrics all come from the
/// delegated `dbMediaFile.PostScan`; a track row reaches them exactly as
/// `MediaFileRepo::get` does.
#[test]
fn mediafile_post_scan_is_reached_through_a_track_row() {
    let store = fixture();
    let track = read_track(&store, "1", "adm", None).expect("read");

    assert_eq!(track.media.participants.len(), 1);
    let artist = track.media.participants[&Role::from("artist")].clone();
    assert_eq!(artist[0].id, "ar-1");
    assert_eq!(artist[0].name, "Song Artist");
    assert_eq!(
        track.media.participant_names,
        vec![album_repo::NamedParticipant {
            id: "ar-1".to_string(),
            name: "Song Artist".to_string(),
            sort_name: String::new(),
            sub_role: String::new(),
        }]
    );

    // tags decoded; genre/genres DERIVED (`model/tag.go:138-148` — no dedup).
    assert_eq!(track.media.tags.len(), 1);
    assert_eq!(
        track.media.genre, "Rock",
        "genres[0].Name overwrote the column"
    );
    let names: Vec<&str> = track.media.genres.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Rock", "Jazz", "rock"]);
    assert_eq!(
        track.media.genres[2].id, track.media.genres[0].id,
        "case-insensitive tag id"
    );

    assert_eq!(track.media.lyrics, "[]");
    assert_eq!(track.media.rg_track_gain, Some(-7.0));
    assert_eq!(track.media.rg_track_peak, Some(0.5));
    assert_eq!(track.media.rg_album_gain, None);
    assert_eq!(track.media.rg_album_peak, None);

    // The same hydration through a scrobble entry.
    enqueue(&store, "sb-2", "lim", "m1", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("entry");
    assert_eq!(entry.media.genres, track.media.genres);
    assert_eq!(entry.media.participants, track.media.participants);
    assert_eq!(entry.media.participant_names, track.media.participant_names);
    assert_eq!(entry.media.rg_track_gain, track.media.rg_track_gain);
    assert_eq!(entry.media.lyrics, track.media.lyrics);
    assert_eq!(entry.media.tags, track.media.tags);
    assert_eq!(
        entry.media.album_id, "al-1",
        "the joined row's own columns hydrate"
    );
}
/// The `participants`/`tags` shims are read through `join_text`, so a projection
/// WITHOUT them yields Go's zero value (`""`) instead of a missing-column error.
/// That leniency mirrors dbx, which binds a ref only for columns the RESULT
/// actually has (`dbx@v1.12.0/rows.go:76-87`): an absent column leaves the struct
/// field at its zero value, and Go's `PostScan` then decodes `""`.
///
/// The leniency does NOT skip the decode. Go's `unmarshalParticipants` is UNGUARDED
/// (`mediafile_repository.go:55-58`) and `json.Unmarshal("")` fails with
/// `unexpected end of JSON input` (executed in a Go oracle), so the zero value still
/// errors, carrying Go's media-file prefix. No live Go read can reach this shape: every
/// media-file projection is `m.*`/`media_file.*`, which always carries both columns.
/// Pinned so a future light projection cannot silently turn a missing shim column into
/// empty participants.
#[test]
fn a_projection_without_the_participants_shim_still_fails_the_unguarded_decode() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Main");
    seed_user(&store, "adm", true);
    seed_album(&store, 1, "al-1");
    seed_mediafile(&store, 1, "m1", true);

    // Every media_file column EXCEPT the two shims, derived from the fresh DDL rather
    // than hand-listed. `media_file.*` always carries both, which is exactly why no
    // real read produces this shape.
    let projection = table_columns(&store, "media_file")
        .into_iter()
        .filter(|name| name != "participants" && name != "tags")
        .collect::<Vec<String>>()
        .join(", ");
    assert!(
        projection.contains("album"),
        "the DDL-derived projection keeps the required columns"
    );
    let sql = (format!("SELECT {projection} FROM media_file"), Vec::new());
    // The row scan succeeds (that is the leniency); only the hook fails, so the
    // hydrate error is collected per row instead of aborting the statement.
    let (ids, errors) = on(&store, |conn| {
        let mut errors: Vec<String> = Vec::new();
        let ids = execution::query_all(conn, &sql, |row| match hydrate::row_to_mediafile(row) {
            Ok(raw) => Ok(match hydrate::mediafile_post_scan(raw) {
                Ok(record) => Some(record.id),
                Err(err) => {
                    errors.push(err.to_string());
                    None
                }
            }),
            Err(err) => Err(err),
        })?;
        Ok((ids, errors))
    })
    .expect("the projection itself is valid SQL against the fresh schema");
    assert!(
        ids.iter().all(|id| id.is_none()),
        "the absent shim column must not read as empty participants: {ids:?}"
    );
    let err = errors.first().expect("the hook reported a failure");
    assert!(
        err.contains("parsing media_file from db"),
        "Go's media-file prefix on the zero value, got {err}"
    );
    assert!(
        err.contains("parsing participants"),
        "the inner sentence: {err}"
    );

    // The same fixture WITH the shim columns hydrates, so the failure above is the
    // missing column and not the fixture itself.
    let ok = on(&store, |conn| {
        execution::query_all(
            conn,
            &(
                "SELECT media_file.* FROM media_file".to_string(),
                Vec::new(),
            ),
            |row| match hydrate::row_to_mediafile(row) {
                Ok(raw) => hydrate::mediafile_post_scan(raw)
                    .map(|record| record.participants.len())
                    .map_err(|err| {
                        rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(1),
                            Some(err.to_string()),
                        )
                    }),
                Err(err) => Err(err),
            },
        )
    })
    .expect("media_file.* carries the shim columns");
    assert_eq!(ok, vec![1], "the fixture's own participants decode");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A bad `participants` cell fails with Go's MEDIA-FILE prefix — the buffer and
/// track hooks add none of their own, so neither `playlist_track` nor
/// `scrobble_buffer` appears in the message.
#[test]
fn decode_failure_keeps_the_media_file_prefix_for_both_repos() {
    let store = fixture();
    insert(
        &store,
        "UPDATE media_file SET participants = 'not-json' WHERE id = 'm1'",
        &[],
    );

    let track_err = read_track(&store, "1", "adm", None).expect_err("participants decode fails");
    let text = track_err.to_string();
    assert!(
        text.contains("parsing media_file from db"),
        "Go's media-file prefix, got {text}"
    );
    assert!(
        !text.contains("playlist_track"),
        "the track hook adds no prefix: {text}"
    );
    assert!(!text.contains("scrobble"));

    // `tags` is guarded by a NON-EMPTY check but NOT by an error guard
    // (`mediafile_repository.go:59-64`: a decode failure is returned), so a bad
    // non-empty tags cell fails the read too — with the same media-file prefix. An
    // EMPTY tags cell is skipped entirely, leaving the column genre and no genres.
    insert(&store, "UPDATE media_file SET participants = '{\"artist\":[{\"id\":\"ar-1\",\"name\":\"Song Artist\"}]}' WHERE id = 'm1'", &[]);
    insert(
        &store,
        "UPDATE media_file SET tags = 'not-json' WHERE id = 'm1'",
        &[],
    );
    let tags_err = read_track(&store, "1", "adm", None).expect_err("tags decode fails");
    assert!(
        tags_err.to_string().contains("parsing media_file from db"),
        "{tags_err}"
    );
    assert!(
        tags_err.to_string().contains("tags"),
        "names the shim column: {tags_err}"
    );

    insert(
        &store,
        "UPDATE media_file SET tags = '' WHERE id = 'm1'",
        &[],
    );
    let empty_tags = read_track(&store, "1", "adm", None).expect("empty tags shim is skipped");
    assert_eq!(
        empty_tags.media.genre, "ColumnGenre",
        "the column value survives"
    );
    assert!(
        empty_tags.media.genres.is_empty(),
        "no ToGenres assignment ran"
    );

    // The same failure through Next.
    insert(
        &store,
        "UPDATE media_file SET participants = 'not-json' WHERE id = 'm2'",
        &[],
    );
    enqueue(&store, "sb-3", "lim", "m2", T0, "svc");
    let entry_err =
        next_entry(&store, "svc", "lim").expect_err("a bad participants cell must fail");
    assert!(
        entry_err.to_string().contains("parsing media_file from db"),
        "same prefix through the buffer hook: {entry_err}"
    );
    assert!(!entry_err.to_string().contains("scrobble"));
}

// ---------------------------------------------------------------------------
// 7. artwork hydration for the media-file kind, reached through tracks
// ---------------------------------------------------------------------------

/// `hydratePlaylistTrackArtwork` (`artwork_hydration.go:169-181`) builds a
/// `[]MediaFile` from the tracks and calls `hydrateMediaFileArtwork` — there is no
/// playlist artwork kind. Executed, the media-file decision table lands on
/// `track.media.image` / `track.media.album_image`.
#[test]
fn get_all_hydrates_media_file_artwork_through_the_tracks() {
    let store = fixture();
    seed_state(&store, "al", "al-1", "ALH");
    seed_image(&store, "ALH", 800, 600, "blur-al", "thumb-al");
    seed_state(&store, "mf", "m1", "MF1H");
    seed_image(&store, "MF1H", 400, 300, "blur-mf", "thumb-mf");

    let outcome = get_all(&store, "lim", Some(&lib_filter("lim"))).expect("get_all");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), 2);

    let m1 = &outcome.items[0].media;
    assert_eq!(m1.id, "m1", "the id overwrite holds after hydration");
    assert_eq!(
        m1.image.image_hash, "MF1H",
        "eligible + resolved ⇒ own image"
    );
    assert!(!m1.image.image_absent);
    assert_eq!(m1.image.blur_hash, "blur-mf");
    assert_eq!(m1.image.image_width, 400);
    assert_eq!(m1.image.dominant_color, "#aabbcc");
    assert_eq!(
        m1.album_image.image_hash, "ALH",
        "album image is always applied"
    );
    assert_eq!(m1.album_image.blur_hash, "blur-al");
    assert_eq!(m1.album_image.image_width, 800);

    let m2 = &outcome.items[1].media;
    assert_eq!(
        m2.image.image_hash, "ALH",
        "not eligible (has_cover_art false) ⇒ inherits the album hash"
    );
    assert!(!m2.image.image_absent);
    assert_eq!(m2.album_image.image_hash, "ALH");

    // `Read` does NOT hydrate artwork — the only track read that skips
    // hydratePlaylistTrackArtwork, which lives in loadTracks (:406).
    let one = read_track(&store, "1", "lim", Some(&lib_filter("lim"))).expect("read");
    assert_eq!(
        one.media.image,
        ItemImage::default(),
        "Read leaves image zero"
    );
    assert_eq!(one.media.album_image, ItemImage::default());
}

/// The album-absent branch (`artwork_hydration.go:126-134`) and the unresolved
/// branch: a known-absent album plus an ineligible track sets `image_absent`; a
/// track with no album artwork row at all stays zero, never falsely absent.
#[test]
fn artwork_absent_and_unresolved_states_reached_through_tracks() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Main");
    seed_user(&store, "adm", true);
    seed_album(&store, 1, "al-1");
    seed_mediafile(&store, 1, "m-a", false);
    seed_mediafile(&store, 1, "m-b", false);
    seed_playlist(&store, "pl-1", "adm");
    seed_track(&store, "pl-1", 1, "m-a");
    seed_track(&store, "pl-1", 2, "m-b");
    seed_state(&store, "al", "al-1", "");

    let outcome = get_all(&store, "adm", None).expect("get_all");
    let absent = &outcome.items[0].media;
    assert!(
        absent.image.image_absent,
        "ineligible + album known-absent ⇒ image_absent"
    );
    assert_eq!(
        absent.image.image_hash, "",
        "a known-absent album applies an empty image"
    );
    assert!(absent.album_image.image_absent);

    // Move m-b to an album with NO artwork state row: unresolved, so neither hash
    // nor absent flag is set.
    seed_album(&store, 1, "al-2");
    insert(
        &store,
        "UPDATE media_file SET album_id = 'al-2' WHERE id = 'm-b'",
        &[],
    );
    let outcome = get_all(&store, "adm", None).expect("get_all again");
    let unresolved = outcome
        .items
        .iter()
        .find(|t| t.media_file_id == "m-b")
        .expect("m-b")
        .media
        .clone();
    assert_eq!(
        unresolved.image,
        ItemImage::default(),
        "no album artwork row ⇒ unresolved, never falsely absent"
    );
    assert_eq!(unresolved.album_image, ItemImage::default());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The wrapper is the media-file one, and Go's early return for an empty slice
/// queries nothing (`artwork_hydration.go:170-172`).
#[test]
fn artwork_wrapper_is_reused_and_empty_tracks_query_nothing() {
    let store = fixture();
    let outcome = get_all(&store, "lim", Some(&lib_filter("lim"))).expect("no artwork rows");
    assert_eq!(
        outcome.artwork_error, None,
        "missing artwork rows fail nothing"
    );

    let mut empty: Vec<MediaFileRecord> = Vec::new();
    let err = lease(&store, |conn| {
        artwork_hydration::hydrate_media_file_artwork(conn, &mut empty, true)
    });
    assert_eq!(err, None, "empty slice ⇒ Go's early return, no query");
    assert!(empty.is_empty());

    // A track's media file resolves by its OVERWRITTEN id, which is the media
    // file's own id — the wrapper never touches ids.
    seed_state(&store, "mf", "m1", "OWN");
    seed_image(&store, "OWN", 120, 90, "b", "t");
    let mut media = vec![read_track(&store, "1", "adm", None).expect("read").media];
    lease(&store, |conn| {
        artwork_hydration::hydrate_media_file_artwork(conn, &mut media, true)
    });
    assert_eq!(media[0].image.image_hash, "OWN");
    assert_eq!(media[0].id, "m1", "the wrapper leaves the id alone");

    // With EnableMediaFileCoverArt off, an eligible track no longer resolves its
    // own art and inherits the album hash instead (`artwork_hydration.go:102,112`).
    seed_state(&store, "al", "al-1", "ALH");
    seed_image(&store, "ALH", 800, 600, "blur-al", "thumb-al");
    let mut off = vec![read_track(&store, "1", "adm", None).expect("read").media];
    lease(&store, |conn| {
        artwork_hydration::hydrate_media_file_artwork(conn, &mut off, false)
    });
    assert_eq!(
        off[0].image.image_hash, "ALH",
        "conf switch off ⇒ own art is not eligible"
    );
}

/// A statement failure in the artwork lookup is REPORTED, not returned as an
/// error (Go logs and returns an empty map so the page still renders), and leaves
/// images zero. Executed with the artwork table absent.
#[test]
fn artwork_lookup_failure_is_reported_not_returned() {
    let (store, path) = fresh_store();
    seed_library(&store, 1, "Main");
    seed_user(&store, "adm", true);
    seed_album(&store, 1, "al-1");
    seed_mediafile(&store, 1, "m1", true);
    seed_playlist(&store, "pl-1", "adm");
    seed_track(&store, "pl-1", 1, "m1");
    insert(&store, "DROP TABLE item_artwork", &[]);

    let outcome = get_all(&store, "adm", None).expect("tracks still come back");
    assert_eq!(outcome.items.len(), 1);
    assert_eq!(outcome.items[0].media.id, "m1");
    assert_eq!(outcome.items[0].media.image, ItemImage::default());
    let err = outcome.artwork_error.expect("the failure is reported");
    assert!(
        err.contains("item_artwork"),
        "the message names the missing table: {err}"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// 8. write-map invariance — equal to the media-file map, key for key
// ---------------------------------------------------------------------------

/// Both embedded repos write the media file's columns and nothing else: reuse the
/// executed write-map precedent instead of deriving new counts. The media-file map
/// is 66 keys (63 base + 3 FTS), per
/// `runtime_hydrate_album_mediafile.rs:1301`/`:2520`.
#[test]
fn playlist_track_and_scrobble_write_maps_equal_the_media_file_map() {
    let store = fixture();
    let track = read_track(&store, "1", "lim", Some(&lib_filter("lim"))).expect("read");
    enqueue(&store, "sb-9", "lim", "m1", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("entry");

    let base_map = mediafile_repo::mediafile_row(&track.media, false);
    let media_keys: Vec<&str> = base_map.map().keys().map(String::as_str).collect();
    assert_eq!(media_keys.len(), 66, "63 base + 3 FTS keys");

    let track_columns = table_columns(&store, "playlist_tracks");
    let scrobble_columns = table_columns(&store, "scrobble_buffer");
    assert_eq!(track_columns, vec!["id", "playlist_id", "media_file_id"]);
    assert_eq!(scrobble_columns.len(), 6);

    for record in [&track.media, &entry.media] {
        let map = mediafile_repo::mediafile_row(record, false);
        let keys: Vec<&str> = map.map().keys().map(String::as_str).collect();
        assert_eq!(
            keys, media_keys,
            "key-for-key identical to the media-file map"
        );
        for forbidden in [
            "id",
            "playlist_id",
            "media_file_id",
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
                "{forbidden} reached the embedded media-file map: {keys:?}"
            );
        }
        for column in track_columns.iter().chain(scrobble_columns.iter()) {
            assert!(
                !keys.contains(&column.as_str()),
                "{column} (a playlist_tracks/scrobble_buffer column) reached the map: {keys:?}"
            );
        }
    }

    // Hydrated state cannot change a write leg: the annotation/library/image/genres
    // fields are hydration-only, so the INSERT renders identically with them zeroed.
    let zeroed = MediaFileRecord {
        image: ItemImage::default(),
        album_image: ItemImage::default(),
        annotation: nd_model::annotation::Annotation::default(),
        genres: Vec::new(),
        library_path: String::new(),
        library_name: String::new(),
        bookmark_position: 0,
        ..track.media.clone()
    };
    let hydrated =
        mediafile_repo::put_sql("m1", &track.media, stamp(T1), false, &[]).expect("renders");
    let plain = mediafile_repo::put_sql("m1", &zeroed, stamp(T1), false, &[]).expect("renders");
    assert_eq!(
        hydrated.insert, plain.insert,
        "hydration changes no write leg"
    );

    // A track whose position differs from its media id still writes the media file
    // under the media id: the position never becomes a map key.
    let renamed = MediaFileRecord {
        id: "9".to_string(),
        ..track.media.clone()
    };
    let renamed_map = mediafile_repo::mediafile_row(&renamed, false);
    let keys: Vec<&str> = renamed_map.map().keys().map(String::as_str).collect();
    assert_eq!(keys, media_keys, "the id is never a map key");
}

// ---------------------------------------------------------------------------
// 9. residuals: options on GetAll, and getParticipants absence
// ---------------------------------------------------------------------------

/// `GetAll` forwards `model.QueryOptions` into `newSelect`
/// (`playlist_track_repository.go:124`) — the asymmetry vs
/// `artist_repository.go:258`/`:272`, where they are NOT forwarded. The port's
/// `tracks_query` takes no options, so none of that reaches SQL. Recorded, not
/// emulated; pinned so the gap stays visible.
#[test]
fn tracks_query_carries_no_pagination_sort_or_filter_legs() {
    let store = fixture();
    let (sql, args) = playlist_repo::tracks_query("pl-1", "lim", None);
    assert!(
        !sql.contains("ORDER BY") && !sql.contains("LIMIT") && !sql.contains("OFFSET"),
        "no options legs exist in the port: {sql}"
    );
    assert!(sql.contains("library.path as library_path"));
    assert!(sql.contains("library.name as library_name"));
    assert_eq!(
        args,
        vec![Value::Text("pl-1".to_string())],
        "only the playlist id binds"
    );

    let all = get_all(&store, "adm", None).expect("get_all");
    assert_eq!(all.items.len(), 3, "no Max/Offset narrowing");
    // Rows arrive in SQLite's own order (no ORDER BY), i.e. the table's order.
    assert_eq!(
        all.items.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
}

/// Go's `ScrobbleEntry` has NO participants field of its own —
/// `model/scrobble_buffer.go:8-16` embeds `MediaFile`, so `ScrobbleEntry.Participants`
/// IS `MediaFile.Participants` (executed against the real repository:
/// `&e.Participants == &e.MediaFile.Participants` is `true` for every entry `Next`
/// returns). `next` therefore exposes that alias's JSON-decoded value, and the two
/// fields cannot disagree. Go's follow-up `getParticipants` re-read
/// (`scrobble_buffer_repository.go:84`, `sql_participations.go:127-146`) is NOT
/// applied here, so the entry reports the pre-re-read values: names come from the
/// `participants` column, and `sort_name`/`order_artist_name`/`mbz_artist_id` stay
/// empty because the column never stores them
/// (`sql_participations.go:13-17`). Executed Go contrast, with an `artist` row whose
/// name differs from the JSON and no `library_artist` junction row: Go's `Next`
/// replaces the participant whose artist is visible through a library and keeps the
/// JSON values for the others — this port does neither, and says so.
#[test]
fn scrobble_entry_participants_are_the_media_file_alias_not_a_fabricated_map() {
    let store = fixture();
    enqueue(&store, "sb-p", "lim", "m1", T0, "svc");
    let entry = next_entry(&store, "svc", "lim")
        .expect("next")
        .expect("entry");
    assert_eq!(
        entry.participants, entry.media.participants,
        "Go's alias: the entry's field IS the media file's, so one may not be populated while the other is empty"
    );
    assert_eq!(
        entry.participants.len(),
        1,
        "the delegated PostScan's decoded participants are what the entry carries"
    );
    let artist = &entry.participants[&Role::from("artist")][0];
    assert_eq!(artist.id, "ar-1");
    assert_eq!(
        artist.name, "Song Artist",
        "the JSON name, not a re-read artist"
    );
    assert_eq!(entry.media.participant_names.len(), 1);
}
