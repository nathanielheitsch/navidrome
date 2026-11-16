//! S-D3c goldens — artwork hydration (`persistence/artwork_hydration.go`)
//! EXECUTED against a real SQLite DB: fresh schema via `nd_dbschema`, one
//! isolated temp file per test (harness shape copied from
//! `tests/runtime_hydrate_album_mediafile.rs:49-70`).
//!
//! Go truth under test: `hydrateItemImages` (`:58-68`), `applyItemImage`
//! (`:71-75`), `hydrateItems` (`:79-90`) and `hydrateMediaFileArtwork`
//! (`:94-135`), plus `GetInfoForItems`' chunked join
//! (`persistence/artwork_repository.go:159-182`) and `ItemImage`/
//! `ItemArtworkInfo` (`model/artwork.go:29-40`, `:70-93`).
//!
//! The claims this slice pins, each executed:
//! (1) the three album states — found / known-absent (`hash = ''`) / unresolved
//!     (no state row) — and that unresolved is NOT absent;
//! (2) `album_image` populated on EVERY media_file branch, including the
//!     own-art-wins `continue`;
//! (3) the whole branch matrix (`artwork_hydration.go:112-133`);
//! (4) the disc-art and eligible-but-unresolved arms that keep a track
//!     requestable;
//! (5) the joined columns (`COALESCE` arms) surviving both the own-art and the
//!     inherited branch;
//! (6) `item_kind` being the kind PREFIX — seeding a kind NAME finds nothing;
//! (7) the 200-id chunk boundary;
//! (8) Go's swallowed lookup failure: the read still succeeds and every item
//!     stays zero-valued.

use nd_db::album_repo::AlbumRecord;
use nd_db::album_repo::AlbumRepo;
use nd_db::artwork_hydration::{self, ItemImage};
use nd_db::mediafile_repo::{MediaFileRecord, MediaFileRepo};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_participations::{Participant, Participants, Role};
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
const LIB_PATH: &str = "/music";
/// Go's default for `EnableMediaFileCoverArt` (`conf/configuration.go:1003`).
const COVER_ART_ON: bool = true;

/// One isolated on-disk DB per invocation, freshly migrated and pooled.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-artwork-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

fn stamp(text: &str) -> chrono::DateTime<chrono::Utc> {
    nd_db::time::parse(text).unwrap_or_else(|| panic!("{text} parses"))
}

/// Run one closure on a leased connection; a lease failure panics.
fn on<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Execute a raw statement with typed args.
fn insert(store: &SQLStore, sql: &str, args: &[rusqlite::types::Value]) {
    store
        .with_connection_sync(|conn| {
            conn.execute(sql, rusqlite::params_from_iter(args.iter()))
                .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"));
}

fn v(text: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(text.to_string())
}
fn i(value: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(value)
}

/// `library.id` is the INTEGER PK; `album.library_id` references it.
fn seed_library(store: &SQLStore) -> i64 {
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[i(1), v("Main"), v(LIB_PATH), v("")],
    );
    1
}

/// An album row, written through the record-driven write path so the shim
/// columns hold exactly what `PostMapArgs` marshals.
fn seed_album(store: &SQLStore, lib_id: i64, id: &str) -> AlbumRecord {
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
        album_artist_id: String::new(),
        min_year: 1969,
        max_year: 1970,
        date: "1969-09-26".to_string(),
        min_original_year: 0,
        max_original_year: 0,
        original_date: String::new(),
        release_date: "1970-02-01".to_string(),
        compilation: false,
        comment: "c".to_string(),
        song_count: 17,
        duration: 1234.5,
        size: 87_654_321,
        discs: None,
        sort_album_name: String::new(),
        sort_album_artist_name: String::new(),
        order_album_name: String::new(),
        order_album_artist_name: String::new(),
        catalog_num: String::new(),
        mbz_album_id: String::new(),
        mbz_album_artist_id: String::new(),
        mbz_album_type: String::new(),
        mbz_album_comment: String::new(),
        mbz_release_group_id: String::new(),
        folder_ids: None,
        explicit_status: String::new(),
        rg_album_gain: None,
        rg_album_peak: None,
        description: String::new(),
        small_image_url: String::new(),
        medium_image_url: String::new(),
        large_image_url: String::new(),
        external_url: String::new(),
        external_info_updated_at: None,
        genre: String::new(),
        tags: nd_db::sql_tags::DbTags::new(),
        participants,
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
    let put = nd_db::album_repo::put_sql(id, &record, stamp(T1), &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind_value).collect();
    insert(store, &put.insert.0, &args);
    record
}

/// `Value` → rusqlite's `Value`, the same conversion the sibling harness uses.
fn bind_value(value: &nd_db::sqlrepo::Value) -> rusqlite::types::Value {
    use nd_db::sqlrepo::Value;
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(n) => rusqlite::types::Value::Integer(*n),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

/// A track written through the record-driven write path. `has_cover_art` and
/// `disc_number` are the two inputs the branch matrix reads
/// (`artwork_hydration.go:99,111,120,128`).
fn seed_track(
    store: &SQLStore,
    lib_id: i64,
    id: &str,
    album_id: &str,
    has_cover_art: bool,
    disc_number: i32,
) -> MediaFileRecord {
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
        album_artist_id: String::new(),
        album_id: album_id.to_string(),
        artist: "Song Artist".to_string(),
        artist_id: "ar-1".to_string(),
        birth_time: stamp(T1),
        bit_depth: None,
        bit_rate: 320,
        bpm: None,
        catalog_num: String::new(),
        channels: 2,
        codec: "flac".to_string(),
        comment: "c".to_string(),
        compilation: false,
        created_at: Some(stamp(T0)),
        date: "1969-09-26".to_string(),
        disc_number,
        disc_subtitle: String::new(),
        duration: 24.5,
        explicit_status: String::new(),
        folder_id: "fold-1".to_string(),
        genre: String::new(),
        has_cover_art,
        library_id: lib_id,
        lyrics: "[]".to_string(),
        mbz_recording_id: String::new(),
        mbz_release_track_id: String::new(),
        mbz_album_id: String::new(),
        mbz_release_group_id: String::new(),
        mbz_artist_id: String::new(),
        mbz_album_artist_id: String::new(),
        mbz_album_type: String::new(),
        mbz_album_comment: String::new(),
        missing: false,
        order_album_artist_name: String::new(),
        order_album_name: String::new(),
        order_artist_name: String::new(),
        order_title: String::new(),
        original_date: String::new(),
        original_year: 0,
        path: format!("{LIB_PATH}/{id}.flac"),
        pid: format!("pid-{id}"),
        probe_data: "{}".to_string(),
        release_date: String::new(),
        release_year: 0,
        rg_album_gain: None,
        rg_album_peak: None,
        rg_track_gain: None,
        rg_track_peak: None,
        sample_rate: 44100,
        size: 12_345_678,
        sort_album_artist_name: String::new(),
        sort_album_name: String::new(),
        sort_artist_name: String::new(),
        sort_title: String::new(),
        suffix: ".flac".to_string(),
        tags: nd_db::sql_tags::DbTags::new(),
        title: "Something".to_string(),
        track_number: 17,
        year: 1969,
        participants,
        participant_names: Vec::new(),
        updated_at: Some(stamp(T1)),
        library_path: String::new(),
        library_name: String::new(),
        annotation: nd_model::annotation::Annotation::default(),
        bookmark_position: 0,
        genres: Vec::new(),
        image: ItemImage::default(),
        album_image: ItemImage::default(),
    };
    let put = nd_db::mediafile_repo::put_sql(id, &record, stamp(T1), false, &[]).expect("renders");
    let args: Vec<rusqlite::types::Value> = put.insert.1.iter().map(bind_value).collect();
    insert(store, &put.insert.0, &args);
    record
}

/// An `item_artwork` state row at `image_type = 'primary'`, the only slot the
/// hydration query reads (`artwork_repository.go:172`). `hash = ""` is the
/// known-absent state (`model/artwork.go:49`).
fn seed_state(store: &SQLStore, kind: &str, id: &str, hash: &str) {
    insert(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, updated_at, \
         attempted_at) VALUES (?, ?, 'primary', ?, 'folder', ?, ?)",
        &[v(kind), v(id), v(hash), v(T1), rusqlite::types::Value::Null],
    );
}

/// An `artwork` image row, so the LEFT JOIN's `COALESCE` arms
/// (`artwork_repository.go:162-167`) have something to read.
fn seed_image(
    store: &SQLStore,
    hash: &str,
    width: i64,
    height: i64,
    blur: &str,
    thumb: &str,
    color: &str,
) {
    insert(
        store,
        "INSERT INTO artwork (hash, mime, width, height, size_bytes, blur_hash, thumb_hash, \
         dominant_color) VALUES (?, 'image/jpeg', ?, ?, 100, ?, ?, ?)",
        &[v(hash), i(width), i(height), v(blur), v(thumb), v(color)],
    );
}

/// The three album artwork states, executed through `AlbumRepo::get_all`.
#[test]
fn album_found_absent_and_unresolved_states() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-found");
    seed_album(&store, lib, "al-absent");
    seed_album(&store, lib, "al-unresolved");
    seed_state(&store, "al", "al-found", "h1");
    seed_state(&store, "al", "al-absent", "");
    // al-unresolved gets NO state row at all.
    let outcome = on(&store, |conn| {
        AlbumRepo::new(conn).get_all(None, false, None, vec!["album.id".to_string()], 0, 0)
    })
    .expect("read");
    assert_eq!(outcome.artwork_error, None, "the lookup succeeded");
    assert_eq!(outcome.items.len(), 3);
    let by_id = |id: &str| {
        outcome
            .items
            .iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("{id} in {outcome:?}"))
            .image
            .clone()
    };
    // Found: hash set, NOT absent (Image() derives image_absent from Absent()).
    assert_eq!(
        by_id("al-found"),
        ItemImage {
            image_hash: "h1".to_string(),
            ..Default::default()
        }
    );
    // Known-absent: a resolved entry with no image.
    assert_eq!(
        by_id("al-absent"),
        ItemImage {
            image_absent: true,
            ..Default::default()
        }
    );
    // Unresolved: applyItemImage's missing-entry branch leaves it entirely zero
    // (artwork_hydration.go:71-75) — image_absent stays FALSE.
    let unresolved = by_id("al-unresolved");
    assert_eq!(unresolved, ItemImage::default());
    assert!(
        !unresolved.image_absent,
        "unresolved must not read as absent"
    );
    std::fs::remove_file(path).ok();
}

/// Every branch sets `album_image` first (`artwork_hydration.go:110`), including
/// the own-art-wins `continue` at `:112-116`.
#[test]
fn album_image_is_populated_on_every_mediafile_branch() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");
    // Four tracks, hitting four different arms.
    seed_track(&store, lib, "mf-own", "al-1", true, 1); // own art wins (continue)
    seed_track(&store, lib, "mf-own-absent", "al-1", true, 0); // own absent → inherit
    seed_track(&store, lib, "mf-not-eligible", "al-1", false, 0); // not eligible → inherit
    seed_track(&store, lib, "mf-disc", "al-1", false, 2); // disc art → no inherit
    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(outcome.items.len(), 4);
    for track in &outcome.items {
        assert_eq!(
            track.album_image,
            ItemImage {
                image_hash: "albumhash".to_string(),
                ..Default::default()
            },
            "{}: album_image must be applied before the branch logic",
            track.id
        );
    }
    std::fs::remove_file(path).ok();
}

/// The branch matrix arms (`artwork_hydration.go:112-133`): own art wins, own
/// absent inherits, not-eligible inherits, and a MISSING album entry leaves the
/// track unresolved rather than absent.
#[test]
fn mediafile_branch_matrix_own_absent_inherit_and_unresolved() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");
    seed_state(&store, "mf", "mf-own", "ownhash");

    // al-none has NO state row: the album entry itself is absent.
    seed_album(&store, lib, "al-none");

    seed_track(&store, lib, "mf-own", "al-1", true, 1);
    seed_track(&store, lib, "mf-own-absent", "al-1", true, 0);
    // An eligible track whose own state says absent → own_wont_resolve → inherit.
    seed_state(&store, "mf", "mf-own-absent", "");
    seed_track(&store, lib, "mf-not-eligible", "al-1", false, 0);
    seed_track(&store, lib, "mf-album-unresolved", "al-none", false, 0);

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    let image_of = |id: &str| {
        outcome
            .items
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .image
            .clone()
    };

    // :112-116 — eligible, resolved, non-absent own art: own hash wins, and the
    // own branch's Image() carries no album hash.
    assert_eq!(
        image_of("mf-own"),
        ItemImage {
            image_hash: "ownhash".to_string(),
            ..Default::default()
        }
    );
    // :117 + :120-125 — own_wont_resolve (own resolved-but-absent) with disc 0
    // inherits the album's image.
    assert_eq!(
        image_of("mf-own-absent"),
        ItemImage {
            image_hash: "albumhash".to_string(),
            ..Default::default()
        }
    );
    // !eligible also sets own_wont_resolve, so it inherits too.
    assert_eq!(
        image_of("mf-not-eligible"),
        ItemImage {
            image_hash: "albumhash".to_string(),
            ..Default::default()
        }
    );
    // No album entry at all: neither the inherit arm nor the absent arm fires
    // (`:131` requires the entry to be PRESENT), so the track stays zero and
    // requestable.
    let unresolved = image_of("mf-album-unresolved");
    assert_eq!(unresolved, ItemImage::default());
    assert!(!unresolved.image_absent);
    std::fs::remove_file(path).ok();
}

/// A multi-disc track never inherits a FOUND album hash (`:120-125` inherits
/// only at `disc_number == 0`), and stays requestable when the album's state is
/// absent (`:128-130` continues before the absent arm).
#[test]
fn multi_disc_track_never_inherits_and_stays_requestable() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-found");
    seed_state(&store, "al", "al-found", "albumhash");
    seed_album(&store, lib, "al-absent");
    seed_state(&store, "al", "al-absent", "");

    // Not eligible (no embedded art) + disc 2: own_wont_resolve is true, the
    // album entry is present-and-found, but disc_number != 0 blocks the inherit.
    seed_track(&store, lib, "mf-disc-found", "al-found", false, 2);
    // Disc art with an ABSENT album state: the absent arm at :131 is unreachable
    // because :128-130 continues first, so the track stays requestable.
    seed_track(&store, lib, "mf-disc-absent", "al-absent", false, 2);
    // Control: the same inputs at disc 0 DO inherit / DO mark absent.
    seed_track(&store, lib, "mf-disc0-found", "al-found", false, 0);
    seed_track(&store, lib, "mf-disc0-absent", "al-absent", false, 0);

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    let image_of = |id: &str| {
        outcome
            .items
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .image
            .clone()
    };
    assert_eq!(image_of("mf-disc-found"), ItemImage::default());
    assert!(!image_of("mf-disc-found").image_absent);
    assert_eq!(image_of("mf-disc-absent"), ItemImage::default());
    assert!(
        !image_of("mf-disc-absent").image_absent,
        "a disc-art track keeps its own requestability even when the album is absent"
    );
    assert_eq!(
        image_of("mf-disc0-found").image_hash,
        "albumhash",
        "disc 0 inherits"
    );
    assert!(
        image_of("mf-disc0-absent").image_absent,
        "disc 0 with an absent album state is the definitive placeholder case"
    );
    assert_eq!(image_of("mf-disc0-absent").image_hash, "");
    std::fs::remove_file(path).ok();
}

/// An ELIGIBLE track with no state row yet stays optimistic (`own_wont_resolve`
/// is false at `:117`): it is not marked absent even when the album's state is
/// absent, and it keeps a bare (empty) own hash even when the album art is
/// found — Go's comment at `:118-119`: serving still extracts its own image.
#[test]
fn eligible_but_unresolved_stays_optimistic_and_bare() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-found");
    seed_state(&store, "al", "al-found", "albumhash");
    seed_album(&store, lib, "al-absent");
    seed_state(&store, "al", "al-absent", "");

    // Eligible (has_cover_art, flag on) and no `mf` state row for either.
    seed_track(&store, lib, "mf-open-found", "al-found", true, 0);
    seed_track(&store, lib, "mf-open-absent", "al-absent", true, 0);
    seed_track(&store, lib, "mf-open-unresolved-album", "al-none", true, 0);

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    let image_of = |id: &str| {
        outcome
            .items
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .image
            .clone()
    };
    for id in [
        "mf-open-found",
        "mf-open-absent",
        "mf-open-unresolved-album",
    ] {
        let image = image_of(id);
        assert_eq!(image, ItemImage::default(), "{id} keeps a bare hash");
        assert!(!image.image_absent, "{id} stays optimistic");
    }
    // The album's own state is still hydrated beside it (album_image), so the
    // Jellyfin album-art tag can be pixel-versioned without a second query.
    assert_eq!(
        outcome
            .items
            .iter()
            .find(|m| m.id == "mf-open-found")
            .expect("present")
            .album_image
            .image_hash,
        "albumhash"
    );
    std::fs::remove_file(path).ok();
}

/// `enable_media_file_cover_art = false` makes an eligible track fall back to
/// the album's info: it is no longer in the eligible id list at all
/// (`artwork_hydration.go:102`), so `ownResolved` is false and
/// `own_wont_resolve` is true (`:117`) → the inherit arm fires.
#[test]
fn cover_art_flag_off_falls_back_to_the_album() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");
    // Own state row exists, but with the flag off its kind is never queried.
    seed_state(&store, "mf", "mf-1", "ownhash");
    seed_track(&store, lib, "mf-1", "al-1", true, 0);

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, false)
    })
    .expect("read");
    assert_eq!(outcome.items.len(), 1);
    assert_eq!(
        outcome.items[0].image.image_hash, "albumhash",
        "flag off → the album image is used"
    );
    assert_eq!(outcome.items[0].album_image.image_hash, "albumhash");

    // Same DB, flag on: own art wins. The parameter is the only difference.
    let on = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(on.items[0].image.image_hash, "ownhash");
    std::fs::remove_file(path).ok();
}

/// The joined columns ride through BOTH branches, and a state row whose image
/// row is gone still reads (the `COALESCE` arms give `''`/`0`, never NULL).
#[test]
fn joined_columns_survive_own_and_inherited_branches() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    // Album: state row whose artwork row was pruned → COALESCE arms matter.
    seed_album(&store, lib, "al-orphan");
    seed_state(&store, "al", "al-orphan", "albumhash");
    // Album with a full artwork row.
    seed_album(&store, lib, "al-full");
    seed_state(&store, "al", "al-full", "fullhash");
    seed_image(
        &store,
        "fullhash",
        600,
        400,
        "albumblur",
        "albumthumb",
        "#112233",
    );
    // Own art with a full artwork row.
    seed_state(&store, "mf", "mf-own", "ownhash");
    seed_image(
        &store, "ownhash", 300, 200, "ownblur", "ownthumb", "#445566",
    );
    // Own art whose image row is gone → own image keeps the hash, zero metadata.
    seed_state(&store, "mf", "mf-orphan-own", "ownhash-orphan");

    seed_track(&store, lib, "mf-own", "al-full", true, 0);
    // Not eligible, album art found → inherit branch carries the joined columns.
    seed_track(&store, lib, "mf-inherit", "al-full", false, 0);
    // Own art wins while the album's image row is missing.
    seed_track(&store, lib, "mf-orphan-own", "al-orphan", true, 1);

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    let track = |id: &str| {
        outcome
            .items
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
    };

    assert_eq!(
        track("mf-own").image,
        ItemImage {
            image_hash: "ownhash".to_string(),
            blur_hash: "ownblur".to_string(),
            thumb_hash: "ownthumb".to_string(),
            dominant_color: "#445566".to_string(),
            image_width: 300,
            image_height: 200,
            ..Default::default()
        },
        "own-art branch carries the join"
    );
    assert_eq!(
        track("mf-inherit").image,
        ItemImage {
            image_hash: "fullhash".to_string(),
            blur_hash: "albumblur".to_string(),
            thumb_hash: "albumthumb".to_string(),
            dominant_color: "#112233".to_string(),
            image_width: 600,
            image_height: 400,
            ..Default::default()
        },
        "inherited branch carries the join"
    );
    // album_image on the own-art branch comes from the FULL album row.
    assert_eq!(track("mf-own").album_image.image_width, 600);
    assert_eq!(track("mf-own").album_image.dominant_color, "#112233");
    // COALESCE arms: a state row with no artwork row still resolves, with the
    // hash and zero metadata.
    assert_eq!(
        track("mf-orphan-own").image,
        ItemImage {
            image_hash: "ownhash-orphan".to_string(),
            ..Default::default()
        }
    );
    assert_eq!(
        track("mf-orphan-own").album_image,
        ItemImage {
            image_hash: "albumhash".to_string(),
            ..Default::default()
        },
        "COALESCE leaves '' and 0, not NULL"
    );
    std::fs::remove_file(path).ok();
}

/// `item_kind` holds Go's `Kind.Prefix()` (`artwork_repository.go:96`,
/// `model/artwork_id.go:22`), so hydration only ever queries by prefix and rows
/// written with a kind NAME are invisible.
#[test]
fn item_kind_is_the_prefix_and_a_kind_name_finds_nothing() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(
        &store,
        nd_id::ARTWORK_KIND_ALBUM_PREFIX,
        "al-1",
        "al-prefix",
    );
    seed_track(&store, lib, "mf-1", "al-1", true, 0);
    seed_state(
        &store,
        nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX,
        "mf-1",
        "mf-prefix",
    );

    let found = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(found.items[0].image.image_hash, "mf-prefix");
    assert_eq!(found.items[0].album_image.image_hash, "al-prefix");

    // Now rewrite the same two rows under the kind NAMES and re-read: the
    // hydration queries (which bind the prefix) must find nothing, leaving both
    // artwork fields zero — the unresolved state, not absent.
    on(&store, |conn| {
        conn.execute("DELETE FROM item_artwork", []).expect("clear");
    });
    seed_state(&store, "album", "al-1", "al-name");
    seed_state(&store, "media_file", "mf-1", "mf-name");
    let missed = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(missed.items[0].image, ItemImage::default());
    assert_eq!(missed.items[0].album_image, ItemImage::default());
    assert!(
        !missed.items[0].image.image_absent,
        "an invisible row reads as unresolved, not absent"
    );
    // The row count proves the rows exist under the wrong token.
    let rows = on(&store, |conn| {
        conn.query_row("SELECT count(*) FROM item_artwork", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count")
    });
    assert_eq!(rows, 2, "both kind-name rows are present but never matched");
    std::fs::remove_file(path).ok();
}

/// `artworkBatchSize` = 200 (`artwork_repository.go:16`), so 201 ids hydrate
/// across TWO statements and every id still gets its entry.
#[test]
fn chunk_boundary_at_200_ids_hydrates_every_id() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    assert_eq!(nd_db::artwork_repo::BATCH_SIZE, 200);
    let total = nd_db::artwork_repo::BATCH_SIZE + 1;
    for n in 0..total {
        let id = format!("al-{n:03}");
        seed_album(&store, lib, &id);
        // Only every third id has a state row, so a dropped chunk cannot pass
        // by accident.
        if n % 3 == 0 {
            seed_state(&store, "al", &id, &format!("h{n}"));
        }
    }
    let outcome = on(&store, |conn| {
        AlbumRepo::new(conn).get_all(None, false, None, vec!["album.id".to_string()], 0, 0)
    })
    .expect("read");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), total);
    for (n, album) in outcome.items.iter().enumerate() {
        let expected = if n % 3 == 0 {
            format!("h{n}")
        } else {
            String::new()
        };
        assert_eq!(
            album.image.image_hash,
            expected,
            "chunked hydration missed id {} of {} (chunk {})",
            album.id,
            total,
            n / nd_db::artwork_repo::BATCH_SIZE
        );
    }
    // The statement shape itself: 201 ids render two chunks, each binding kind +
    // image_type + its own slice of ids.
    let ids: Vec<String> = (0..total).map(|n| format!("al-{n:03}")).collect();
    let statements =
        nd_db::artwork_repo::get_info_for_items_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &ids)
            .expect("renders");
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].1.len(), 2 + nd_db::artwork_repo::BATCH_SIZE);
    assert_eq!(statements[1].1.len(), 2 + 1);
    std::fs::remove_file(path).ok();
}

/// Go logs and swallows a failed lookup and returns an EMPTY map so the page
/// still renders (`artwork_hydration.go:62-66`); the port hands back the empty
/// entries plus the error text. Dropping `item_artwork` is the failure.
#[test]
fn artwork_query_failure_leaves_items_unresolved_and_the_read_succeeds() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "h1");
    seed_track(&store, lib, "mf-1", "al-1", true, 0);
    seed_state(&store, "mf", "mf-1", "ownhash");

    on(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop");
    });

    let album = on(&store, |conn| {
        AlbumRepo::new(conn).get_all(None, false, None, vec!["album.id".to_string()], 0, 0)
    })
    .expect("album read still succeeds");
    assert_eq!(album.items.len(), 1, "the rows come back regardless");
    assert_eq!(album.items[0].image, ItemImage::default());
    let message = album
        .artwork_error
        .as_ref()
        .expect("the swallowed failure is exposed to the caller's logger");
    assert!(
        message.contains("item_artwork"),
        "the text names the failing table: {message}"
    );

    let track = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("media_file read still succeeds");
    assert_eq!(track.items.len(), 1);
    assert_eq!(track.items[0].image, ItemImage::default());
    assert_eq!(track.items[0].album_image, ItemImage::default());
    // Two lookups failed (album kind then media_file kind), reported in Go's
    // call order.
    let message = track
        .artwork_error
        .as_ref()
        .expect("both swallowed failures are exposed");
    assert_eq!(
        message.matches("item_artwork").count(),
        2,
        "both kinds report separately: {message}"
    );

    // Get keeps Go's swallow-and-log shape: the record still comes back.
    let one = on(&store, |conn| {
        AlbumRepo::new(conn).get("al-1", None, false, None)
    })
    .expect("get");
    assert_eq!(one.image, ItemImage::default());
    std::fs::remove_file(path).ok();
}

/// `hydrate_items` with no ids issues no query at all (`:59-61`), so an empty
/// page never touches `item_artwork` — here, with the table gone.
#[test]
fn empty_id_list_queries_nothing() {
    let (store, path) = fresh_store();
    on(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop");
    });
    let (infos, error) = on(&store, |conn| {
        artwork_hydration::hydrate_item_images(conn, nd_id::ARTWORK_KIND_ALBUM_PREFIX, &[])
    });
    assert!(infos.is_empty());
    assert_eq!(error, None, "no query, so no failure to report");
    std::fs::remove_file(path).ok();
}

/// The full branch matrix as one table, each row's expectation read off Go's
/// source rather than recomputed: `eligible = has_cover_art && flag`
/// (`:99,111`), `ownResolved` = an `mf` entry exists (only queried when eligible,
/// `:106`), `album` = the `al` entry (missing / absent / found), and the disc
/// number. Columns: id, has_cover_art, disc_number, own state (`None` = no row,
/// `Some("")` = absent, `Some(h)` = found), album state, expected own image.
/// One row of the branch matrix: the two Go inputs (`has_cover_art`,
/// `disc_number`), the own state (`None` = no row, `Some("")` = resolved-absent,
/// `Some(h)` = resolved-found) and the album state (same spelling), plus the
/// own-image expectation read straight off Go's source.
struct MatrixRow {
    id: &'static str,
    album_id: &'static str,
    has_cover_art: bool,
    disc_number: i32,
    own: Option<&'static str>,
    album: Option<&'static str>,
    expected: ItemImage,
}

fn found(hash: &str) -> ItemImage {
    ItemImage {
        image_hash: hash.to_string(),
        ..ItemImage::default()
    }
}

#[must_use]
fn matrix_rows() -> Vec<MatrixRow> {
    let absent_placeholder = ItemImage {
        image_absent: true,
        ..ItemImage::default()
    };
    vec![
        // A: eligible + own found → :112-116 own wins (continue), disc 1 aside.
        MatrixRow {
            id: "a",
            album_id: "al-1",
            has_cover_art: true,
            disc_number: 1,
            own: Some("own"),
            album: Some("album"),
            expected: found("own"),
        },
        // B: eligible + own found, disc 0 → same continue, disc never consulted.
        MatrixRow {
            id: "b",
            album_id: "al-1",
            has_cover_art: true,
            disc_number: 0,
            own: Some("own"),
            album: Some("album"),
            expected: found("own"),
        },
        // C: eligible + own ABSENT, disc 0 → own_wont_resolve (`:117`), so the
        //    present-and-found album is inherited (:120-125).
        MatrixRow {
            id: "c",
            album_id: "al-1",
            has_cover_art: true,
            disc_number: 0,
            own: Some(""),
            album: Some("album"),
            expected: found("album"),
        },
        // D: eligible + own ABSENT, disc 2 → the present-and-found arm continues
        //    WITHOUT inheriting, because disc art is served separately (:121-124).
        MatrixRow {
            id: "d",
            album_id: "al-1",
            has_cover_art: true,
            disc_number: 2,
            own: Some(""),
            album: Some("album"),
            expected: ItemImage::default(),
        },
        // E: eligible + own UNRESOLVED, disc 0 → own_wont_resolve is FALSE, so no
        //    inherit even though the album art is found (:117, :121).
        MatrixRow {
            id: "e",
            album_id: "al-1",
            has_cover_art: true,
            disc_number: 0,
            own: None,
            album: Some("album"),
            expected: ItemImage::default(),
        },
        // F: eligible + own UNRESOLVED, album ABSENT → own_wont_resolve false, so
        //    the absent arm (:131) never fires: the track stays optimistic.
        MatrixRow {
            id: "f",
            album_id: "al-2",
            has_cover_art: true,
            disc_number: 0,
            own: None,
            album: Some(""),
            expected: ItemImage::default(),
        },
        // G: NOT eligible (own state row exists but its kind is never queried),
        //    disc 0, album found → inherit.
        MatrixRow {
            id: "g",
            album_id: "al-1",
            has_cover_art: false,
            disc_number: 0,
            own: Some("own"),
            album: Some("album"),
            expected: found("album"),
        },
        // H: NOT eligible, disc 0, album ABSENT → the definitive placeholder case
        //    (:131-133): image_absent true and no hash.
        MatrixRow {
            id: "h",
            album_id: "al-2",
            has_cover_art: false,
            disc_number: 0,
            own: None,
            album: Some(""),
            expected: absent_placeholder,
        },
        // I: NOT eligible, disc 2, album ABSENT → :128-130 continues first, so the
        //    track stays requestable (NOT absent).
        MatrixRow {
            id: "i",
            album_id: "al-2",
            has_cover_art: false,
            disc_number: 2,
            own: None,
            album: Some(""),
            expected: ItemImage::default(),
        },
        // J: NOT eligible, disc 0, album entry MISSING → :131 requires the entry to
        //    be present, so this is unresolved rather than absent.
        MatrixRow {
            id: "j",
            album_id: "al-3",
            has_cover_art: false,
            disc_number: 0,
            own: None,
            album: None,
            expected: ItemImage::default(),
        },
        // K: NOT eligible, disc 2, album entry MISSING → unresolved.
        MatrixRow {
            id: "k",
            album_id: "al-3",
            has_cover_art: false,
            disc_number: 2,
            own: None,
            album: None,
            expected: ItemImage::default(),
        },
        // L: eligible, own found, album entry MISSING → own wins regardless.
        MatrixRow {
            id: "l",
            album_id: "al-3",
            has_cover_art: true,
            disc_number: 0,
            own: Some("own"),
            album: None,
            expected: found("own"),
        },
    ]
}

/// Execute [`matrix_rows`] against one DB: three albums (found / absent /
/// entry-missing) and one track per row, flag on.
#[test]
fn branch_matrix_table_matches_go() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    // The album rows come out of the matrix itself, so the expectations and the
    // data cannot drift: al-1 is found, al-2 is known-absent, al-3 has no `al`
    // state row at all. Rows sharing an album must agree about its state.
    for id in ["al-1", "al-2", "al-3"] {
        seed_album(&store, lib, id);
    }
    for row in matrix_rows() {
        if let Some(hash) = row.album {
            let seeded = on(&store, |conn| {
                let mut stmt = conn
                    .prepare("SELECT hash FROM item_artwork WHERE item_kind = 'al' AND item_id = ?")
                    .expect("album state query");
                let mut rows = stmt.query([&row.album_id]).expect("album state rows");
                rows.next()
                    .expect("album state row")
                    .map(|r| r.get::<_, String>(0).expect("hash column"))
            });
            if let Some(existing) = seeded {
                assert_eq!(existing, hash, "{} disagrees with {}", row.id, row.album_id);
            } else {
                seed_state(&store, "al", row.album_id, hash);
            }
        }
    }

    for row in matrix_rows() {
        seed_track(
            &store,
            lib,
            row.id,
            row.album_id,
            row.has_cover_art,
            row.disc_number,
        );
        if let Some(hash) = row.own {
            seed_state(&store, "mf", row.id, hash);
        }
    }

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), matrix_rows().len());

    let album_image_expected = |album_id: &str| match album_id {
        "al-1" => ItemImage {
            image_hash: "album".to_string(),
            ..ItemImage::default()
        },
        "al-2" => ItemImage {
            image_absent: true,
            ..ItemImage::default()
        },
        _ => ItemImage::default(),
    };
    for row in matrix_rows() {
        let track = outcome
            .items
            .iter()
            .find(|m| m.id == row.id)
            .unwrap_or_else(|| panic!("{} missing from the page", row.id));
        assert_eq!(
            track.image, row.expected,
            "{} (album {}, cover {}, disc {}): own image",
            row.id, row.album_id, row.has_cover_art, row.disc_number
        );
        assert_eq!(
            track.album_image,
            album_image_expected(row.album_id),
            "{}: album_image is applied on every branch (:110)",
            row.id
        );
    }
    std::fs::remove_file(path).ok();
}

/// `GetRandom` on the artwork-failure path: Go's `GetRandom` runs `hydrateArtwork`
/// on the hydrated subset (`mediafile_repository.go:280-282`) and
/// `hydrateItemImages` swallows a failed lookup, logs and returns an EMPTY map
/// (`artwork_hydration.go:62-66`), so the random read still returns its rows with
/// every `ItemImage` left zero. The two-pass statement is NOT the failure — only
/// the artwork lookup is.
#[test]
fn get_random_succeeds_with_the_artwork_table_gone() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");
    seed_track(&store, lib, "mf-1", "al-1", true, 0);
    seed_state(&store, "mf", "mf-1", "ownhash");
    seed_track(&store, lib, "mf-2", "al-1", false, 0);

    on(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop");
    });

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_random(None, None, 0, None, COVER_ART_ON)
    })
    .expect("the random read still succeeds with the artwork table gone");
    assert_eq!(outcome.items.len(), 2, "both rows come back anyway");
    for track in &outcome.items {
        assert_eq!(
            track.image,
            ItemImage::default(),
            "{}: image stays zero",
            track.id
        );
        assert_eq!(
            track.album_image,
            ItemImage::default(),
            "{}: album_image stays zero",
            track.id
        );
    }
    let message = outcome
        .artwork_error
        .as_ref()
        .expect("both swallowed failures are exposed");
    assert_eq!(
        message.matches("item_artwork").count(),
        2,
        "album kind then media_file kind, in Go's call order: {message}"
    );
    std::fs::remove_file(path).ok();
}

/// Duplicate ids are NOT deduplicated before the IN-list: Go collects ids in slice
/// order with duplicates retained (`artwork_hydration.go:81-84`), and media_file's
/// `albumIDs` (`:98-103`) has one entry per track, so a page of tracks from one
/// album repeats that album id. A repeated id hydrates identically to a single one,
/// and the statement keeps both bindings.
#[test]
fn duplicate_ids_keep_their_bindings_and_hydrate_identically() {
    let (store, path) = fresh_store();
    let lib = seed_library(&store);
    seed_album(&store, lib, "al-1");
    seed_state(&store, "al", "al-1", "albumhash");
    // Two tracks of the SAME album: the album-id list is ["al-1", "al-1"].
    seed_track(&store, lib, "mf-a", "al-1", false, 0);
    seed_track(&store, lib, "mf-b", "al-1", false, 0);

    // Statement shape: one chunk, and the IN-list still binds the id twice
    // (kind + image_type + two id bindings).
    let dup = ["al-1".to_string(), "al-1".to_string()];
    let statements =
        nd_db::artwork_repo::get_info_for_items_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &dup)
            .expect("renders");
    assert_eq!(statements.len(), 1, "two ids are still one chunk");
    assert_eq!(
        statements[0].1.len(),
        4,
        "kind, image_type and BOTH id bindings survive: {:?}",
        statements[0].1
    );
    assert_eq!(
        statements[0].0.matches("?").count(),
        4,
        "the IN-list renders two placeholders: {}",
        statements[0].0
    );

    // Executed: the duplicated-id lookup returns ONE entry (Go's map is keyed by
    // item_id, artwork_repository.go:178-180), and both tracks hydrate the same way.
    let (infos, error) = on(&store, |conn| {
        artwork_hydration::hydrate_item_images(conn, nd_id::ARTWORK_KIND_ALBUM_PREFIX, &dup)
    });
    assert_eq!(error, None);
    assert_eq!(infos.len(), 1, "the map is keyed by item_id");
    assert_eq!(infos["al-1"].hash, "albumhash");

    let outcome = on(&store, |conn| {
        MediaFileRepo::new(conn).get_all(None, None, &["media_file.id"], 0, 0, COVER_ART_ON)
    })
    .expect("read");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), 2);
    let expected = ItemImage {
        image_hash: "albumhash".to_string(),
        ..ItemImage::default()
    };
    for track in &outcome.items {
        assert_eq!(
            track.album_image, expected,
            "{} hydrates identically under a duplicated id",
            track.id
        );
        // Both tracks are disc 0 and not eligible, so the inherit arm
        // (`artwork_hydration.go:120-125`) gives them the SAME own image too — the
        // duplicated album id resolves identically for both.
        assert_eq!(
            track.image, expected,
            "{}: own image under a duplicated album id",
            track.id
        );
    }
    std::fs::remove_file(path).ok();
}
