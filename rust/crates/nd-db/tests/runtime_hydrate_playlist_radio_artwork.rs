//! S-D3f golden — the playlist (`pl`) and radio (`ra`) `hydrateItems` wrappers
//! and the executed read sides of both repositories, EXECUTED against a real
//! SQLite DB: fresh schema via `nd_dbschema`, one isolated temp file per test
//! (harness shape copied from `tests/runtime_hydrate_artist.rs:92-120`, artwork
//! seeding idioms from `tests/runtime_artwork_hydration.rs:278-306`).
//!
//! Go truth under test: `playlistRepository.hydrateArtwork`
//! (`persistence/playlist_repository.go:191-194`, reached by `findBy` at `:187`,
//! `GetAll` at `:207` and `GetPlaylists` at `:253`), `radioRepository.hydrateArtwork`
//! (`radio_repository.go:80-83`, reached by `Get` at `:64` and `GetAll` at `:75`),
//! `findBy` (`:175-189`), `Get`/`GetAll`/`FindByPath`/`GetPlaylists`
//! (`:148-150`, `:196-210`, `:171-173`, `:237-255`), radio's `Get`/`GetAll`
//! (`:56-66`, `:68-77`), and the shared `hydrateItems`/`hydrateItemImages`/
//! `applyItemImage` (`persistence/artwork_hydration.go:56-90`) over
//! `GetInfoForItems` (`persistence/artwork_repository.go:159-182`).
//!
//! Every claim below was also executed against the real Go repositories by the
//! parent's oracle (`/tmp/sd3f_oracle/`, `/tmp/sd3f_oracle2/`); the divergences
//! that oracle exposed are recorded in the tests that pin them.
//!
//! The claims this slice pins, each executed:
//! (1) all FOUR artwork states for BOTH kinds — resolved, known-absent
//!     (`hash = ''`), unresolved (no `item_artwork` row at all, which is NOT
//!     absent), and the DANGLING-HASH state (a state row whose hash has no
//!     `artwork` row: the `COALESCE` arms yield empty metadata but the hash
//!     survives, so `image_hash` is set and `image_absent` stays false);
//! (2) `NotFound` comes from `findBy`'s row count for the playlist reads and from
//!     `queryOne`'s no-rows mapping for radio's `Get`, and in BOTH cases the
//!     artwork lookup never runs — proven by reading with `item_artwork` dropped,
//!     where a lookup would have produced a swallowed error instead of NotFound;
//! (3) `GetAll`/`GetPlaylists` never report NotFound (dbx's `All` makes an empty
//!     non-nil slice, `dbx@v1.12.0/query.go:278-291`);
//! (4) `FindByPath` emits no visibility clause, exactly as Go calls it;
//! (5) a dropped `item_artwork` table fails the lookup: the read still returns
//!     every row with a zero image, and the failure is REPORTED by `get_all` and
//!     SWALLOWED by `get`, mirroring Go's `log.Error` + empty map
//!     (`artwork_hydration.go:62-66`);
//! (6) `item_kind` is the kind PREFIX: a `pl` state row never lands on a radio and
//!     vice versa, the kind NAMES (`playlist`, `radio`) match nothing, and a
//!     playlist whose id equals an `mf` state row's id gets nothing from it;
//! (7) the 200-id chunk boundary over 201 playlists hydrates every id and leaves
//!     the id list NON-deduplicated (no re-write, no row loss);
//! (8) the write maps are UNCHANGED by the new `image` field — the playlist key
//!     set derived from the fresh DDL, `put_sql`'s INSERT byte-identical with the
//!     image zeroed vs hydrated, and `put_cols` still exactly the 7 radio legs;
//! (9) `annotation`/`owner_name` stay derived-only (no annotation column and no
//!     `owner_name` is a write key for either repo);
//! (10) an `Offset`-only page (`Max == 0`) FAILS in Go (`near "1": syntax error`)
//!     but executes in the port, which emits `LIMIT -1 OFFSET n` — the builder's
//!     own cited deviation (`sqlrepo.rs:313-319`), pinned here rather than "fixed";
//! (11) radio's NULL `created_at`/`updated_at` is a SCAN error, not NotFound and
//!     not a zero timestamp — Go's non-pointer `time.Time` (`model/radio.go:19-20`)
//!     rejects NULL (`sql: Scan error on column index 4, name "created_at":
//!     unsupported Scan, storing driver.Value type <nil> into type *time.Time`,
//!     oracle-executed) — and the artwork layer never runs for such a row because
//!     Go returns before `hydrateArtwork` (`radio_repository.go:60-62`).

use nd_db::artwork_hydration::ItemImage;
use nd_db::playlist_repo::{self, PlaylistError, PlaylistRecord, PlaylistRepo};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::radio_repo::{self, RadioError, RadioRecord, RadioRepo};
use nd_db::sql_restful::{Filter, RestQueryOptions};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
/// The logged user id (Go's `loggedUser(ctx).ID`): an ADMIN, whose
/// `userFilter` is the `And{}` identity (`playlist_repository.go:85-86`), which
/// the traced SQL renders as `AND (1=1)` and the port omits entirely.
const ADMIN: &str = "adm";
/// A non-admin (`is_admin = 0`), whose visibility clause is
/// `public = ? OR owner_id = ?` (`playlist_repository.go:87-90`).
const LIMITED: &str = "lim";
/// The `pl` artwork kind prefix (`nd-id/src/artwork.rs:51`, Go's
/// `model.KindPlaylistArtwork` at `model/artwork_id.go:31`).
const KIND_PL: &str = "pl";
/// The `ra` prefix (`nd-id/src/artwork.rs:67`, Go's `model.KindRadioArtwork`,
/// `model/artwork_id.go:33`).
const KIND_RA: &str = "ra";
/// The kind NAMES, which must match nothing: the column stores the PREFIX
/// (`artwork_repository.go:169`).
const KIND_PL_NAME: &str = "playlist";
const KIND_RA_NAME: &str = "radio";
/// The media-file kind, for the cross-kind isolation claim.
const KIND_MF: &str = "mf";
/// `crate::artwork_repo::BATCH_SIZE` — Go's `artworkBatchSize`
/// (`artwork_repository.go:16`).
const BATCH: usize = 200;

/// One isolated on-disk DB per invocation: a unique temp path, freshly migrated,
/// pooled so `SEEDEDRAND`/`NATSORT` are registered per connection.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-hydrate-pl-ra-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one closure on a leased connection. Lease/store errors panic; the repo's
/// own error value is handed back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
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
fn i64(value: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(value)
}
/// The columns a table actually has, read from the live schema. Used to DERIVE
/// the expected write-map key set instead of hard-coding a guessed count.
fn columns(store: &SQLStore, table: &str) -> Vec<String> {
    lease(store, |conn| {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap_or_else(|e| panic!("table_info({table}): {e}"));
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap_or_else(|e| panic!("table_info({table}) rows: {e}"));
        rows.collect::<Result<Vec<String>, _>>()
            .unwrap_or_else(|e| panic!("table_info({table}) decode: {e}"))
    })
}

/// `user` rows: the admin and a non-admin. `selectPlaylist` INNER JOINs `user`
/// (`playlist_repository.go:258`), so a playlist is unreadable without its owner.
fn seed_users(store: &SQLStore) {
    for (id, admin) in [(ADMIN, 1), (LIMITED, 0)] {
        insert(
            store,
            "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, \
             updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                v(id),
                v(id),
                v(id),
                v(&format!("{id}@x.y")),
                v("CIPHER"),
                i64(admin),
                v(T0),
                v(T1),
            ],
        );
    }
}

/// A playlist written directly (not through `put_sql`, so this file stays
/// independent of the write lane). `owner_id` is the admin by default.
fn seed_playlist(store: &SQLStore, id: &str, public: bool, owner: &str) {
    insert(
        store,
        "INSERT INTO playlist (id, name, comment, duration, song_count, public, created_at, \
         updated_at, path, sync, size, rules, evaluated_at, owner_id, average_rating, \
         imported_hash, uploaded_image, external_image_url) \
         VALUES (?, ?, '', 3.0, 2, ?, ?, ?, ?, 0, 100, NULL, NULL, ?, 0, '', '', '')",
        &[
            v(id),
            v(id),
            i64(i64::from(public)),
            v(T0),
            v(T1),
            v(&format!("/p/{id}")),
            v(owner),
        ],
    );
}

/// A radio row, all columns present.
fn seed_radio(store: &SQLStore, id: &str) {
    insert(
        store,
        "INSERT INTO radio (id, name, stream_url, home_page_url, created_at, updated_at, \
         uploaded_image) VALUES (?, ?, 'https://x/y', '', ?, ?, '')",
        &[v(id), v(id), v(T0), v(T1)],
    );
}

/// A radio row whose timestamp columns are NULL — reachable, because the fresh
/// DDL declares `created_at datetime, updated_at datetime` with no DEFAULT
/// (`nd-dbschema/src/schema_ddl.rs:54`).
fn seed_radio_null_times(store: &SQLStore, id: &str) {
    insert(
        store,
        "INSERT INTO radio (id, name, stream_url, home_page_url, created_at, updated_at, \
         uploaded_image) VALUES (?, ?, 'https://x/y', '', NULL, NULL, '')",
        &[v(id), v(id)],
    );
}

/// A `playlist_tracks` row linking a playlist to a media file id. The media file
/// itself need not exist: `GetPlaylists` joins only `playlist_tracks`
/// (`playlist_repository.go:240-242`).
fn seed_track_link(store: &SQLStore, playlist_id: &str, media_file_id: &str, pos: i64) {
    insert(
        store,
        "INSERT INTO playlist_tracks (id, playlist_id, media_file_id) VALUES (?, ?, ?)",
        &[i64(pos), v(playlist_id), v(media_file_id)],
    );
}

/// An `item_artwork` state row at `image_type = 'primary'` — the only slot the
/// hydration query reads (`artwork_repository.go:172`). `hash = ""` is the
/// known-absent state (`model/artwork.go:50`).
fn seed_state(store: &SQLStore, kind: &str, id: &str, hash: &str) {
    insert(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, updated_at, \
         attempted_at) VALUES (?, ?, 'primary', ?, 'folder', ?, NULL)",
        &[v(kind), v(id), v(hash), v(T1)],
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
        &[
            v(hash),
            i64(width),
            i64(height),
            v(blur),
            v(thumb),
            v(color),
        ],
    );
}

/// The four artwork states the oracle exercised, keyed by playlist id:
/// `pl-1` resolved, `pl-2` known-absent, `pl-3` unresolved, `pl-4` dangling hash.
fn seed_playlist_states(store: &SQLStore) {
    seed_image(store, "PLH", 800, 800, "BLURPL", "THUMBPL", "#aaaaaa");
    seed_state(store, KIND_PL, "pl-1", "PLH");
    seed_state(store, KIND_PL, "pl-2", "");
    // pl-3 gets NO state row at all → unresolved.
    // pl-4 has a state row whose hash has no `artwork` row → the LEFT JOIN's
    // COALESCE arms give empty metadata, and the hash survives.
    seed_state(store, KIND_PL, "pl-4", "GHOST");
}

/// The same four states for radios.
fn seed_radio_states(store: &SQLStore) {
    seed_image(store, "RAH", 640, 480, "BLURRA", "THUMBRA", "#bbbbbb");
    seed_state(store, KIND_RA, "ra-1", "RAH");
    seed_state(store, KIND_RA, "ra-2", "");
    seed_state(store, KIND_RA, "ra-4", "GHOST");
}

fn resolved_playlist_image() -> ItemImage {
    ItemImage {
        image_hash: "PLH".to_string(),
        image_absent: false,
        blur_hash: "BLURPL".to_string(),
        thumb_hash: "THUMBPL".to_string(),
        dominant_color: "#aaaaaa".to_string(),
        image_width: 800,
        image_height: 800,
    }
}

fn resolved_radio_image() -> ItemImage {
    ItemImage {
        image_hash: "RAH".to_string(),
        image_absent: false,
        blur_hash: "BLURRA".to_string(),
        thumb_hash: "THUMBRA".to_string(),
        dominant_color: "#bbbbbb".to_string(),
        image_width: 640,
        image_height: 480,
    }
}

/// The known-absent state: `hash = ''` → `Image()` sets `image_absent`
/// (`artwork_hydration.go:71-75`, `model/artwork.go:82-95`) and every other
/// field stays empty.
fn absent_image() -> ItemImage {
    ItemImage {
        image_absent: true,
        ..ItemImage::default()
    }
}

/// The dangling-hash state: the hash survives, the metadata does not
/// (`COALESCE(a.blur_hash, '')` etc., `artwork_repository.go:162-167`).
fn dangling_image(hash: &str) -> ItemImage {
    ItemImage {
        image_hash: hash.to_string(),
        ..ItemImage::default()
    }
}

/// Look one item's hydrated image up by id out of a read result. The accessor
/// pair is passed in, because playlist and radio records are different types.
fn image_of<T>(
    items: &[T],
    id: &str,
    id_of: impl Fn(&T) -> &str,
    get_image: impl Fn(&T) -> &ItemImage,
) -> ItemImage {
    items
        .iter()
        .find(|item| id_of(item) == id)
        .map(|item| get_image(item).clone())
        .unwrap_or_else(|| panic!("no item with id {id} in the read result"))
}

/// A resolved state whose image row is present: hash plus the full joined
/// metadata, i.e. every `COALESCE` arm filled.
fn chunk_image(hash: &str) -> ItemImage {
    ItemImage {
        image_hash: hash.to_string(),
        blur_hash: "BLURC".to_string(),
        thumb_hash: "THUMBC".to_string(),
        dominant_color: "#eeeeee".to_string(),
        image_width: 640,
        image_height: 480,
        ..ItemImage::default()
    }
}

/// The playlist artwork states, executed through `PlaylistRepo::get` — the Go
/// path `Get` → `findBy` → `hydrateArtwork` (`:148-150`, `:175-189`, `:191-194`).
#[test]
fn playlist_get_pins_all_four_artwork_states() {
    let (store, path) = fresh_store();
    seed_users(&store);
    for id in ["pl-1", "pl-2", "pl-3", "pl-4"] {
        seed_playlist(&store, id, true, ADMIN);
    }
    seed_playlist_states(&store);

    for (id, expected) in [
        ("pl-1", resolved_playlist_image()),
        ("pl-2", absent_image()),
        ("pl-3", ItemImage::default()),
        ("pl-4", dangling_image("GHOST")),
    ] {
        let got = lease(&store, |conn| PlaylistRepo::new(conn).get(id, ADMIN, &None));
        let record = got.unwrap_or_else(|e| panic!("Get({id}) failed: {e}"));
        assert_eq!(record.id, id, "the row's own id survives hydration");
        assert_eq!(record.name, id);
        // `OwnerName` is scanned by field name despite `structs:"-"`.
        assert_eq!(record.owner_name, ADMIN);
        assert_eq!(record.image, expected, "{id} artwork state");
        if id == "pl-3" {
            assert!(
                !record.image.image_absent,
                "an item with no state row is UNRESOLVED, never falsely absent"
            );
        }
    }
    std::fs::remove_file(path).ok();
}

/// The same four states for radio, through `RadioRepo::get` (`radio_repository.go:56-66`).
#[test]
fn radio_get_pins_all_four_artwork_states() {
    let (store, path) = fresh_store();
    for id in ["ra-1", "ra-2", "ra-3", "ra-4"] {
        seed_radio(&store, id);
    }
    seed_radio_states(&store);

    for (id, expected) in [
        ("ra-1", resolved_radio_image()),
        ("ra-2", absent_image()),
        ("ra-3", ItemImage::default()),
        ("ra-4", dangling_image("GHOST")),
    ] {
        let got = lease(&store, |conn| RadioRepo::new(conn).get(id));
        let record = got.unwrap_or_else(|e| panic!("Get({id}) failed: {e}"));
        assert_eq!(record.id, id);
        assert_eq!(record.stream_url, "https://x/y");
        assert_eq!(record.image, expected, "{id} artwork state");
        if id == "ra-3" {
            assert!(
                !record.image.image_absent,
                "no state row means UNRESOLVED, not absent"
            );
        }
    }
    std::fs::remove_file(path).ok();
}

/// `GetAll` (`:196-210`) hydrates every row, keeps statement order, and reports
/// no artwork error. The non-admin's visibility clause is the same read path.
#[test]
fn playlist_get_all_hydrates_every_row_for_admin_and_non_admin() {
    let (store, path) = fresh_store();
    seed_users(&store);
    for id in ["pl-1", "pl-2", "pl-3", "pl-4"] {
        // Public, so the non-admin sees all four (`public = ? OR owner_id = ?`).
        seed_playlist(&store, id, true, ADMIN);
    }
    seed_playlist_states(&store);

    for user in [None, Some(LIMITED.to_string())] {
        let outcome = lease(&store, |conn| {
            PlaylistRepo::new(conn).get_all(
                ADMIN,
                &user,
                &[playlist_repo::natural_sort("playlist.name", false)],
            )
        })
        .expect("GetAll");
        assert_eq!(outcome.artwork_error, None, "the lookup succeeded");
        assert_eq!(outcome.items.len(), 4, "user filter {user:?}");
        assert_eq!(
            outcome
                .items
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["pl-1", "pl-2", "pl-3", "pl-4"],
            "statement order is preserved"
        );
        assert_eq!(
            image_of(&outcome.items, "pl-1", |p| p.id.as_str(), |p| &p.image),
            resolved_playlist_image()
        );
        assert_eq!(
            image_of(&outcome.items, "pl-2", |p| p.id.as_str(), |p| &p.image),
            absent_image()
        );
        assert_eq!(
            image_of(&outcome.items, "pl-3", |p| p.id.as_str(), |p| &p.image),
            ItemImage::default()
        );
        assert_eq!(
            image_of(&outcome.items, "pl-4", |p| p.id.as_str(), |p| &p.image),
            dangling_image("GHOST")
        );
        for record in &outcome.items {
            assert_eq!(record.owner_name, ADMIN, "the owner_name alias scanned");
        }
    }
    std::fs::remove_file(path).ok();
}

/// `FindByPath` (`:171-173`) reaches the same `findBy`, and its traced statement
/// carries NO visibility clause (`WHERE path = {:p2}`), so a non-admin still
/// finds another user's playlist by path.
#[test]
fn playlist_find_by_path_hydrates_and_ignores_visibility() {
    let (store, path) = fresh_store();
    seed_users(&store);
    // Private and owned by the admin — a visibility clause would hide it.
    seed_playlist(&store, "pl-1", false, ADMIN);
    seed_playlist_states(&store);

    let got = lease(&store, |conn| {
        PlaylistRepo::new(conn).find_by_path("/p/pl-1", ADMIN)
    });
    let record = got.expect("FindByPath finds the private playlist (no visibility clause)");
    assert_eq!(record.id, "pl-1");
    assert_eq!(record.image, resolved_playlist_image());

    let missing = lease(&store, |conn| {
        PlaylistRepo::new(conn).find_by_path("/p/nobody", ADMIN)
    });
    assert!(
        matches!(missing, Err(PlaylistError::NotFound)),
        "zero rows ⇒ NotFound, as findBy's len check says (:182-184): {missing:?}"
    );
    std::fs::remove_file(path).ok();
}

/// `GetPlaylists` (`:237-255`): the playlists containing a media file, hydrated.
#[test]
fn playlist_get_playlists_hydrates_the_matching_playlist() {
    let (store, path) = fresh_store();
    seed_users(&store);
    for id in ["pl-1", "pl-2", "pl-3", "pl-4"] {
        seed_playlist(&store, id, true, ADMIN);
    }
    seed_playlist_states(&store);
    seed_track_link(&store, "pl-1", "m1", 1);
    seed_track_link(&store, "pl-2", "m2", 1);

    let outcome = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_playlists("m1", ADMIN, &None, "playlist.name")
    })
    .expect("GetPlaylists");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), 1);
    assert_eq!(outcome.items[0].id, "pl-1");
    assert_eq!(outcome.items[0].image, resolved_playlist_image());

    // A media file in no playlist is an EMPTY Vec — Go's ErrNotFound arm
    // (:244-246) is dead, so the port must not invent a NotFound here.
    let none = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_playlists("m-ghost", ADMIN, &None, "playlist.name")
    })
    .expect("zero rows are Ok, not NotFound");
    assert!(none.items.is_empty());
    assert_eq!(none.artwork_error, None, "no ids ⇒ no artwork query at all");
    std::fs::remove_file(path).ok();
}

/// Radio's `GetAll` (`:68-77`) hydrates every row; the parsed `name` filter
/// narrows it to one hydrated row.
#[test]
fn radio_get_all_hydrates_every_row_and_applies_the_name_filter() {
    let (store, path) = fresh_store();
    for id in ["ra-1", "ra-2", "ra-3", "ra-4"] {
        seed_radio(&store, id);
    }
    seed_radio_states(&store);

    let outcome = lease(&store, |conn| RadioRepo::new(conn).get_all(None)).expect("GetAll");
    assert_eq!(outcome.artwork_error, None);
    assert_eq!(outcome.items.len(), 4);
    assert_eq!(
        image_of(&outcome.items, "ra-1", |r| r.id.as_str(), |r| &r.image),
        resolved_radio_image()
    );
    assert_eq!(
        image_of(&outcome.items, "ra-2", |r| r.id.as_str(), |r| &r.image),
        absent_image()
    );
    assert_eq!(
        image_of(&outcome.items, "ra-3", |r| r.id.as_str(), |r| &r.image),
        ItemImage::default()
    );
    assert_eq!(
        image_of(&outcome.items, "ra-4", |r| r.id.as_str(), |r| &r.image),
        dangling_image("GHOST")
    );

    // `Filters:{"name":"ra-1"}` → 1 row, hydrated (oracle-executed). `get_all` parses
    // the REST shape itself (`radio_repository.go:121`), and the registered `name`
    // mapping is `containsFilter("name")` (`radio_repository.go:22-24`), so this is a
    // LIKE `%ra-1%` rather than the hand-built `Eq` the renderer used to be handed.
    let filtered = RestQueryOptions {
        filters: [("name".to_string(), "ra-1".to_string())]
            .into_iter()
            .collect(),
        ..RestQueryOptions::default()
    };
    let one =
        lease(&store, |conn| RadioRepo::new(conn).get_all(Some(&filtered))).expect("filtered");
    assert_eq!(one.items.len(), 1);
    assert_eq!(one.items[0].id, "ra-1");
    assert_eq!(one.items[0].image, resolved_radio_image());
    // The parsed clause shape, asserted separately.
    let parsed = radio_repo::parse_rest_options(Some(&filtered));
    assert_eq!(
        parsed.filters,
        Some(vec![Filter::Like {
            column: "name".to_string(),
            pattern: "%ra-1%".to_string(),
        }]),
        "the name mapping is a contains-match, not an Eq"
    );
    std::fs::remove_file(path).ok();
}

/// NotFound for BOTH repos, with the artwork lookup provably never run: the state
/// table is DROPPED first, so a lookup that ran would have produced Go's
/// swallowed error path (`Ok` with a zero image) instead of `Err(NotFound)`.
#[test]
fn unknown_ids_are_not_found_and_never_reach_the_artwork_lookup() {
    let (store, path) = fresh_store();
    seed_users(&store);
    seed_playlist(&store, "pl-1", true, ADMIN);
    seed_radio(&store, "ra-1");
    lease(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop")
    });

    let playlist = lease(&store, |conn| {
        PlaylistRepo::new(conn).get("pl-nobody", ADMIN, &None)
    });
    assert!(
        matches!(playlist, Err(PlaylistError::NotFound)),
        "findBy's len check decides NotFound (:182-184): {playlist:?}"
    );
    let by_path = lease(&store, |conn| {
        PlaylistRepo::new(conn).find_by_path("/p/nobody", ADMIN)
    });
    assert!(
        matches!(by_path, Err(PlaylistError::NotFound)),
        "same findBy path for FindByPath: {by_path:?}"
    );
    // Radio's NotFound is queryOne's no-rows mapping (`sql_base_repository.go:375-388`).
    let radio = lease(&store, |conn| RadioRepo::new(conn).get("ra-nobody"));
    assert!(
        matches!(radio, Err(RadioError::NotFound)),
        "queryOne maps sql.ErrNoRows to model.ErrNotFound: {radio:?}"
    );
    std::fs::remove_file(path).ok();
}

/// An empty table is an empty `Vec` with `artwork_error == None` for every bulk
/// read, and never NotFound (dbx's `All` yields an empty non-nil slice).
#[test]
fn empty_tables_are_ok_with_no_artwork_error() {
    let (store, path) = fresh_store();
    seed_users(&store);

    let playlists = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_all(ADMIN, &None, &[])
    })
    .expect("GetAll");
    assert!(playlists.items.is_empty());
    assert_eq!(playlists.artwork_error, None, "no ids ⇒ no lookup ran");

    let by_media_file = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_playlists("m1", ADMIN, &None, "playlist.name")
    })
    .expect("GetPlaylists");
    assert!(by_media_file.items.is_empty());
    assert_eq!(by_media_file.artwork_error, None);

    let radios = lease(&store, |conn| RadioRepo::new(conn).get_all(None)).expect("GetAll");
    assert!(radios.items.is_empty());
    assert_eq!(radios.artwork_error, None);
    std::fs::remove_file(path).ok();
}

/// Go's swallow-and-render path (`artwork_hydration.go:62-66`): with
/// `item_artwork` gone the lookup fails, every read still returns its rows, each
/// with a ZERO (unresolved) image, and the failure text is reported by `get_all`
/// and swallowed by `get`. Executed in Go: the read returned its rows and the log
/// line was `Failed to hydrate artwork info onto page … no such table: item_artwork kind=playlist`.
#[test]
fn a_failed_lookup_is_reported_by_get_all_and_swallowed_by_get() {
    let (store, path) = fresh_store();
    seed_users(&store);
    for id in ["pl-1", "pl-2"] {
        seed_playlist(&store, id, true, ADMIN);
    }
    seed_playlist_states(&store);
    seed_radio(&store, "ra-1");
    seed_radio(&store, "ra-2");
    seed_radio_states(&store);
    lease(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop")
    });

    let playlists = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_all(ADMIN, &None, &[])
    })
    .expect("Ok");
    assert_eq!(playlists.items.len(), 2);
    for record in &playlists.items {
        assert_eq!(record.image, ItemImage::default(), "{} zero", record.id);
        assert!(!record.image.image_absent, "zero is unresolved, not absent");
    }
    let message = playlists
        .artwork_error
        .as_ref()
        .expect("get_all reports Go's swallowed log.Error");
    assert!(
        message.contains("item_artwork"),
        "the failure names the missing table: {message}"
    );

    let one = lease(&store, |conn| {
        PlaylistRepo::new(conn).get("pl-1", ADMIN, &None)
    })
    .expect("get is Ok");
    assert_eq!(one.image, ItemImage::default(), "Go's empty-map path");

    let radios = lease(&store, |conn| RadioRepo::new(conn).get_all(None)).expect("Ok");
    assert_eq!(radios.items.len(), 2);
    for record in &radios.items {
        assert_eq!(record.image, ItemImage::default(), "{} zero", record.id);
    }
    let message = radios
        .artwork_error
        .as_ref()
        .expect("radio get_all reports the failure too");
    assert!(message.contains("item_artwork"), "{message}");
    let one = lease(&store, |conn| RadioRepo::new(conn).get("ra-1")).expect("get is Ok");
    assert_eq!(one.image, ItemImage::default());

    // The one failure is reported once per read, not once per chunk.
    assert_eq!(message.matches("item_artwork").count(), 1, "{message}");
    std::fs::remove_file(path).ok();
}

/// `item_kind` is the PREFIX (`artwork_repository.go:169`): a `pl` state never
/// lands on a radio and vice versa, and the kind NAMES match nothing.
#[test]
fn kind_prefix_isolates_playlist_from_radio_and_kind_names_match_nothing() {
    let (store, path) = fresh_store();
    seed_users(&store);
    seed_playlist(&store, "same-id", true, ADMIN);
    seed_radio(&store, "same-id");
    // Same id, different kinds: each entity may only see its own row.
    seed_image(&store, "PLH", 800, 800, "BLURPL", "THUMBPL", "#aaaaaa");
    seed_image(&store, "RAH", 640, 480, "BLURRA", "THUMBRA", "#bbbbbb");
    seed_state(&store, KIND_PL, "same-id", "PLH");
    seed_state(&store, KIND_RA, "same-id", "RAH");

    let playlist = lease(&store, |conn| {
        PlaylistRepo::new(conn).get("same-id", ADMIN, &None)
    })
    .expect("playlist");
    assert_eq!(
        playlist.image,
        resolved_playlist_image(),
        "no radio art leaked"
    );
    let radio = lease(&store, |conn| RadioRepo::new(conn).get("same-id")).expect("radio");
    assert_eq!(
        radio.image,
        resolved_radio_image(),
        "no playlist art leaked"
    );

    // Kind NAMES seeded instead of prefixes hydrate nothing (`artwork_repository.go:169`
    // binds `ia.item_kind = kind.Prefix()`).
    let (store2, path2) = fresh_store();
    seed_users(&store2);
    seed_playlist(&store2, "pl-name", true, ADMIN);
    seed_radio(&store2, "ra-name");
    seed_image(&store2, "PLH", 800, 800, "BLURPL", "THUMBPL", "#aaaaaa");
    seed_image(&store2, "RAH", 640, 480, "BLURRA", "THUMBRA", "#bbbbbb");
    seed_state(&store2, KIND_PL_NAME, "pl-name", "PLH");
    seed_state(&store2, KIND_RA_NAME, "ra-name", "RAH");
    let playlist = lease(&store2, |conn| {
        PlaylistRepo::new(conn).get("pl-name", ADMIN, &None)
    })
    .expect("read");
    assert_eq!(
        playlist.image,
        ItemImage::default(),
        "kind NAME matches nothing"
    );
    let radio = lease(&store2, |conn| RadioRepo::new(conn).get("ra-name")).expect("read");
    assert_eq!(
        radio.image,
        ItemImage::default(),
        "kind NAME matches nothing"
    );

    std::fs::remove_file(path).ok();
    std::fs::remove_file(path2).ok();
}

/// The `pl` vs `mf` distinction: a media-file state row carrying a playlist's own
/// id contributes NOTHING to that playlist.
#[test]
fn a_playlist_never_reads_a_media_file_state_row_with_its_own_id() {
    let (store, path) = fresh_store();
    seed_users(&store);
    seed_playlist(&store, "shared", true, ADMIN);
    seed_radio(&store, "shared");
    seed_image(&store, "MFH", 300, 200, "BLURMF", "THUMBMF", "#dddddd");
    seed_image(&store, "RAH", 640, 480, "BLURRA", "THUMBRA", "#bbbbbb");
    // Only `mf` and `ra` states exist — no `pl` row at all.
    seed_state(&store, KIND_MF, "shared", "MFH");
    seed_state(&store, KIND_RA, "shared", "RAH");

    let playlist = lease(&store, |conn| {
        PlaylistRepo::new(conn).get("shared", ADMIN, &None)
    })
    .expect("read");
    assert_eq!(
        playlist.image,
        ItemImage::default(),
        "the mf row must not hydrate onto a playlist"
    );
    let radio = lease(&store, |conn| RadioRepo::new(conn).get("shared")).expect("read");
    assert_eq!(
        radio.image,
        resolved_radio_image(),
        "its own kind still resolves"
    );
    std::fs::remove_file(path).ok();
}

/// The 200-id chunk boundary (`artworkBatchSize = 200`,
/// `artwork_repository.go:16`, `:161`): 201 ids hydrate every id, the state table
/// is untouched (ids are NOT deduplicated and nothing is written), and the
/// statement count Go traced (2 chunks) is reproduced by the builder.
#[test]
fn chunk_boundary_at_201_playlists_hydrates_every_id_without_deduplication() {
    let (store, path) = fresh_store();
    seed_users(&store);
    let ids: Vec<String> = (1..=BATCH + 1).map(|n| format!("pl-{n}")).collect();
    for id in &ids {
        seed_playlist(&store, id, true, ADMIN);
    }
    seed_image(&store, "CHUNK", 640, 480, "BLURC", "THUMBC", "#eeeeee");
    // Resolved states at both ends of the boundary and in the tail chunk.
    for id in ["pl-1", "pl-200", "pl-201"] {
        seed_state(&store, KIND_PL, id, "CHUNK");
    }
    let before = lease(&store, |conn| {
        conn.query_row("SELECT count(*) FROM item_artwork", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count")
    });
    assert_eq!(before, 3);

    let outcome = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_all(ADMIN, &None, &[])
    })
    .expect("GetAll");
    assert_eq!(outcome.artwork_error, None, "both chunks succeeded");
    assert_eq!(outcome.items.len(), BATCH + 1, "every id came back");
    assert_eq!(
        image_of(&outcome.items, "pl-1", |p| p.id.as_str(), |p| &p.image),
        chunk_image("CHUNK")
    );
    assert_eq!(
        image_of(&outcome.items, "pl-200", |p| p.id.as_str(), |p| &p.image),
        chunk_image("CHUNK")
    );
    assert_eq!(
        image_of(&outcome.items, "pl-201", |p| p.id.as_str(), |p| &p.image),
        chunk_image("CHUNK")
    );
    // The rest have no state row: unresolved, and still present.
    assert_eq!(
        image_of(&outcome.items, "pl-199", |p| p.id.as_str(), |p| &p.image),
        ItemImage::default()
    );
    for record in &outcome.items {
        assert!(
            !record.image.image_absent,
            "a chunk failure would fake absence"
        );
    }

    let after = lease(&store, |conn| {
        conn.query_row("SELECT count(*) FROM item_artwork", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count")
    });
    assert_eq!(
        after, before,
        "hydration reads only — no dedup rewrite, no row lost"
    );

    // The statement shape the oracle traced: 2 chunks for 201 ids, IN-list in
    // slice order and NOT deduplicated.
    let statements = nd_db::artwork_repo::get_info_for_items_sql(
        KIND_PL,
        &["pl-1".to_string(), "pl-1".to_string(), "pl-2".to_string()],
    )
    .expect("one chunk");
    assert_eq!(statements.len(), 1, "3 ids fit one chunk");
    let (_, args) = &statements[0];
    let ids_in_list: Vec<String> = args
        .iter()
        .skip(2)
        .map(|value| match value {
            Value::Text(text) => text.clone(),
            other => panic!("unexpected bound value {other:?}"),
        })
        .collect();
    assert_eq!(
        ids_in_list,
        ["pl-1", "pl-1", "pl-2"],
        "duplicates survive, order kept"
    );
    std::fs::remove_file(path).ok();
}

/// The write maps are UNCHANGED by the new `image` field. The playlist key set is
/// DERIVED from the live fresh DDL (persisted columns minus `id`, minus `rules`
/// for the non-smart PostMapArgs arm), so the count cannot drift silently.
#[test]
fn image_changes_no_write_leg_for_playlist_or_radio() {
    let (store, path) = fresh_store();
    let ddl = columns(&store, "playlist");
    // Derived from the live DDL rather than a guessed count. Three DDL columns are
    // legitimately NOT map keys: `id` (the seam rejects it,
    // `crate::marshal::MarshalError::IdInMap`; the write legs take it as their own
    // argument), `rules` (deleted by PostMapArgs' non-smart arm,
    // `playlist_repository.go:49`), and `average_rating` — which lives on Go's
    // `Annotations` embed (`structs:"-"`, `model/playlist.go:21`, `model/annotation.go`)
    // and so never reaches `structs.Map`.
    let mut expected: Vec<String> = ddl
        .iter()
        .filter(|c| !matches!(c.as_str(), "id" | "rules" | "average_rating"))
        .cloned()
        .collect();
    // `playlist_row` renders a sorted `ColumnMap` (Go's structs.Map + sorted legs),
    // so the derived set is compared sorted too.
    expected.sort();
    assert_eq!(ddl.len(), 18, "the fresh playlist DDL column count");
    assert_eq!(
        expected.len(),
        15,
        "the 15 non-id keys the existing playlist_row test pins"
    );

    let zeroed = playlist_record();
    let hydrated = PlaylistRecord {
        image: resolved_playlist_image(),
        ..playlist_record()
    };

    let zero_map = playlist_repo::playlist_row(&zeroed).expect("non-smart row");
    let hydr_map = playlist_repo::playlist_row(&hydrated).expect("non-smart row");
    let keys: Vec<String> = zero_map.iter().map(|(c, _)| c.to_string()).collect();
    assert_eq!(
        keys, expected,
        "the key set is exactly the persisted columns"
    );
    assert_eq!(zero_map.len(), expected.len());
    for column in [
        "image_hash",
        "image_absent",
        "blur_hash",
        "thumb_hash",
        "dominant_color",
        "image_width",
        "image_height",
    ] {
        assert!(
            !zero_map.contains(column),
            "{column} must never be a write key"
        );
    }
    assert_eq!(
        keys,
        hydr_map
            .iter()
            .map(|(c, _)| c.to_string())
            .collect::<Vec<_>>(),
        "hydration adds no key"
    );

    // The INSERT leg is byte-identical with the image zeroed vs hydrated.
    let now = nd_db::time::parse(T1).expect("T1 parses");
    let zero_insert = playlist_repo::put_sql("pl-1", &zeroed, now, &[])
        .expect("renders")
        .insert;
    let hydr_insert = playlist_repo::put_sql("pl-1", &hydrated, now, &[])
        .expect("renders")
        .insert;
    assert_eq!(zero_insert.0, hydr_insert.0, "INSERT SQL unchanged");
    assert_eq!(zero_insert.1, hydr_insert.1, "INSERT args unchanged");

    // Smart arm: PostMapArgs deletes the three evaluation-owned counters
    // (`playlist_repository.go:43-45`), so 16 − 1(rules is set, not deleted) − 3.
    let mut fields = nd_model::Fields::new();
    fields.insert(
        "artist".to_string(),
        serde_json::Value::String("Frahm".to_string()),
    );
    let smart = PlaylistRecord {
        rules: Some(nd_model::Criteria {
            expression: nd_model::Expression::Is(fields),
            ..nd_model::Criteria::default()
        }),
        ..playlist_record()
    };
    let smart_map = playlist_repo::playlist_row(&smart).expect("smart row");
    let smart_keys: Vec<String> = smart_map.iter().map(|(c, _)| c.to_string()).collect();
    let mut expected_smart = expected.clone();
    for column in playlist_repo::SMART_OWNED_COUNTER_COLS {
        expected_smart.retain(|c| c != column);
    }
    expected_smart.push("rules".to_string());
    expected_smart.sort();
    assert_eq!(
        smart_keys, expected_smart,
        "smart key set derived, image never a key"
    );

    // Radio: still exactly the 7 persisted legs, in struct order.
    let radio_ddl = columns(&store, "radio");
    assert_eq!(
        radio_ddl.len(),
        7,
        "the radio table has 7 persisted columns"
    );
    let legs = radio_repo::put_cols(&RadioRecord {
        image: resolved_radio_image(),
        ..radio_record()
    });
    assert_eq!(legs.len(), 7, "put_cols is unchanged by the image field");
    let leg_names: Vec<&str> = legs.iter().map(|col| col.column.as_str()).collect();
    assert_eq!(
        leg_names,
        [
            "id",
            "stream_url",
            "name",
            "home_page_url",
            "uploaded_image",
            "created_at",
            "updated_at"
        ]
    );
    std::fs::remove_file(path).ok();
}

/// `annotation` and `owner_name` stay derived-only (`structs:"-"`), i.e. neither
/// appears as a write key, for either repository.
#[test]
fn annotation_and_owner_name_stay_derived_only() {
    let (store, path) = fresh_store();
    let annotation_columns = columns(&store, "annotation");
    let record = playlist_record();
    let map = playlist_repo::playlist_row(&record).expect("row");
    for column in &annotation_columns {
        assert!(
            !map.contains(column),
            "{column} is annotation state, never a playlist write key"
        );
    }
    assert!(!map.contains("owner_name"), "the alias is read-path only");

    let legs = radio_repo::put_cols(&radio_record());
    let leg_names: Vec<&str> = legs.iter().map(|col| col.column.as_str()).collect();
    for column in &annotation_columns {
        assert!(
            !leg_names.contains(&column.as_str()),
            "{column} is not a radio column"
        );
    }
    assert!(!leg_names.contains(&"owner_name"));
    std::fs::remove_file(path).ok();
}

/// Radio's NULL timestamps are a SCAN error (`model/radio.go:19-20` is a
/// non-pointer `time.Time`), NOT NotFound and not a zero value, and the artwork
/// layer never runs — proven by dropping `item_artwork`: if hydration had run,
/// the read would have come back `Ok` with a zero image and an error text.
#[test]
fn radio_null_timestamps_are_a_scan_error_not_not_found() {
    let (store, path) = fresh_store();
    seed_radio_null_times(&store, "rn");
    seed_radio_states(&store);
    lease(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", []).expect("drop")
    });

    let got = lease(&store, |conn| RadioRepo::new(conn).get("rn"));
    match got {
        Err(RadioError::Sqlite(err)) => {
            let message = err.to_string();
            assert!(
                message.contains("created_at"),
                "the failing column is named, as Go's wrapper names it: {message}"
            );
            assert!(
                message.contains("NULL"),
                "the failure is the NULL cell: {message}"
            );
        }
        other => panic!("expected a Sqlite scan error, got {other:?}"),
    }

    // GetAll over the same row: Go aborts rows.all and returns the error beside the
    // rows already appended (`radio_repository.go:71-73`; dbx appends after a
    // successful scan at `rows.go:192` and aborts at `:169-171`). Here the NULL row is
    // the only row, so Go's slice comes back empty — executed `len == 0`. With two good
    // rows ahead of it the same statement returns `len == 2` beside the error (the
    // parent's oracle and the reviewer's probe both executed that shape); the port has
    // no value-beside-error, so it reports only the error either way.
    let all = lease(&store, |conn| RadioRepo::new(conn).get_all(None));
    match all {
        Err(RadioError::Sqlite(err)) => {
            assert!(err.to_string().contains("created_at"), "{err}");
        }
        other => panic!("expected a Sqlite scan error from GetAll, got {other:?}"),
    }
    std::fs::remove_file(path).ok();
}

/// The visibility clause must restrict the requested id. Go builds
/// `And{Eq{"playlist.id": id}, userFilter(ctx)}` (`playlist_repository.go:149`) and
/// squirrel parenthesizes the `Or` conjunct (executed against squirrel v1.5.4:
/// `WHERE (playlist.id = ? AND (public = ? OR owner_id = ?))`). Executed against the
/// REAL Go repository over {adm-private, adm-public, lim-private} with the caller a
/// non-admin: `Get("pl-adm-private")` → `data not found` (nil pointer),
/// `Get("pl-adm-public")` → that row, `Get("pl-lim-private")` → that row,
/// `Get("pl-nobody")` → `data not found`. Unparenthesized, SQL binds `AND` tighter
/// than `OR` and every one of those four requests returns `lim-private`, so this
/// test pins both the NotFound arms and the exact row identity returned.
#[test]
fn playlist_get_visibility_clause_still_restricts_the_requested_id() {
    let (store, path) = fresh_store();
    seed_users(&store);
    seed_playlist(&store, "pl-adm-private", false, ADMIN);
    seed_playlist(&store, "pl-adm-public", true, ADMIN);
    seed_playlist(&store, "pl-lim-private", false, LIMITED);
    seed_playlist_states(&store);

    let lim = Some(LIMITED.to_string());
    for (id, expected) in [
        ("pl-adm-private", None),
        ("pl-adm-public", Some("pl-adm-public")),
        ("pl-lim-private", Some("pl-lim-private")),
        ("pl-nobody", None),
    ] {
        let got = lease(&store, |conn| {
            PlaylistRepo::new(conn).get(id, LIMITED, &lim)
        });
        match expected {
            Some(want) => {
                let record = got.unwrap_or_else(|e| panic!("Get({id}) as non-admin: {e}"));
                assert_eq!(
                    record.id, want,
                    "{id} returns its OWN row, not any visible row"
                );
            }
            None => assert!(
                matches!(got, Err(PlaylistError::NotFound)),
                "Get({id}) as non-admin must be NotFound, got {got:?}"
            ),
        }
    }

    // An admin sees the private row too, and still gets NotFound for a missing id
    // (`GetAll`/`Get` traced as `AND (1=1)` for the admin).
    let private = lease(&store, |conn| {
        PlaylistRepo::new(conn).get("pl-adm-private", ADMIN, &None)
    })
    .expect("admin sees every playlist");
    assert_eq!(private.id, "pl-adm-private");
    assert!(matches!(
        lease(&store, |conn| PlaylistRepo::new(conn).get(
            "pl-nobody",
            ADMIN,
            &None
        )),
        Err(PlaylistError::NotFound)
    ));

    // Bulk-read lengths, matching the Go oracle exactly: the non-admin sees 2 of 3.
    let as_lim = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_all(
            LIMITED,
            &lim,
            &[playlist_repo::natural_sort("playlist.name", false)],
        )
    })
    .expect("GetAll as non-admin");
    let ids: Vec<&str> = as_lim.items.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["pl-adm-public", "pl-lim-private"]);
    let as_admin = lease(&store, |conn| {
        PlaylistRepo::new(conn).get_all(
            ADMIN,
            &None,
            &[playlist_repo::natural_sort("playlist.name", false)],
        )
    })
    .expect("GetAll as admin");
    assert_eq!(as_admin.items.len(), 3, "the admin sees all three");
    std::fs::remove_file(path).ok();
}

/// Phase 1 alone leaves `image` zero, and neither read statement emits an artwork
/// column — so `row_to_playlist`/`row_to_radio` never scan one. Go's selects
/// provably do not either (`dbx` maps by the `db` tag / field name,
/// `dbx@v1.12.0/struct.go:48`, and no select lists an image column).
#[test]
fn phase_one_leaves_the_image_zero_and_no_select_emits_an_artwork_column() {
    let (store, path) = fresh_store();
    seed_users(&store);
    seed_playlist(&store, "pl-1", true, ADMIN);
    seed_playlist_states(&store);
    seed_radio(&store, "ra-1");
    seed_radio_states(&store);

    for column in [
        "image_hash",
        "image_absent",
        "blur_hash",
        "thumb_hash",
        "dominant_color",
        "image_width",
        "image_height",
    ] {
        let (sql, _) = playlist_repo::get_sql("pl-1", ADMIN, &None);
        assert!(!sql.contains(column), "get_sql must not project {column}");
        let (sql, _) = playlist_repo::get_all_sql(ADMIN, &None, &[]);
        assert!(
            !sql.contains(column),
            "get_all_sql must not project {column}"
        );
        let (sql, _) = playlist_repo::get_playlists_sql("m1", ADMIN, &None, "playlist.name");
        assert!(
            !sql.contains(column),
            "get_playlists_sql must not project {column}"
        );
    }
    let (radio_sql, _) = radio_repo::get_sql("ra-1");
    assert_eq!(
        radio_sql, "SELECT * FROM radio WHERE id = ?",
        "no artwork column"
    );

    // Reading through phase 1 only (no second layer) leaves every image zero even
    // though resolved state rows exist.
    let playlists = lease(&store, |conn| {
        let statement = playlist_repo::get_sql("pl-1", ADMIN, &None);
        nd_db::execution::query_all(conn, &statement, nd_db::hydrate::row_to_playlist)
            .expect("phase 1 read")
            .into_iter()
            .map(nd_db::playlist_repo::PlaylistRow::into_record)
            .collect::<Result<Vec<PlaylistRecord>, _>>()
            .expect("PostScan decodes")
    });
    assert_eq!(playlists.len(), 1);
    assert_eq!(
        playlists[0].rules, None,
        "PostScan ran and left a NULL rules as None"
    );
    assert_eq!(
        playlists[0].image,
        ItemImage::default(),
        "phase 1 never fills the image"
    );

    let radios = lease(&store, |conn| {
        let statement = radio_repo::get_sql("ra-1");
        nd_db::execution::query_all(conn, &statement, nd_db::hydrate::row_to_radio)
            .expect("radio read")
    });
    assert_eq!(radios.len(), 1);
    assert_eq!(
        radios[0].image,
        ItemImage::default(),
        "radio has no phase 2 at all"
    );
    std::fs::remove_file(path).ok();
}

/// Go's `GetAll` with `Offset` only (`Max == 0`) renders `SELECT * FROM radio
/// OFFSET 1` — `applyOptions` sets the two bounds independently
/// (`sql_base_repository.go:151-156`) — and SQLite rejects it: oracle-executed,
/// `err="near \"1\": syntax error"`, `len=0`. The port instead emits
/// `LIMIT -1 OFFSET 1` — the deliberate, already-cited deviation in
/// [`crate::sqlrepo`] (`sqlrepo.rs:313-319`: squirrel's bare `OFFSET` is valid on
/// PostgreSQL/MySQL but a parse error on SQLite, so the builder prepends SQLite's
/// unlimited bound). This pins the DIVERGENCE so a future change to either side is
/// visible: the port executes and skips the offset, Go fails the read.
#[test]
fn offset_without_limit_diverges_the_way_the_builder_documents() {
    let (store, path) = fresh_store();
    for id in ["ra-1", "ra-2"] {
        seed_radio(&store, id);
    }
    let offset_only = RestQueryOptions {
        max: 0,
        offset: 1,
        ..RestQueryOptions::default()
    };
    // The renderer takes the parser's output (see `get_all_sql`'s WARNING).
    let parsed = radio_repo::parse_rest_options(Some(&offset_only));
    let (sql, _) = radio_repo::get_all_sql(Some(&parsed)).expect("materializes");
    assert!(
        sql.contains("LIMIT -1 OFFSET 1"),
        "the builder's cited deviation: {sql}"
    );

    let got = lease(&store, |conn| {
        RadioRepo::new(conn).get_all(Some(&offset_only))
    });
    let outcome = match got {
        // The port executes where Go fails (see the doc comment above).
        Ok(outcome) => outcome,
        Err(err) => panic!("the port executes the offset-only page, unlike Go: {err}"),
    };
    assert_eq!(outcome.items.len(), 1, "the offset skips one row");
    assert_eq!(outcome.items[0].id, "ra-2");
    assert_eq!(outcome.artwork_error, None);
    std::fs::remove_file(path).ok();
}

/// A write record for the invariance assertions: every persisted column named,
/// non-zero where it matters.
fn playlist_record() -> PlaylistRecord {
    PlaylistRecord {
        id: "pl-1".to_string(),
        name: "Mix".to_string(),
        comment: "c".to_string(),
        duration: 3.0,
        size: 100,
        song_count: 2,
        owner_id: ADMIN.to_string(),
        public: true,
        path: "p".to_string(),
        sync: false,
        uploaded_image: "up".to_string(),
        external_image_url: "ext".to_string(),
        created_at: nd_db::time::parse(T0),
        updated_at: nd_db::time::parse(T1).expect("T1 parses"),
        imported_hash: "ih".to_string(),
        rules: None,
        evaluated_at: None,
        annotation: nd_model::annotation::Annotation::default(),
        image: ItemImage::default(),
        owner_name: ADMIN.to_string(),
    }
}

fn radio_record() -> RadioRecord {
    RadioRecord {
        id: "ra-1".to_string(),
        stream_url: "https://x/y".to_string(),
        name: "Radio One".to_string(),
        home_page_url: "https://x".to_string(),
        uploaded_image: "up".to_string(),
        created_at: nd_db::time::parse(T0).expect("T0 parses"),
        updated_at: nd_db::time::parse(T1).expect("T1 parses"),
        image: ItemImage::default(),
    }
}

/// Bind the `smart` filter's own clause and read the row set from the fresh
/// schema. Go builds `Or{Eq{"rules": ""}, Eq{"rules": nil}}`
/// (`playlist_repository.go:76-81`), and squirrel renders the nil leg as
/// `rules IS NULL` with no placeholder (`squirrel@v1.5.4/expr.go:182-183`, the
/// `if val == nil` arm; executed in `/tmp/sd3f_p1/args.go`: `(rules = ? OR rules IS NULL)`, one arg).
/// The port previously emitted `rules = ? OR rules = ?` with a NULL bind, and SQL
/// three-valued logic makes `rules = NULL` never true — so a playlist whose
/// `rules` cell is NULL, which is what a non-smart `Put` writes
/// (`playlist_repository.go:49` deletes the key), was silently excluded from a
/// filter Go includes it in. `seed_playlist` writes `rules = NULL` for every row,
/// so the NULL arm is the one this fixture exercises.
#[test]
fn smart_filter_clause_includes_a_null_rules_row() {
    let (store, path) = fresh_store();
    seed_users(&store);
    // rules = NULL (what a non-smart playlist persists) and rules = '' (an
    // explicitly emptied cell), both owned by the admin.
    seed_playlist(&store, "pl-null-rules", false, ADMIN);
    insert(
        &store,
        "INSERT INTO playlist (id, name, comment, duration, song_count, public, created_at, \
         updated_at, path, sync, size, rules, evaluated_at, owner_id, average_rating, \
         imported_hash, uploaded_image, external_image_url) \
         VALUES ('pl-empty-rules', 'pl-empty-rules', '', 1, 1, 0, ?, ?, '/p/e', 0, 1, '', \
         NULL, ?, 0, '', '', '')",
        &[v(T0), v(T1), v(ADMIN)],
    );

    let (clause, args) = playlist_repo::smart_playlist_filter();
    let sql = format!("SELECT playlist.id FROM playlist WHERE {clause}");
    let ids = lease(&store, |conn| {
        let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_else(|e| panic!("smart filter query: {e}"));
        rows.collect::<Result<Vec<String>, _>>()
            .unwrap_or_else(|e| panic!("rows: {e}"))
    });
    // SQLite row order is not contractual here (no ORDER BY), so compare sorted.
    let mut ids = ids;
    ids.sort();
    assert_eq!(
        ids,
        ["pl-empty-rules", "pl-null-rules"],
        "Go's smart filter includes BOTH the empty-string and the NULL rules row"
    );

    // The OLD text, kept here as the counterfactual: it drops the NULL row, which
    // is the bug this lane fixed. (It also needed two binds, so the arg count
    // itself was wrong against squirrel's one-arg render.)
    let old_sql = "SELECT playlist.id FROM playlist WHERE rules = ? OR rules = ?";
    let old_ids = lease(&store, |conn| {
        let mut stmt = conn.prepare(old_sql).unwrap();
        stmt.query_map(
            rusqlite::params_from_iter([Value::Text(String::new()), Value::Null].iter()),
            |r| r.get::<_, String>(0),
        )
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
    });
    assert_eq!(
        old_ids,
        ["pl-empty-rules"],
        "the pre-fix text matches only the empty-string row — the defect"
    );
    std::fs::remove_file(path).ok();
}

/// `playlistFilter` is `Or{substringFilter(name), substringFilter(comment)}`
/// (`playlist_repository.go:69-74`), and each `substringFilter`
/// (`sql_restful.go:119-126`) is an `And` over `strings.Fields`. Executed against
/// the pinned squirrel (`/tmp/sd3f_p1/q.go`), a two-word query renders
/// `((name LIKE ? AND name LIKE ?) OR (comment LIKE ? AND comment LIKE ?))`. The
/// port previously flattened all parts of both fields into one `OR`, matching a
/// row whose words are split across the two fields — something Go never matches.
#[test]
fn q_filter_requires_all_words_within_one_field_not_across_fields() {
    let (store, path) = fresh_store();
    seed_users(&store);
    // "rah" in the name, "frahm" in the comment: Go's per-field And rejects it.
    insert(
        &store,
        "INSERT INTO playlist (id, name, comment, duration, song_count, public, created_at, \
         updated_at, path, sync, size, rules, evaluated_at, owner_id, average_rating, \
         imported_hash, uploaded_image, external_image_url) \
         VALUES ('pl-split', 'alpha mix', 'zeta notes', '', 0, 1, ?, ?, '/p/s', 0, 0, \
         NULL, NULL, ?, 0, '', '', '')",
        &[v(T0), v(T1), v(ADMIN)],
    );
    // Both words in the name: Go matches.
    insert(
        &store,
        "INSERT INTO playlist (id, name, comment, duration, song_count, public, created_at, \
         updated_at, path, sync, size, rules, evaluated_at, owner_id, average_rating, \
         imported_hash, uploaded_image, external_image_url) \
         VALUES ('pl-both', 'alpha zeta mix', '', '', 0, 1, ?, ?, '/p/b', 0, 0, \
         NULL, NULL, ?, 0, '', '', '')",
        &[v(T0), v(T1), v(ADMIN)],
    );

    let (clause, args) = playlist_repo::playlist_filter("alpha zeta");
    let ids = lease(&store, |conn| {
        let sql = format!("SELECT playlist.id FROM playlist WHERE {clause}");
        let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
    });
    let mut ids = ids;
    ids.sort();
    assert_eq!(
        ids,
        ["pl-both"],
        "all words must land in the SAME field: `pl-split` has one word in each field"
    );

    // The flattened pre-fix text, as the counterfactual: it also hits pl-split.
    let flat = lease(&store, |conn| {
        let sql = "SELECT playlist.id FROM playlist WHERE playlist.name LIKE ? \
                   OR playlist.name LIKE ? OR playlist.comment LIKE ? OR playlist.comment LIKE ?";
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map(
            rusqlite::params_from_iter(
                [
                    Value::Text("%alpha%".to_string()),
                    Value::Text("%zeta%".to_string()),
                    Value::Text("%alpha%".to_string()),
                    Value::Text("%zeta%".to_string()),
                ]
                .iter(),
            ),
            |r| r.get::<_, String>(0),
        )
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
    });
    let mut flat = flat;
    flat.sort();
    assert_eq!(
        flat,
        ["pl-both", "pl-split"],
        "the pre-fix flattened OR matched the cross-field row — the defect"
    );

    // An empty query is NOT `1=0`: Go's Or legs are empty Ands, rendered `1=1` each,
    // so it matches every row (`/tmp/sd3f_p1/q.go`, `q empty`).
    let (empty_clause, empty_args) = playlist_repo::playlist_filter("");
    let all = lease(&store, |conn| {
        let sql = format!("SELECT playlist.id FROM playlist WHERE {empty_clause}");
        let mut stmt = conn.prepare(&sql).unwrap();
        stmt.query_map(rusqlite::params_from_iter(empty_args.iter()), |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
    });
    assert_eq!(all.len(), 2, "an empty q matches everything");
    std::fs::remove_file(path).ok();
}
