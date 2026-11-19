//! S-D3d golden — artist hydration + read record + repo read side EXECUTED
//! against a real SQLite DB: fresh schema via `nd_dbschema`, one isolated temp
//! file per test (harness shape copied from
//! `tests/runtime_hydrate_album_mediafile.rs:49-120`, artwork idioms from
//! `tests/runtime_artwork_hydration.rs:281-307`).
//!
//! Go truth under test: `dbArtist`/`PostScan`
//! (`persistence/artist_repository.go:32-98`), the stats select (`selectArtist`
//! `:207-215` → `applyLibraryFilterToArtistQuery` `:191-205` → `withAnnotation`
//! `sql_annotations.go:63-91`), `Get` (`:255-267`), `GetAll` (`:269-279`),
//! `hydrateArtwork` (`:294-297`) and the `ar` artwork kind
//! (`model/artwork_id.go:29`, bound as the PREFIX at `artwork_repository.go:169`).
//!
//! The claims this slice pins, each executed:
//! (1) `Stats` is created UNCONDITIONALLY (`:44`), so `""` yields an EMPTY map;
//! (2) `"total"` is added to the totals and NEVER becomes a stats key
//!     (`:63-72`: the `+=` runs, then the `continue` drops the key);
//! (3) an unknown role contributes NOTHING, to stats or to the totals
//!     (`:63-67` gated by `key == "total"`, `:69-72` `continue`);
//! (4) pre-seeded totals ACCUMULATE (`+=`, not an assignment), and an overflowing
//!     fold WRAPS instead of panicking — Go's `+=` has no overflow check, while a
//!     plain Rust `+=` panics under this workspace's profiles;
//! (5) Go's null tolerance at every nesting level, its zero defaults for a
//!     missing `a`/`s`, and its rejection of a non-integer stat value — all
//!     carrying the EXACT stats prefix including the trailing space;
//! (6) the stats loop runs BEFORE the similar-artists decode;
//! (6b) a non-string `id`/`name` in a similar element reports Go's FIELD-level
//!     sentence byte-for-byte (`… into Go struct field .0.id of type string`), and
//!     the null / missing-key / unknown-field / `[null,{…}]` arms stay error-free;
//! (7) annotation columns hydrate for a logged user, decode to zeros with no
//!     annotation row, and with `annotation_user = None` (Go's `invalidUserId`,
//!     `sql_annotations.go:63-67`) there is no join and `average_rating` still
//!     comes from `artist.*`;
//! (8) `GROUP BY artist.id` collapses multiple `library_artist` rows into one;
//! (9) `Get` reports `NotFound` for an unknown id, an ungranted non-admin, and
//!     an artist with NO `library_artist` row (the INNER join drops it — Go
//!     parity), while `GetAll` returns an EMPTY Vec and never `NotFound`;
//! (9b) `get_all_sql` renders filters and LIMIT/OFFSET but NO ORDER BY, so
//!     `QueryOptions.sort`/`order` are ignored — and the stats-backed sort keys
//!     reference the `artist.stats` column the fresh schema does not have;
//! (10) the `ar` artwork states — resolved / known-absent / unresolved — and
//!     Go's swallowed lookup failure;
//! (11) the write map is UNCHANGED by hydration: 18 keys with
//!     `sort_artist_name`/`mbz_artist_id` set, 16 without (17 counting the `id`
//!     key Go's `filterUpdateValues` adds, `sql_base_repository.go:612-616`), and
//!     none of the `structs:"-"` fields is a key.

use nd_db::artist_repo::{
    self, ArtistError, ArtistRecord, ArtistRepo, ArtistRow, ArtistStats, SimilarArtist,
};
use nd_db::artwork_hydration::ItemImage;
use nd_db::hydrate::{self, HydrateError, RowGet};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sql_participations::Role;
use nd_db::sql_restful::{Filter, QueryOptions};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;
use std::collections::BTreeMap;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
/// The `starred_at` cell: a stored offset the driver preserves, so the wire text
/// keeps `+03:00` rather than a UTC-normalised instant.
const STARRED_AT: &str = "2024-03-03 03:00:00+03:00";
/// The Go wire text of [`STARRED_AT`]: RFC3339Nano with the parsed zone.
const STARRED_AT_WIRE: &str = "2024-03-03T03:00:00+03:00";
/// The `ar` artwork kind prefix (`nd-id/src/artwork.rs:30-35`,
/// `model/artwork_id.go:29`).
const KIND_AR: &str = "ar";
/// The same kind's NAME, which must NOT match (`artwork_repository.go:169` binds
/// `ia.item_kind = kind.Prefix()`).
const KIND_NAME: &str = "artist";
/// The alias payload used by the decode-level tests: one library's entry, i.e.
/// what `JSON_GROUP_OBJECT` emits. The library key `1` is added BY the fold, not
/// stored in the `library_artist.stats` cell — see [`CELL_WITH_TOTAL`].
const STATS_WITH_TOTAL: &str =
    r#"{"1":{"albumartist":{"m":2,"a":3,"s":40},"total":{"m":1,"a":1,"s":10}}}"#;
/// The `library_artist.stats` CELL whose fold is [`STATS_WITH_TOTAL`] once
/// `JSON_GROUP_OBJECT(1, …)` wraps it in the library key.
const CELL_WITH_TOTAL: &str =
    r#"{"albumartist":{"m":2,"a":3,"s":40},"total":{"m":1,"a":1,"s":10}}"#;

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled so `SEEDEDRAND`/`NATSORT` are registered per connection.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-hydrate-artist-",
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

/// Run one closure on a leased connection (the repo calls return their own error
/// type rather than a raw `rusqlite::Error`). The connection is a reference only,
/// so nothing can outlive the lease.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
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

/// Phase 1 + phase 2 of the seam, surfaced the way Go's `ScanStruct` error
/// leaves the read (a sqlite conversion failure carrying the Go message).
fn hydrate_artist_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtistRecord> {
    hydrate::row_to_artist(row)
        .map_err(HydrateError::Sqlite)
        .and_then(ArtistRow::into_record)
        .map_err(HydrateError::into_sqlite)
}

/// A `library` row; `library_artist.library_id` references it. The `path` column
/// is UNIQUE, so each library gets its own.
fn seed_library(store: &SQLStore, id: i64) {
    insert(
        store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[
            i(id),
            v(&format!("lib{id}")),
            v(&format!("/music-{id}")),
            v(""),
        ],
    );
}

/// A NON-admin user (`is_admin = 0`), so the `user_library` join actually
/// narrows the artist selects (`artist_repository.go:199-202`).
fn seed_user(store: &SQLStore, id: &str) {
    insert(
        store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            v(id),
            v(id),
            v(id),
            v(&format!("{id}@x.y")),
            v("CIPHER"),
            i(0),
            v(T0),
            v(T1),
        ],
    );
}

/// `user_library` row granting `user_id` access to `lib_id`.
fn grant_library(store: &SQLStore, user_id: &str, lib_id: i64) {
    insert(
        store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[v(user_id), i(lib_id)],
    );
}

/// An `artist` row written cell-by-cell so a test can pin any column. `extra`
/// cells are appended to the column list.
fn insert_artist_raw(store: &SQLStore, id: &str, extra: &[(&str, &str)]) {
    let mut columns = vec![
        "id",
        "name",
        "order_artist_name",
        "created_at",
        "updated_at",
    ];
    let mut values: Vec<rusqlite::types::Value> = vec![
        v(id),
        v("Eddie Van Halen"),
        v("Eddie Van Halen"),
        v(T0),
        v(T1),
    ];
    for (column, value) in extra {
        columns.push(column);
        values.push(v(value));
    }
    let placeholders = vec!["?"; columns.len()].join(", ");
    insert(
        store,
        &format!(
            "INSERT INTO artist ({}) VALUES ({})",
            columns.join(", "),
            placeholders
        ),
        &values,
    );
}

/// The `library_artist` junction row carrying one library's stats. Go's scanner
/// writes this column (`RefreshStats` `:465-586`); the shape is documented at
/// `:208` — `{"1": {"albumartist": {"m": 10, "a": 5, "s": 1024}, "total": {…}}}`.
/// An EMPTY stats cell is deliberately not seeded anywhere: `JSONB('')` is
/// rejected by SQLite, which fails the STATEMENT rather than reaching PostScan
/// (executed below, and recorded as a residual).
fn seed_library_artist(store: &SQLStore, lib_id: i64, artist_id: &str, stats: &str) {
    insert(
        store,
        "INSERT INTO library_artist (library_id, artist_id, stats) VALUES (?, ?, ?)",
        &[i(lib_id), v(artist_id), v(stats)],
    );
}

/// The annotation row a logged-in user's LEFT JOIN picks up. `item_type` is the
/// repository's table name, bound by `withAnnotation` (`sql_annotations.go:71`).
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

/// `item_artwork` state row for one kind, copied from
/// `tests/runtime_artwork_hydration.rs:281-288`.
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
fn seed_image(store: &SQLStore, hash: &str, width: i64, height: i64, blur: &str, color: &str) {
    insert(
        store,
        "INSERT INTO artwork (hash, mime, width, height, size_bytes, blur_hash, thumb_hash, \
         dominant_color) VALUES (?, 'image/jpeg', ?, ?, 100, ?, 'th', ?)",
        &[v(hash), i(width), i(height), v(blur), v(color)],
    );
}

/// The Go-parity conversion-failure text of a failed row read.
fn go_error_text(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::FromSqlConversionFailure(_, _, msg) => msg.to_string(),
        other => panic!("expected a conversion failure, got {other:?}"),
    }
}

/// A failed seam read as text.
fn read_error(got: Result<ArtistRecord, rusqlite::Error>) -> String {
    let err = match got {
        Err(err) => err,
        Ok(record) => panic!("expected a read error, got {record:?}"),
    };
    go_error_text(&err)
}

/// The stats select narrowed to one id, run through the seam — the same statement
/// `ArtistRepo::get` executes, so a seam-level failure and a repo-level one
/// report the same thing.
fn hydrate_artist(
    store: &SQLStore,
    id: &str,
    annotation_user: Option<&str>,
) -> Result<ArtistRecord, rusqlite::Error> {
    let annotation = annotation_user.and_then(artist_repo::annotation_join);
    let statement = artist_repo::get_sql(id, &None, annotation.as_ref());
    query_all(store, &statement, hydrate_artist_row).map(|mut rows| rows.remove(0))
}

/// The stats payload under `library_stats_json` verbatim — the shape
/// `JSON_GROUP_OBJECT` produces. `artist_post_scan` is called directly for the
/// decode arms SQLite itself rejects at `JSONB()`.
fn stats_row(payload: &str, record: ArtistRecord) -> ArtistRow {
    ArtistRow {
        library_stats_json: payload.to_string(),
        record,
    }
}

/// A `QueryOptions` with pagination only (the struct has no `Default` derive;
/// sibling tests use `QueryOptions::default()`, which this crate does not offer).
fn page(max: i64, offset: i64) -> QueryOptions {
    QueryOptions {
        sort: String::new(),
        order: String::new(),
        max,
        offset,
        seed: None,
        filters: None,
    }
}

/// A `QueryOptions` carrying ONE `artist.name = ?` filter — the shape Go's
/// `selectArtist(ctx, options...)` hands to `applyFilters`
/// (`sql_base_repository.go:247-252`).
fn filtered(name: &str) -> QueryOptions {
    QueryOptions {
        filters: Some(vec![Filter::Eq {
            column: "artist.name".to_string(),
            value: Value::from(name),
        }]),
        ..page(0, 0)
    }
}

// ---------------------------------------------------------------------------
// PostScan's stats fold — executed through the REAL select (JSON_GROUP_OBJECT
// over library_artist.stats on the fresh schema), never a string-rendered SQL.
// ---------------------------------------------------------------------------

/// Multi-library fold: two `library_artist` rows for one artist. The per-role
/// entries MERGE across libraries (`:73-78` `+=`) and each library's `"total"`
/// entry SUMS onto the artist's own fields (`:63-67`). Executed on the alias the
/// real `JSON_GROUP_OBJECT` builds.
#[test]
fn multi_library_stats_fold_sums_totals_and_merges_roles_across_libraries() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_library(&store, 2);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-1",
        r#"{"albumartist":{"m":3,"a":2,"s":30},"total":{"m":5,"a":4,"s":50}}"#,
    );
    seed_library_artist(
        &store,
        2,
        "ar-1",
        r#"{"composer":{"m":1,"a":1,"s":10},"albumartist":{"m":4,"a":1,"s":40},"total":{"m":2,"a":2,"s":20}}"#,
    );

    let got = hydrate_artist(&store, "ar-1", None).expect("hydrates two libraries");
    assert_eq!(
        got.stats,
        BTreeMap::from([
            (
                Role::from("albumartist"),
                ArtistStats {
                    song_count: 7,
                    album_count: 3,
                    size: 70
                }
            ),
            (
                Role::from("composer"),
                ArtistStats {
                    song_count: 1,
                    album_count: 1,
                    size: 10
                }
            ),
        ]),
        "the same role merges across libraries, distinct roles coexist"
    );
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (70, 7, 6),
        "both libraries' total entries sum onto the artist's own fields"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `"total"` NEVER lands in `stats`: the `+=` at `:63-67` runs first, then the
/// `continue` at `:69-72` drops the key, because `RoleFromString("total")` is
/// invalid (`model/participants.go:71-76`). Also executes the brief's oracle:
/// an unknown role plus a `total` ⇒ ONLY the total lands.
#[test]
fn total_is_added_to_totals_but_never_becomes_a_stats_key() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-1",
        r#"{"albumartist":{"m":9,"a":8,"s":7},"total":{"m":1,"a":2,"s":3}}"#,
    );

    let got = hydrate_artist(&store, "ar-1", None).expect("hydrates");
    assert_eq!(
        got.stats.keys().collect::<Vec<&Role>>(),
        vec![&Role::from("albumartist")],
        "exactly one stats key, and it is not `total`"
    );
    assert!(
        !got.stats.contains_key(&Role::from("total")),
        "`total` is dropped AFTER the += (`:63-72`), never stored"
    );
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (3, 1, 2),
        "the total entry lands on the artist's own fields"
    );

    // The brief's executed oracle fact: bogusrole contributes nothing, the
    // total lands. Verified against a verbatim Go PostScan before porting.
    insert_artist_raw(&store, "ar-2", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-2",
        r#"{"bogusrole":{"m":9,"a":9,"s":9},"total":{"m":1,"a":1,"s":1}}"#,
    );
    let got = hydrate_artist(&store, "ar-2", None).expect("hydrates");
    assert_eq!(got.stats, BTreeMap::new(), "an unknown role adds nothing");
    assert_eq!((got.size, got.song_count, got.album_count), (1, 1, 1));

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// An unknown role contributes NOTHING at all — neither to `stats` nor to the
/// totals — because the `+=` block is gated by `key == "total"` and the role
/// parse `continue`s. The key differs from a real role only in case: Go's
/// `RoleFromString` is exact-match (`model/participants.go:71-76`).
#[test]
fn unknown_role_contributes_nothing_to_stats_or_totals() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-1",
        r#"{"AlbumArtist":{"m":9,"a":9,"s":9},"bogus":{"m":5,"a":5,"s":5}}"#,
    );
    let got = hydrate_artist(&store, "ar-1", None).expect("the read succeeds");
    assert_eq!(got.stats, BTreeMap::new());
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (0, 0, 0),
        "nothing accumulates, so the discarded `invalid role:` text has no effect"
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Pre-seeded fields ACCUMULATE: `PostScan` uses `+=` on the artist's own
/// `Size`/`SongCount`/`AlbumCount`, so a row carrying 100/10/5 ends at 101/11/6
/// after a `total` of 1/1/1 — not overwritten. Phase 2 is called directly here
/// because no SELECT can pre-seed those `structs:"-"` fields.
#[test]
fn pre_seeded_totals_accumulate_and_are_not_overwritten() {
    let row = stats_row(
        r#"{"1":{"total":{"m":1,"a":1,"s":1}}}"#,
        ArtistRecord {
            size: 100,
            song_count: 10,
            album_count: 5,
            ..ArtistRecord::default()
        },
    );
    let got = hydrate::artist_post_scan(row).expect("decodes");
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (101, 11, 6),
        "`+=` onto the pre-seeded values (`:64-66`)"
    );

    // Per-role entries accumulate too: the same role across two libraries
    // (`:74-78` reads the current entry and adds, never replaces).
    let row = stats_row(
        r#"{"1":{"albumartist":{"m":4,"a":4,"s":40}},"2":{"albumartist":{"m":6,"a":6,"s":60}}}"#,
        ArtistRecord::default(),
    );
    let got = hydrate::artist_post_scan(row).expect("decodes");
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 10,
            album_count: 10,
            size: 100
        }),
        "the same role across libraries accumulates"
    );
}
/// Go's `+=` on `int64`/`int` has no overflow check (`artist_repository.go:64-66`
/// and `:74-76`): an overflowing fold leaves the WRAPPED sum in the field
/// (executed Go 1.27.4: `2147483647 += 2147483647` → `4294967294`, and
/// `int64MAX += 1` → `int64MIN`). A plain Rust `+=` PANICS instead — overflow
/// checks are on in this workspace (`rust/Cargo.toml`'s `[profile.dev]` and
/// `[profile.test]` set only `debug`/`incremental`, so nothing disables them;
/// executed probe built with those exact profiles: `attempt to add with
/// overflow` at `i64::MAX += 1`). The fold therefore adds through
/// `wrapping_add` (`hydrate.rs::wrapping_int64`, `hydrate.rs::narrow_i32_go`).
#[test]
fn total_arm_accumulation_wraps_instead_of_panicking() {
    // `size` is the port's `i64`, exactly Go's accumulation width, so a wrapped
    // sum lands in the field with no clamping step in between.
    let got = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"total":{"s":1}}}"#,
        ArtistRecord {
            size: i64::MAX,
            ..ArtistRecord::default()
        },
    ))
    .expect("an overflowing size fold must not panic");
    assert_eq!(got.size, i64::MIN, "MAX + 1 wraps to MIN, Go parity");

    // Two libraries' `total` entries are two wraps of the same field.
    let got = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"total":{"s":9223372036854775807}},"2":{"total":{"s":9223372036854775807}}}"#,
        ArtistRecord::default(),
    ))
    .expect("two huge totals must not panic");
    assert_eq!(got.size, -2, "MAX + MAX wraps to -2");

    // `song_count`/`album_count` are the port's `i32` while Go's `int` is 64-bit
    // (`model/artist.go:29-30`), so the interplay of the wrap with the port-wide
    // narrowing is pinned for each direction. The clamp is applied to the SUM of
    // each add — the field itself is `i32`, so it cannot carry Go's raw 64-bit
    // wrapped value:
    //  * MAX from a zero field has not wrapped, so it saturates at `i32::MAX`;
    //  * a second MAX wraps the WIDENED sum negative (`i32::MAX as i64 + MAX` →
    //    `-9223372034707292162`, executed) and the clamped field lands on
    //    `i32::MIN`, where Go's 64-bit `int` field holds `-2` (executed: its steps
    //    are `MAX` then `MAX + MAX` → `-2`). That gap is the `i32` narrowing, NOT
    //    the accumulation: `size`, which is `i64` like Go's, agrees with Go exactly
    //    (`-2` below, and Go's `Size` field prints `-2` on the same two adds).
    //  * a pre-seeded `i32::MAX` plus MAX wraps negative the same way.
    let huge = r#"{"1":{"total":{"m":9223372036854775807,"a":9223372036854775807}}}"#;
    let one = hydrate::artist_post_scan(stats_row(huge, ArtistRecord::default()))
        .expect("a huge total must not panic");
    assert_eq!(
        (one.song_count, one.album_count),
        (i32::MAX, i32::MAX),
        "no wrap happened, so the sum saturates at the field's bound"
    );
    let twice = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"total":{"m":9223372036854775807,"a":9223372036854775807,"s":9223372036854775807}},"2":{"total":{"m":9223372036854775807,"a":9223372036854775807,"s":9223372036854775807}}}"#,
        ArtistRecord::default(),
    ))
    .expect("two huge totals must not panic");
    assert_eq!(
        (twice.song_count, twice.album_count),
        (i32::MIN, i32::MIN),
        "the wrapped sum is clamped to the i32 field's bound"
    );
    assert_eq!(
        twice.size, -2,
        "the i64 field carries Go's raw wrapped sum exactly (Go prints -2 too)"
    );
    let pre_seeded = hydrate::artist_post_scan(stats_row(
        huge,
        ArtistRecord {
            song_count: i32::MAX,
            album_count: i32::MAX,
            ..ArtistRecord::default()
        },
    ))
    .expect("a pre-seeded i32::MAX plus MAX must not panic");
    assert_eq!(
        (pre_seeded.song_count, pre_seeded.album_count),
        (i32::MIN, i32::MIN),
        "i32::MAX widened to i64 + MAX wraps negative and is clamped"
    );
}

/// The per-role arm (`artist_repository.go:74-76`) wraps exactly like the total
/// arm. The entry starts at Go's zero-valued `ArtistStats` (`:56-60` zero-fills a
/// missing key) and every later same-role entry ADDS to it, so a second
/// `i64::MAX` size overflows the entry — not the artist's own fields, which the
/// `"total"` arm alone owns (`:63-67`).
#[test]
fn per_role_stats_size_wraps_instead_of_panicking() {
    let payload = r#"{"1":{"albumartist":{"m":9223372036854775807,"a":9223372036854775807,"s":9223372036854775807},"total":{"m":1,"a":1,"s":1}},"2":{"albumartist":{"m":1,"a":1,"s":1}}}"#;
    let got = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
        .expect("a per-role overflow must not panic");
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: i32::MAX,
            album_count: i32::MAX,
            size: i64::MIN,
        }),
        "size: MAX + 1 wraps to MIN; the i32 counts saturate at MAX"
    );
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (1, 1, 1),
        "the per-role entry contributes nothing to the totals"
    );

    // The same fold executed through the REAL seam: two junction cells each carry
    // an `i64::MAX` role size, so `JSON_GROUP_OBJECT` hands PostScan two huge
    // sizes for one role and the statement still succeeds.
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_library(&store, 2);
    insert_artist_raw(&store, "ar-wrap", &[]);
    let huge_role = r#"{"albumartist":{"m":1,"a":1,"s":9223372036854775807}}"#;
    seed_library_artist(&store, 1, "ar-wrap", huge_role);
    seed_library_artist(&store, 2, "ar-wrap", huge_role);
    let got = hydrate_artist(&store, "ar-wrap", None).expect("the wrapped fold executes");
    assert_eq!(
        got.size, 0,
        "no `total` entry, so the artist's size is untouched"
    );
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 2,
            album_count: 2,
            size: -2,
        }),
        "the executed alias fold wraps MAX + MAX to -2"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The guard and Go's decode arms, executed against the REAL schema where the
/// payload can be stored: `""` takes the skip branch and leaves an EMPTY (made at
/// `:44`) map; `null`, `{"1":null}`, `{"1":{"role":null}}`, `{}` and a NULL
/// `library_artist.stats` cell all decode with no error. The rejection arms —
/// which SQLite's `JSONB()` refuses to accept as a stored cell — are asserted on
/// the decode seam with the exact payload PostScan would receive.
#[test]
fn stats_guard_and_decode_arms_match_go() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    // IMPORTANT: the `library_artist.stats` CELL holds ONE library's role map —
    // the library key is added by `JSON_GROUP_OBJECT(library_artist.library_id,
    // JSONB(library_artist.stats))`, not stored in the cell. So a test that wants
    // the alias `{"1":null}` seeds a cell that folds to it, and so on.
    insert_artist_raw(&store, "ar-braces", &[]);
    seed_library_artist(&store, 1, "ar-braces", "{}");
    insert_artist_raw(&store, "ar-role-null", &[]);
    seed_library_artist(&store, 1, "ar-role-null", r#"{"albumartist":null}"#);
    insert_artist_raw(&store, "ar-lib-null", &[]);
    insert(
        &store,
        "INSERT INTO library_artist (library_id, artist_id, stats) VALUES (?, ?, NULL)",
        &[i(1), v("ar-lib-null")],
    );
    // Executed on the bundled build: `JSON_GROUP_OBJECT(1, JSONB(NULL))` emits
    // `{"1":null}` — the entry IS present, holding a null, which Go's decoder
    // zero-fills with `err == nil`. (Go 3.53.4/go-sqlite3 reportedly drops the
    // entry; recorded as a residual.) So a NULL cell is NOT the empty string.
    let alias_of = |id: &str| {
        on(&store, |conn| {
            nd_db::execution::query_all(conn, &artist_repo::get_sql(id, &None, None), |row| {
                row.get_opt_text("library_stats_json")
            })
        })
        .unwrap_or_else(|e| panic!("{id} fold must execute: {e}"))
        .remove(0)
        .unwrap_or_else(|| panic!("{id} alias must not be NULL"))
    };
    assert_eq!(alias_of("ar-lib-null"), r#"{"1":null}"#);
    assert_eq!(alias_of("ar-braces"), r#"{"1":{}}"#);
    assert_eq!(alias_of("ar-role-null"), r#"{"1":{"albumartist":null}}"#);

    // The decode arms that CANNOT be stored as a cell (`JSONB` rejects them) go
    // through the seam with the alias payload verbatim.
    // `""` — the skip branch: the map is EMPTY because `make` at `:44` ran, and
    // the totals stay at their zero values.
    let got =
        hydrate::artist_post_scan(stats_row("", ArtistRecord::default())).expect("skip branch");
    assert!(got.stats.is_empty());
    assert_eq!((got.size, got.song_count, got.album_count), (0, 0, 0));
    // `null` — Go leaves the map nil and the `range` does nothing.
    let got = hydrate::artist_post_scan(stats_row("null", ArtistRecord::default())).expect("null");
    assert!(got.stats.is_empty());

    // Executed through the REAL fold: a null entry per library, and a `{}` cell.
    // A null role value still creates the map entry (`:74-78`), because the role
    // key parses and its zero-valued stats are `+=`-ed onto a fresh entry.
    let got = hydrate_artist(&store, "ar-role-null", None).expect("decodes");
    assert_eq!(
        got.stats,
        BTreeMap::from([(Role::from("albumartist"), ArtistStats::default())]),
        "a null role value is a zero-valued entry, not an absent one (`:56-60`)"
    );
    assert_eq!((got.size, got.song_count, got.album_count), (0, 0, 0));

    for id in ["ar-lib-null", "ar-braces"] {
        let got = hydrate_artist(&store, id, None)
            .unwrap_or_else(|e| panic!("{id} must decode with no error: {e}"));
        assert!(got.stats.is_empty(), "{id} contributes no stats entries");
        assert_eq!(
            (got.size, got.song_count, got.album_count),
            (0, 0, 0),
            "{id}"
        );
    }

    // The rejection arms, with Go's EXACT prefix (trailing space included).
    // The value-level arms reproduce Go's `cannot unmarshal …` wording. A PARSE
    // failure (`{`) reproduces only the PREFIX: the inner text is serde_json's
    // `EOF while parsing an object at line 1 column 1`, not Go's
    // `unexpected end of JSON input`. Recorded as a residual — this is the
    // port-wide pattern for every shim, not something this slice claims parity on.
    let parse_failure = hydrate::artist_post_scan(stats_row("{", ArtistRecord::default()))
        .expect_err("a truncated object must fail");
    let text = parse_failure.to_string();
    assert!(
        text.starts_with(hydrate::ARTIST_STATS_PREFIX),
        "prefix only, inner text is serde's: {text}"
    );
    assert!(
        text.contains("EOF while parsing"),
        "serde wording pinned: {text}"
    );

    let malformed_cases: &[(&str, &str)] = &[
        ("[]", "cannot unmarshal array into Go value of type"),
        (
            r#""x""#,
            "cannot unmarshal string into Go value of type map[string]map[string]map[string]int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":"x"}}}"#,
            "cannot unmarshal string into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":true}}}"#,
            "cannot unmarshal bool into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":1.5}}}"#,
            "cannot unmarshal number 1.5 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            // Go accepts only an integer SPELLING into int64: `1.0` is rejected
            // even though it is mathematically integral (executed oracle).
            r#"{"1":{"albumartist":{"m":1.0}}}"#,
            "cannot unmarshal number 1.0 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":5}}"#,
            "cannot unmarshal number into Go struct field .1.albumartist of type map[string]int64",
        ),
        (
            r#"{"1":[]}"#,
            "cannot unmarshal array into .1 of type map[string]map[string]int64",
        ),
        (
            // Go decides the `intoWhat` phrase POSITIONALLY from the last path
            // token, not from the target type (`encoding/json/v2_decode.go:143-147`).
            // The SAME library-level arm therefore reads both ways: a digit-only
            // library id drops the phrase, a non-digit one keeps it. Each side is
            // pinned byte-for-byte, prefix included, by
            // `digit_path_tokens_drop_the_go_struct_field_phrase` below.
            r#"{"1":5}"#,
            "cannot unmarshal number into .1 of type map[string]map[string]int64",
        ),
        (
            r#"{"alpha":5}"#,
            "cannot unmarshal number into Go struct field .alpha of type map[string]map[string]int64",
        ),
        (
            r#"{"1":{"2":5}}"#,
            "cannot unmarshal number into .1.2 of type map[string]int64",
        ),
        (
            r#"{"1":{"albumartist":{"5":true}}}"#,
            "cannot unmarshal bool into .1.albumartist.5 of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m2":true}}}"#,
            "cannot unmarshal bool into Go struct field .1.albumartist.m2 of type int64",
        ),
    ];
    for (payload, inner) in malformed_cases {
        let err = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
            .err()
            .unwrap_or_else(|| panic!("{payload} must fail"));
        let text = err.to_string();
        assert!(
            text.starts_with(hydrate::ARTIST_STATS_PREFIX),
            "Go's exact prefix (with the trailing space) on {payload}: {text}"
        );
        assert!(text.contains(inner), "inner text for {payload}: {text}");
        // The prefix is reproduced byte-for-byte; the inner scanner wording is
        // not Go's verbatim text for every arm (recorded residual).
        assert!(
            text.starts_with("parsing artist stats from db: "),
            "prefix spelled literally: {text}"
        );
    }

    // A malformed `library_artist.stats` CELL is rejected by SQLite's JSON
    // functions on BOTH pinned builds, so the read fails inside the STATEMENT —
    // Go's scanner would hand PostScan the malformed string instead. Recorded as
    // a divergence; no Rust pre-filter is added.
    insert_artist_raw(&store, "ar-bad-stats-cell", &[]);
    seed_library_artist(&store, 1, "ar-bad-stats-cell", "{");
    let err = on(&store, |conn| {
        nd_db::execution::query_all(
            conn,
            &artist_repo::get_sql("ar-bad-stats-cell", &None, None),
            |row| row.get_text("id"),
        )
    });
    assert!(
        err.is_err(),
        "JSONB('{{') fails the statement rather than handing PostScan a malformed string"
    );
    // `''` fails the same way (executed on both builds, see the residual note).
    insert_artist_raw(&store, "ar-empty-stats-cell", &[]);
    seed_library_artist(&store, 1, "ar-empty-stats-cell", "");
    let err = on(&store, |conn| {
        nd_db::execution::query_all(
            conn,
            &artist_repo::get_sql("ar-empty-stats-cell", &None, None),
            |row| row.get_text("id"),
        )
    });
    assert!(
        err.is_err(),
        "JSONB('') is a hard read failure on both builds"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Go's `UnmarshalTypeError.Error()` decides the `Go struct field ` phrase with a
/// POSITIONAL heuristic, not by the target type: it takes the token after the LAST
/// `.` of `Field` and drops the phrase when that token is non-empty and made
/// entirely of ASCII digits (`encoding/json/v2_decode.go:143-147`, GOROOT 1.27.4).
/// A digit-only key therefore reads like an array index at EVERY level, and the
/// sentence shape is byte-exact on both sides of the rule — the only remaining
/// value-level divergence is the number LITERAL rendering (serde prints the parsed
/// `Number`, Go echoes the raw literal), which the last table below pins.
///
/// Every `want` here is Go 1.27.4's own output, executed against
/// `map[string]map[string]map[string]int64` — not port output.
#[test]
fn digit_path_tokens_drop_the_go_struct_field_phrase() {
    // Phrase DROPPED: the last path token is all-digits, at the library, role and
    // stat-key level respectively.
    let dropped: &[(&str, &str)] = &[
        (
            r#"{"1":5}"#,
            "json: cannot unmarshal number into .1 of type map[string]map[string]int64",
        ),
        (
            // `01` counts as all-digits: Go `TrimRight`s a cutset rather than
            // parsing a number, so a zero-padded library id reads as an index.
            r#"{"01":5}"#,
            "json: cannot unmarshal number into .01 of type map[string]map[string]int64",
        ),
        (
            r#"{"1":{"2":5}}"#,
            "json: cannot unmarshal number into .1.2 of type map[string]int64",
        ),
        (
            r#"{"1":{"albumartist":{"5":true}}}"#,
            "json: cannot unmarshal bool into .1.albumartist.5 of type int64",
        ),
        (
            // A digit token drops the phrase on the SPELLED-literal arm too, and
            // `1.5` is a literal serde renders identically to Go.
            r#"{"1":{"albumartist":{"5":1.5}}}"#,
            "json: cannot unmarshal number 1.5 into .1.albumartist.5 of type int64",
        ),
        (
            // An integer above `i64::MAX` but inside `u64` keeps its plain digits
            // in BOTH renderings, so this sentence is byte-exact end to end.
            r#"{"1":{"albumartist":{"5":12345678901234567890}}}"#,
            "json: cannot unmarshal number 12345678901234567890 into .1.albumartist.5 of type int64",
        ),
    ];
    // Phrase KEPT: the LAST token is non-digit. Go looks only at that token, so
    // `.1.alpha` keeps the phrase even though the library id is a digit.
    let kept: &[(&str, &str)] = &[
        (
            r#"{"alpha":5}"#,
            "json: cannot unmarshal number into Go struct field .alpha of type map[string]map[string]int64",
        ),
        (
            r#"{"alpha":[1]}"#,
            "json: cannot unmarshal array into Go struct field .alpha of type map[string]map[string]int64",
        ),
        (
            r#"{"1":{"alpha":5}}"#,
            "json: cannot unmarshal number into Go struct field .1.alpha of type map[string]int64",
        ),
        (
            r#"{"1":{"albumartist":{"m2":true}}}"#,
            "json: cannot unmarshal bool into Go struct field .1.albumartist.m2 of type int64",
        ),
        (
            // Every token non-digit.
            r#"{"alpha":{"albumartist":{"m":"x"}}}"#,
            "json: cannot unmarshal string into Go struct field .alpha.albumartist.m of type int64",
        ),
        (
            r#"{"alpha":{"beta":{"gamma":true}}}"#,
            "json: cannot unmarshal bool into Go struct field .alpha.beta.gamma of type int64",
        ),
        (
            r#"{"1":{"albumartist":[1]}}"#,
            "json: cannot unmarshal array into Go struct field .1.albumartist of type map[string]int64",
        ),
        (
            // A digit library id with a non-digit role: last token wins.
            r#"{"1":{"albumartist":{"m":"x"}}}"#,
            "json: cannot unmarshal string into Go struct field .1.albumartist.m of type int64",
        ),
    ];
    for (payload, want) in dropped.iter().chain(kept.iter()) {
        let err = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
            .err()
            .unwrap_or_else(|| panic!("{payload} must fail the stats decode"));
        assert_eq!(
            err.to_string(),
            format!("{}{want}", hydrate::ARTIST_STATS_PREFIX),
            "Go's exact sentence for {payload}"
        );
    }

    // The number-literal residual, pinned on BOTH sides so it cannot silently
    // widen. Executed: Go 1.27.4 echoes the raw literal; serde_json 1.0.154
    // renders its parsed `Number`. The sentences below are the ones where the two
    // agree (plain decimal spellings, including a u64-sized integer), so they are
    // asserted byte-for-byte against Go.
    let literal_agrees: &[(&str, &str)] = &[
        (
            r#"{"1":{"albumartist":{"m":1.0}}}"#,
            "json: cannot unmarshal number 1.0 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":1.5}}}"#,
            "json: cannot unmarshal number 1.5 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":12345678901234567890}}}"#,
            "json: cannot unmarshal number 12345678901234567890 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            r#"{"1":{"albumartist":{"m":9223372036854775808}}}"#,
            "json: cannot unmarshal number 9223372036854775808 into Go struct field .1.albumartist.m of type int64",
        ),
        (
            // A signed-zero spelling WITH a fraction is rejected identically on both
            // sides, and serde's rendering happens to echo Go's bytes (executed).
            r#"{"1":{"albumartist":{"m":-0.0}}}"#,
            "json: cannot unmarshal number -0.0 into Go struct field .1.albumartist.m of type int64",
        ),
    ];
    for (payload, want) in literal_agrees {
        let err = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
            .err()
            .unwrap_or_else(|| panic!("{payload} must fail the stats decode"));
        assert_eq!(
            err.to_string(),
            format!("{}{want}", hydrate::ARTIST_STATS_PREFIX),
            "literal rendering agrees for {payload}"
        );
    }

    // Where they DISAGREE, pin the port's own rendering so the divergence stays
    // visible and bounded: exponent spellings and integers above `u64::MAX`.
    // Go says `number 1e3` / `number -9223372036854775809` /
    // `number 18446744073709551616`; serde says `number 1000.0`,
    // `number -9.223372036854776e+18` and `number 1.8446744073709552e+19`.
    let literal_diverges: &[(&str, &str)] = &[
        (r#"{"1":{"albumartist":{"m":1e3}}}"#, "number 1000.0"),
        (r#"{"1":{"albumartist":{"m":1E3}}}"#, "number 1000.0"),
        (r#"{"1":{"albumartist":{"m":1e9}}}"#, "number 1000000000.0"),
        (r#"{"1":{"albumartist":{"m":-1e3}}}"#, "number -1000.0"),
        (
            r#"{"1":{"albumartist":{"m":-9223372036854775809}}}"#,
            "number -9.223372036854776e+18",
        ),
        (
            r#"{"1":{"albumartist":{"m":18446744073709551616}}}"#,
            "number 1.8446744073709552e+19",
        ),
    ];
    for (payload, serde_spelling) in literal_diverges {
        let err = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
            .err()
            .unwrap_or_else(|| panic!("{payload} must fail the stats decode"));
        // Everything but the literal is Go's sentence verbatim: the phrase stays
        // (`m` is non-digit), the path and the type are Go's. Only the number's
        // inner spelling is serde's.
        assert_eq!(
            err.to_string(),
            format!(
                "{}json: cannot unmarshal {serde_spelling} into Go struct field \
                 .1.albumartist.m of type int64",
                hydrate::ARTIST_STATS_PREFIX
            ),
            "shape byte-exact, literal is serde's, for {payload}"
        );
    }

    // A divergence class found while pinning the literals, and it is BEHAVIORAL,
    // not wording: Go accepts the signed-zero INTEGER spelling `-0` into int64
    // (executed: `{"1":{"albumartist":{"m":-0}}}` → `err == nil`, value 0), but
    // serde_json 1.0.154 stores `-0` as a float to preserve the sign, so
    // `as_i64` is `None` (executed probe) and the port REJECTS it. Plain `0`
    // decodes cleanly on both sides. Recorded as a residual; pinned here so it
    // cannot widen unnoticed.
    let got = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"albumartist":{"m":0}}}"#,
        ArtistRecord::default(),
    ))
    .expect("plain 0 decodes on both sides");
    assert_eq!(
        got.stats,
        BTreeMap::from([(Role::from("albumartist"), ArtistStats::default())]),
    );
    let err = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"albumartist":{"m":-0}}}"#,
        ArtistRecord::default(),
    ))
    .expect_err("DIVERGENCE: the port rejects `-0` where Go reports no error");
    assert_eq!(
        err.to_string(),
        format!(
            "{}json: cannot unmarshal number -0.0 into Go struct field .1.albumartist.m of type int64",
            hydrate::ARTIST_STATS_PREFIX
        ),
        "serde's float rendering of `-0`, where Go errors never fire"
    );
}

/// Round 4 residual, pinned so it cannot widen: two decode-arm divergences of
/// `decode_library_stats` whose GO strings were executed against Go 1.27.4
/// (`encoding/json` into `map[string]map[string]map[string]int64`, the same target
/// `dbArtist.PostScan` decodes at `persistence/artist_repository.go:47`) and whose
/// PORT strings were captured from this exact seam
/// (`hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))`).
///
/// (1) A DUPLICATE key inside one stats cell: Go keeps the FIRST value and errors
/// (`{"1":{"albumartist":{"m":true,"m":1}}}` →
/// `json: cannot unmarshal bool into Go struct field .1.albumartist.m of type int64`,
/// executed); `serde_json` 1.0.154 collapses the duplicate to the LAST one (executed
/// probe: `{"m":true,"m":1}` parses to the single entry `m: 1`), so the port ACCEPTS
/// with `m = 1` and reports no error. Reachable only through a hand-edited or
/// corrupted `library_artist.stats` cell (arbitrary TEXT, `schema_ddl.rs:62`) —
/// Navidrome's own SQL writer emits unique role keys.
///
/// (2) WHICH entry a multi-bad cell names: Go reports the first offending entry in
/// SOURCE (document) order (executed: `{"b":{"y":true},"a":{"x":true}}` →
/// `json: cannot unmarshal bool into Go struct field .b.y of type map[string]int64`),
/// while `decode_library_stats` iterates serde's `BTreeMap` and so reports the first in
/// SORTED key order (executed on this seam: `.a.x`). Both sides reject; only the
/// sentence's path differs. Same root cause as the similar-artists decoder's residual
/// (the B-tree walk).
///
/// Recorded, not closed: changing either arm is a decode change this slice does not
/// own. See the S-D3d residual list in `docs/rust-port/hydrate-contract.md`.
#[test]
fn duplicate_keys_and_sorted_error_order_diverge_from_go() {
    // (1) The duplicate-key ACCEPT: Go errors on this cell (string executed above),
    // the port folds it to the LAST value and succeeds.
    let got = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"albumartist":{"m":true,"m":1}}}"#,
        ArtistRecord::default(),
    ))
    .expect("DIVERGENCE: the port accepts a cell Go rejects (last duplicate wins)");
    assert_eq!(
        got.stats,
        BTreeMap::from([(
            Role::from("albumartist"),
            ArtistStats {
                song_count: 1,
                album_count: 0,
                size: 0
            }
        )]),
        "the collapsed last duplicate (`m: 1`) folds into the record"
    );
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (0, 0, 0),
        "no `total` entry, so the artist's own fields are untouched"
    );
    // The same shape at the role level: the port ACCEPTS the duplicated role key
    // because the LAST value is valid (`m: 1`); the swapped spelling (last value
    // a bool) errors at the seam with `json: cannot unmarshal bool into Go struct
    // field .1.albumartist.m of type int64` (executed). Go errors on this cell
    // too (executed: `json: cannot unmarshal bool into Go struct field
    // .1.albumartist.m of type int64`).
    let got = hydrate::artist_post_scan(stats_row(
        r#"{"1":{"albumartist":{"m":true},"albumartist":{"m":1}}}"#,
        ArtistRecord::default(),
    ))
    .expect("DIVERGENCE: the port accepts a duplicate role key Go rejects");
    assert_eq!(
        got.stats,
        BTreeMap::from([(
            Role::from("albumartist"),
            ArtistStats {
                song_count: 1,
                album_count: 0,
                size: 0
            }
        )]),
        "the last duplicate role value folds in, where Go errors"
    );
    // (2) The ORDERING: a cell with two bad role entries. Go names `.b.y` (first in
    // source order); the port walks the B-tree and names `.a.x` (first in sorted
    // order). The port string below is the one this seam produced (executed);
    // Go's is quoted in the doc comment, not assertable here.
    let err = hydrate::artist_post_scan(stats_row(
        r#"{"b":{"y":true},"a":{"x":true}}"#,
        ArtistRecord::default(),
    ))
    .expect_err("both sides reject a multi-bad cell");
    assert_eq!(
        err.to_string(),
        format!(
            "{}json: cannot unmarshal bool into Go struct field .a.x of type map[string]int64",
            hydrate::ARTIST_STATS_PREFIX
        ),
        "the port names the SORTED-order entry, Go the document-order one"
    );
}

/// The two remaining stats-decode sentence divergences: Go escapes path keys per
/// JSON Pointer, and an EMPTY library-id key takes Go's ROOT branch (no field at
/// all). Executed Go 1.27.4 against `map[string]map[string]map[string]int64`:
/// * `{"a/b":5}` → `json: cannot unmarshal number into Go struct field .a~1b of type
///   map[string]map[string]int64`
/// * `{"a~b":5}` → `… Go struct field .a~0b of type map[string]map[string]int64`
/// * `{"1":{"a/b":{"m":true}}}` → `json: cannot unmarshal bool into Go struct field
///   .1.a~1b.m of type int64`
/// * `{"":5}` → `json: cannot unmarshal number into Go value of type
///   map[string]map[string]int64` (Go's `Field` is empty, so `v2_decode.go:149-151`
///   emits the root sentence with NO field)
/// * `{"a":{"":5}}` → `… Go struct field .a. of type map[string]int64` (matches the
///   port byte-for-byte — the divergence is only at the level Go reports as root)
///
/// The port's seam strings below are the ones this test produced (executed): the
/// library arm interpolates the raw key (`hydrate.rs:2749`) and names
/// `map[string]map[string]int64` (`hydrate.rs:2755`), so only the escaping and the
/// root-vs-field SHAPE differ, never the type leg.
#[test]
fn path_escaping_and_empty_key_diverge_from_go() {
    for (payload, want) in [
        (
            r#"{"a/b":5}"#,
            "json: cannot unmarshal number into Go struct field .a/b of type map[string]map[string]int64",
        ),
        (
            r#"{"a~b":5}"#,
            "json: cannot unmarshal number into Go struct field .a~b of type map[string]map[string]int64",
        ),
        (
            r#"{"1":{"a/b":{"m":true}}}"#,
            "json: cannot unmarshal bool into Go struct field .1.a/b.m of type int64",
        ),
        (
            r#"{"":5}"#,
            "json: cannot unmarshal number into Go struct field . of type map[string]map[string]int64",
        ),
        (
            // Go and the port agree here, pinned so the divergence cannot silently
            // widen into the deeper levels.
            r#"{"a":{"":5}}"#,
            "json: cannot unmarshal number into Go struct field .a. of type map[string]int64",
        ),
    ] {
        let err = hydrate::artist_post_scan(stats_row(payload, ArtistRecord::default()))
            .err()
            .unwrap_or_else(|| panic!("{payload} must fail the stats decode"));
        assert_eq!(
            err.to_string(),
            format!("{}{want}", hydrate::ARTIST_STATS_PREFIX),
            "port seam string for {payload}"
        );
    }
}

/// A missing `a`/`s` key zero-fills with no error (`int(stat["a"])` on a missing
/// map key is Go's zero value, `artist_repository.go:57-59`), as does a JSON
/// `null` at the stat level.
#[test]
fn missing_stat_keys_and_null_stats_zero_fill_without_error() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-m", &[]);
    seed_library_artist(&store, 1, "ar-m", r#"{"albumartist":{"m":4}}"#);
    let got = hydrate_artist(&store, "ar-m", None).expect("decodes");
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 4,
            album_count: 0,
            size: 0
        }),
        "missing a/s are Go's zero values"
    );

    insert_artist_raw(&store, "ar-null-stat", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-null-stat",
        r#"{"total":{"m":null,"a":null,"s":null}}"#,
    );
    let got = hydrate_artist(&store, "ar-null-stat", None).expect("decodes");
    assert_eq!(
        (got.size, got.song_count, got.album_count),
        (0, 0, 0),
        "a JSON null at the stat level zero-fills with err=nil"
    );
    assert!(got.stats.is_empty(), "`total` is still not a stats key");

    // And a null stat inside a real role entry, executed through the select.
    insert_artist_raw(&store, "ar-null-role-stat", &[]);
    seed_library_artist(
        &store,
        1,
        "ar-null-role-stat",
        r#"{"albumartist":{"m":null,"a":2,"s":null}}"#,
    );
    let got = hydrate_artist(&store, "ar-null-role-stat", None).expect("decodes");
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 0,
            album_count: 2,
            size: 0
        })
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A projection carrying a NULL `library_stats_json` cell fails the row read:
/// dbx binds the alias to `dbArtist.LibraryStatsJSON`, a non-pointer `string`,
/// so Go's `converting NULL to string is unsupported` aborts the scan. `join_text`
/// distinguishes an ABSENT alias (Go's zero value) from a NULL cell, which is
/// exactly what a light projection needs.
#[test]
fn null_library_stats_json_is_a_required_column_error_and_an_absent_alias_is_not() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");

    let null_alias = (
        "SELECT NULL as library_stats_json, artist.* FROM artist WHERE artist.id = ?".to_string(),
        vec![Value::from("ar-1")],
    );
    let msg =
        read_error(query_all(&store, &null_alias, hydrate_artist_row).map(|mut r| r.remove(0)));
    assert!(
        msg.contains("NULL") && msg.contains("`library_stats_json`"),
        "Go parity, and the failing column is named: {msg}"
    );

    // A projection WITHOUT the alias at all (getAllIDs' shape, `:283-291`) leaves
    // Go's zero value and takes the skip branch — no error.
    let no_alias = (
        "SELECT artist.* FROM artist WHERE artist.id = ?".to_string(),
        vec![Value::from("ar-1")],
    );
    let got = query_all(&store, &no_alias, hydrate_artist_row)
        .expect("an absent alias is Go's zero value")
        .remove(0);
    assert!(got.stats.is_empty());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// similar_artists
// ---------------------------------------------------------------------------

/// `""` / `[]` / `null` all give an EMPTY decoded list, and the RAW column text
/// survives on the record — which is what the write map emits.
#[test]
fn similar_artists_empty_forms_decode_to_empty_and_keep_the_raw_text() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    for (id, cell) in [
        ("ar-empty-sa", ""),
        ("ar-array", "[]"),
        ("ar-null-sa", "null"),
    ] {
        insert_artist_raw(&store, id, &[("similar_artists", cell)]);
        seed_library_artist(&store, 1, id, "{}");
        let got = hydrate_artist(&store, id, None).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(got.similar, Vec::new(), "{id} decodes to an empty list");
        assert_eq!(
            got.similar_artists, cell,
            "{id}: the raw column text is what put_cols writes"
        );
    }
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `[null,{…}]` yields a zero-valued FIRST entry, not a skip — Go appends a zero
/// `dbSimilarArtist` for a JSON null element (`artist_repository.go:91-96`).
/// A missing key zero-fills, and an unknown JSON field is ignored on read.
#[test]
fn similar_artists_null_element_is_a_zero_entry_and_unknown_fields_are_ignored() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(
        &store,
        "ar-mixed",
        &[(
            "similar_artists",
            r#"[null,{"id":"a","name":"B"},{"id":"c"}]"#,
        )],
    );
    seed_library_artist(&store, 1, "ar-mixed", "{}");
    let got = hydrate_artist(&store, "ar-mixed", None).expect("decodes");
    assert_eq!(
        got.similar,
        vec![
            SimilarArtist::default(),
            SimilarArtist {
                id: "a".to_string(),
                name: "B".to_string()
            },
            SimilarArtist {
                id: "c".to_string(),
                name: String::new()
            },
        ]
    );
    // The Go marshal the decoded list would produce (`PostMapArgs` `:101-106`,
    // `omitempty` on both keys). The port's write map keeps the RAW text instead
    // — the documented round-trip direction difference.
    assert_eq!(
        artist_repo::marshal_similar_artists(&[
            (String::new(), String::new()),
            (got.similar[1].id.clone(), got.similar[1].name.clone()),
            (got.similar[2].id.clone(), got.similar[2].name.clone()),
        ]),
        r#"[{},{"id":"a","name":"B"},{"id":"c"}]"#
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}
/// Field-level `encoding/json` parity for a similar-artists element. A
/// non-string `id`/`name` reports Go's FIELD-level sentence verbatim —
/// `json: cannot unmarshal <type> into Go struct field .<index>.<key> of type
/// string`. Go's struct-field sentence carries NO struct name
/// (`UnmarshalTypeError.Error`, `encoding/json/decode.go:136-143`), so exact
/// parity is reachable here: it replaces serde's
/// ``invalid type: integer `3`, expected a string``, which named neither the
/// `.0.id` path nor the `string` target. Executed oracle (Go 1.27.4, verbatim
/// `dbSimilarArtist` with `omitempty` on both fields) produced every row below.
#[test]
fn similar_artists_field_type_errors_match_go_sentence() {
    let exact: &[(&str, &str)] = &[
        (
            r#"[{"id":3}]"#,
            "json: cannot unmarshal number into Go struct field .0.id of type string",
        ),
        (
            r#"[{"name":true}]"#,
            "json: cannot unmarshal bool into Go struct field .0.name of type string",
        ),
        (
            r#"[{"name":3}]"#,
            "json: cannot unmarshal number into Go struct field .0.name of type string",
        ),
        (
            r#"[{"id":1.5}]"#,
            "json: cannot unmarshal number into Go struct field .0.id of type string",
        ),
        (
            // A number DESTINED for a string field never carries `Value`: Go's
            // number arm only saves the literal for a numeric destination, so
            // even a non-integer spelling reads bare `number` (contrast the
            // int64 stats arm, which spells `number 1.5`). Executed.
            r#"[{"id":1e3}]"#,
            "json: cannot unmarshal number into Go struct field .0.id of type string",
        ),
        (
            r#"[{"id":-9223372036854775808}]"#,
            "json: cannot unmarshal number into Go struct field .0.id of type string",
        ),
        (
            r#"[{"id":{"a":1}}]"#,
            "json: cannot unmarshal object into Go struct field .0.id of type string",
        ),
        (
            r#"[{"name":[1]}]"#,
            "json: cannot unmarshal array into Go struct field .0.name of type string",
        ),
        (
            // A case-insensitive match is still a match, and the sentence names
            // the RAW JSON key: Go sets `UnmarshalTypeError.Field` to the key it
            // read (`decode.go`'s `struct`), not to the struct field's name.
            r#"[{"ID":3}]"#,
            "json: cannot unmarshal number into Go struct field .0.ID of type string",
        ),
        (
            // The index is the element position, so a later element reports `.1`.
            r#"[{"zzz":1},{"id":3}]"#,
            "json: cannot unmarshal number into Go struct field .1.id of type string",
        ),
    ];
    for (cell, want) in exact {
        let err = hydrate::artist_post_scan(stats_row(
            "{}",
            ArtistRecord {
                similar_artists: (*cell).to_string(),
                ..ArtistRecord::default()
            },
        ))
        .err()
        .unwrap_or_else(|| panic!("{cell} must fail the similar decode"));
        assert_eq!(
            err.to_string(),
            format!("{}{want}", hydrate::ARTIST_SIMILAR_PREFIX),
            "Go's exact field-level sentence for {cell}"
        );
    }

    // The arms that must NOT error, all executed Go oracle cases: a null field
    // zero-fills, a missing key zero-fills, an unknown field is ignored, a null
    // element is a zero entry, and a case-folded key fills the field.
    let ok_cells: &[(&str, &[SimilarArtist])] = &[
        (r#"[{"id":null}]"#, &[SimilarArtist::default()]),
        (r#"[{"name":null}]"#, &[SimilarArtist::default()]),
        (
            r#"[{"id":null,"name":"B"}]"#,
            &[SimilarArtist {
                id: String::new(),
                name: "B".to_string(),
            }],
        ),
        (r#"[{"zzz":1}]"#, &[SimilarArtist::default()]),
        (r#"[{}]"#, &[SimilarArtist::default()]),
        (
            r#"[null,{"id":"a","name":"b"}]"#,
            &[
                SimilarArtist::default(),
                SimilarArtist {
                    id: "a".to_string(),
                    name: "b".to_string(),
                },
            ],
        ),
        (
            r#"[{"ID":"x","NAME":"y"}]"#,
            &[SimilarArtist {
                id: "x".to_string(),
                name: "y".to_string(),
            }],
        ),
    ];
    for (cell, want) in ok_cells {
        let got = hydrate::artist_post_scan(stats_row(
            "{}",
            ArtistRecord {
                similar_artists: (*cell).to_string(),
                ..ArtistRecord::default()
            },
        ))
        .unwrap_or_else(|e| panic!("{cell} must decode like Go, got {e}"));
        assert_eq!(got.similar, *want, "{cell}");
    }

    // Executed through the REAL seam: a bad stored cell fails the whole row read
    // with Go's sentence inside Go's conversion-failure shape, and the failure is
    // the SIMILAR decode's — the stats fold above it (`:52-79`) already ran.
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-bad-sa", &[("similar_artists", r#"[{"id":3}]"#)]);
    seed_library_artist(&store, 1, "ar-bad-sa", CELL_WITH_TOTAL);
    let msg = read_error(hydrate_artist(&store, "ar-bad-sa", None));
    assert_eq!(
        msg,
        format!(
            "{}json: cannot unmarshal number into Go struct field .0.id of type string",
            hydrate::ARTIST_SIMILAR_PREFIX
        )
    );
    assert!(
        !msg.starts_with(hydrate::ARTIST_STATS_PREFIX),
        "the stats payload is valid, so the failure is the similar one: {msg}"
    );
    // The same artist with a valid similar cell folds the stats the same way, i.e.
    // the stats loop is the loop that ran before this failure.
    insert_artist_raw(
        &store,
        "ar-ok-sa",
        &[("similar_artists", r#"[{"id":"a"}]"#)],
    );
    seed_library_artist(&store, 1, "ar-ok-sa", CELL_WITH_TOTAL);
    let got = hydrate_artist(&store, "ar-ok-sa", None).expect("valid cell decodes");
    assert_eq!(
        got.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 2,
            album_count: 3,
            size: 40
        }),
        "the stats fold is the same one that ran before the failing read"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A malformed `similar_artists` cell carries the EXACT similar prefix — and the
/// stats loop has already run by then. Go's decoder mutates the target before
/// failing, so the ordering proof is that a valid stats payload PLUS a broken
/// similar cell reaches the failure arm with the fold already applied (asserted
/// by the port's own re-run of the same payload pair, then by Go's executed
/// behaviour recorded in the residual note).
#[test]
fn similar_artists_failure_carries_its_prefix_and_runs_after_the_stats_loop() {
    for payload in ["{", "[\"x\"]", "{}", "1"] {
        let err = hydrate::artist_post_scan(stats_row(
            STATS_WITH_TOTAL,
            ArtistRecord {
                similar_artists: payload.to_string(),
                ..ArtistRecord::default()
            },
        ))
        .err()
        .unwrap_or_else(|| panic!("{payload} must fail the similar decode"));
        let text = err.to_string();
        assert!(
            text.starts_with(hydrate::ARTIST_SIMILAR_PREFIX),
            "Go's exact similar prefix on {payload}: {text}"
        );
        assert!(
            !text.starts_with(hydrate::ARTIST_STATS_PREFIX),
            "the stats payload is valid, so the failure is the similar one: {text}"
        );
    }

    // Ordering, proved by mutation: `artist_post_scan` is the whole PostScan, so
    // run the stats fold first and confirm it lands, then feed the SAME row with
    // a broken similar cell and confirm the stats fold ran before the failure
    // (Go's decoder leaves `Stats` populated and the row is discarded upstream).
    let ok = hydrate::artist_post_scan(stats_row(
        STATS_WITH_TOTAL,
        ArtistRecord {
            similar_artists: r#"[{"id":"a","name":"B"}]"#.to_string(),
            ..ArtistRecord::default()
        },
    ))
    .expect("both decodes succeed");
    assert_eq!(
        ok.stats.get(&Role::from("albumartist")),
        Some(&ArtistStats {
            song_count: 2,
            album_count: 3,
            size: 40
        }),
        "stats fold applied"
    );
    assert_eq!(
        (ok.size, ok.song_count, ok.album_count),
        (10, 1, 1),
        "the total fold ran before the similar decode"
    );
    assert_eq!(ok.similar.len(), 1);

    // The port's ordering claim, executed: a similar-decode failure does not
    // undo the fold. `artist_post_scan` returns the error, so re-run the stats
    // payload alone and compare against the same row's fold — identical, i.e.
    // the stats loop already ran by the time the similar arm failed.
    let stats_only = hydrate::artist_post_scan(stats_row(
        STATS_WITH_TOTAL,
        ArtistRecord {
            similar_artists: String::new(),
            ..ArtistRecord::default()
        },
    ))
    .expect("stats-only decode");
    assert_eq!(
        stats_only.stats, ok.stats,
        "the stats loop is the same loop that ran before the similar failure"
    );
    assert_eq!(stats_only.size, ok.size);
}

// ---------------------------------------------------------------------------
// annotation columns
// ---------------------------------------------------------------------------

/// Logged-in user WITH an annotation row: the six coalesced columns land and the
/// stored zone survives (`+03:00`). WITHOUT one: the LEFT-join miss decodes to
/// zeros, with `None` stamps rather than zero times.
#[test]
fn annotation_hydrates_with_a_row_and_decodes_zeros_without_one() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_user(&store, "u-1");
    insert_artist_raw(&store, "ar-ann", &[("average_rating", "3.5")]);
    seed_library_artist(&store, 1, "ar-ann", "{}");
    insert_artist_raw(&store, "ar-none", &[("average_rating", "0")]);
    seed_library_artist(&store, 1, "ar-none", "{}");
    seed_annotation(&store, "u-1", "artist", "ar-ann");

    let got = hydrate_artist(&store, "ar-ann", Some("u-1")).expect("hydrates with a row");
    assert_eq!(got.annotation.play_count, 7);
    assert_eq!(got.annotation.rating, 4);
    assert!(got.annotation.starred);
    assert_eq!(got.annotation.play_date, None, "NULL *time.Time is nil");
    assert_eq!(
        got.annotation.rated_at.as_deref(),
        Some("2024-02-02T00:00:00Z"),
        "a UTC-stored stamp renders Z"
    );
    assert_eq!(
        got.annotation.starred_at.as_deref(),
        Some(STARRED_AT_WIRE),
        "RFC3339Nano with the zone the driver parsed"
    );
    assert_eq!(got.annotation.average_rating, 3.5, "artist.average_rating");

    let got = hydrate_artist(&store, "ar-none", Some("u-1")).expect("hydrates without a row");
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
        },
        "LEFT-join miss: coalesce arms give zeros, bare stamps give None"
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `annotation_user = None` is Go's `invalidUserId`: `withAnnotation` adds ONLY
/// `artist.average_rating` and NO join (`sql_annotations.go:63-67`). No extra
/// projection column is needed — `artist.*` already carries `average_rating`
/// (`schema_ddl.rs:48`), which is why `row_to_annotation`'s `has_column` probe
/// finds it.
#[test]
fn annotation_user_none_skips_the_join_and_still_reads_average_rating() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_user(&store, "u-1");
    insert_artist_raw(&store, "ar-1", &[("average_rating", "4.25")]);
    seed_library_artist(&store, 1, "ar-1", "{}");
    // Even with an annotation row present, a None user must NOT see it.
    seed_annotation(&store, "u-1", "artist", "ar-1");

    let (sql, args) = artist_repo::get_sql("ar-1", &None, None);
    assert!(
        !sql.contains("LEFT JOIN annotation"),
        "invalidUserId adds no join: {sql}"
    );
    assert!(args.len() == 1, "only the id binds: {args:?}");

    let got = hydrate_artist(&store, "ar-1", None).expect("hydrates");
    assert_eq!(got.annotation.average_rating, 4.25, "read from artist.*");
    assert_eq!(
        got.annotation.play_count, 0,
        "no join, so no play_count column"
    );
    assert_eq!(got.annotation.rating, 0);
    assert!(!got.annotation.starred);
    assert_eq!(got.annotation.starred_at, None);

    // The same read WITH the join sees the row: the two arms differ only by it.
    let joined = hydrate_artist(&store, "ar-1", Some("u-1")).expect("hydrates with join");
    assert_eq!(joined.annotation.play_count, 7);
    assert_eq!(joined.annotation.average_rating, 4.25);
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// GROUP BY / repo reads
// ---------------------------------------------------------------------------

/// `GROUP BY artist.id` (`:213`) collapses the `library_artist` fan-out: exactly
/// one row per artist, even across two libraries.
#[test]
fn group_by_yields_exactly_one_row_per_artist() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_library(&store, 2);
    insert_artist_raw(&store, "ar-1", &[]);
    insert_artist_raw(&store, "ar-2", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");
    seed_library_artist(&store, 2, "ar-1", "{}");
    seed_library_artist(&store, 1, "ar-2", "{}");

    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, None)
    })
    .expect("get_all runs");
    let mut ids: Vec<&str> = outcome.items.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["ar-1", "ar-2"], "one row per artist");
    assert_eq!(outcome.artwork_error, None);
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `Get` reports `NotFound` — never a statement error — on all three zero-row
/// paths: an unknown id; a non-admin whose library is not granted
/// (`artist_repository.go:199-202`); an artist with NO `library_artist` row,
/// which the INNER join drops (`:195`).
#[test]
fn get_reports_not_found_for_unknown_id_ungranted_user_and_no_junction_row() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_library(&store, 2);
    seed_user(&store, "u-1");
    seed_user(&store, "u-2");
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");
    insert_artist_raw(&store, "ar-no-junction", &[]);

    // Unknown id.
    let err = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-nope", &None, None)
    })
    .expect_err("unknown id is not found");
    assert!(matches!(err, ArtistError::NotFound), "{err:?}");
    assert_eq!(err.to_string(), "record not found");

    // Artist with no library_artist row: dropped by the INNER join (Go parity).
    let err = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-no-junction", &None, None)
    })
    .expect_err("an artist with no junction row is invisible");
    assert!(matches!(err, ArtistError::NotFound), "{err:?}");

    // Granted user reads it; a user granted only library 2 does not.
    grant_library(&store, "u-1", 1);
    grant_library(&store, "u-2", 2);
    let got = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &Some("u-1".to_string()), Some("u-1"))
    })
    .expect("granted user reads the artist");
    assert_eq!(got.id, "ar-1");
    let err = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &Some("u-2".to_string()), None)
    })
    .expect_err("ungranted user reads nothing");
    assert!(matches!(err, ArtistError::NotFound), "{err:?}");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `GetAll` returns an EMPTY Vec and never `NotFound` (`:272-279` returns
/// `dba.toModels()`, a non-nil slice, plus `queryAll`'s error). It also expresses
/// `QueryOptions::Filters` and pagination, so the S-D3c "repo get_all cannot
/// express filters" residual is entity-specific, not port-wide.
#[test]
fn get_all_returns_an_empty_vec_and_never_reports_not_found() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    seed_user(&store, "u-1");
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");

    // No grant ⇒ zero rows, and the annotation join still runs (Go's GetAll
    // always applies withAnnotation for a logged-in user).
    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&Some("u-1".to_string()), Some("u-1"), None)
    })
    .expect("a zero-row GetAll is Ok");
    assert_eq!(outcome.items.len(), 0);
    assert_eq!(outcome.artwork_error, None);

    // A filter that matches nothing ⇒ zero rows, with the filter expressed.
    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, Some(&filtered("Nobody")))
    })
    .expect("filters are applied");
    assert_eq!(outcome.items.len(), 0);

    // Pagination through applyOptions (LIMIT/OFFSET).
    insert_artist_raw(&store, "ar-2", &[("name", "Beatles")]);
    seed_library_artist(&store, 1, "ar-2", "{}");
    let all = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, None)
    })
    .expect("unpaged GetAll");
    assert_eq!(all.items.len(), 2);
    let first = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, Some(&page(1, 0)))
    })
    .expect("first page");
    assert_eq!(first.items.len(), 1, "LIMIT 1");
    let second = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, Some(&page(1, 1)))
    })
    .expect("second page");
    assert_eq!(second.items.len(), 1, "LIMIT 1 OFFSET 1");
    assert_ne!(
        first.items[0].id, second.items[0].id,
        "the two pages are disjoint, so OFFSET applied"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `artist.*` carries `full_text` and `search_normalized`, which have no
/// destination field on `dbArtist`; dbx binds them to a throwaway `sql.NullString`
/// and discards them (`dbx@v1.12.0/rows.go:80-86`), so `row_to_artist` must read
/// neither and must not fail when both are present.
#[test]
fn row_to_artist_drops_full_text_and_search_normalized_without_failing() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    // The real select already expands artist.* — which includes both columns.
    insert_artist_raw(
        &store,
        "ar-1",
        &[
            ("full_text", " eddie halen"),
            ("search_normalized", "eddie"),
        ],
    );
    seed_library_artist(&store, 1, "ar-1", "{}");
    let (sql, _) = artist_repo::get_sql("ar-1", &None, None);
    assert!(
        sql.contains("artist.*"),
        "the select expands the columns: {sql}"
    );
    let got = hydrate_artist(&store, "ar-1", None).expect("both dropped columns hydrate fine");
    assert_eq!(got.name, "Eddie Van Halen");

    // A NULL cell in a DROPPED column is also fine, because neither column is
    // read at all (Go binds it to a throwaway `sql.NullString`, which accepts
    // NULL). Only `full_text` is nullable in the DDL — `search_normalized` is
    // `TEXT NOT NULL DEFAULT ''` (`schema_ddl.rs:48`), so a NULL there is
    // unreachable and attempting it is a constraint failure, not a read path.
    insert_artist_raw(
        &store,
        "ar-2",
        &[("full_text", ""), ("search_normalized", "")],
    );
    seed_library_artist(&store, 1, "ar-2", "{}");
    insert(
        &store,
        "UPDATE artist SET full_text = NULL WHERE id = 'ar-2'",
        &[],
    );
    let got = hydrate_artist(&store, "ar-2", None).expect("NULL full_text is not read");
    assert_eq!(got.id, "ar-2");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// artwork (kind prefix `ar`)
// ---------------------------------------------------------------------------

/// The three artist artwork states, each distinct: resolved (`item_artwork` +
/// `artwork` rows), known-absent (`hash = ''` ⇒ `image_absent`), unresolved (no
/// row ⇒ the zero `ItemImage`, which is NOT absent). Seeding the kind NAME finds
/// nothing — the lookup binds the prefix (`artwork_repository.go:169`).
#[test]
fn artist_artwork_has_three_distinct_states_and_matches_the_kind_prefix() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    for id in ["ar-found", "ar-absent", "ar-unresolved"] {
        insert_artist_raw(&store, id, &[]);
        seed_library_artist(&store, 1, id, "{}");
    }
    seed_image(&store, "h1", 800, 600, "blur", "#ff0000");
    seed_state(&store, KIND_AR, "ar-found", "h1");
    seed_state(&store, KIND_AR, "ar-absent", "");
    // A kind NAME row must never match the `ar` lookup.
    seed_image(&store, "h-name", 10, 10, "x", "#000000");
    seed_state(&store, KIND_NAME, "ar-unresolved", "h-name");

    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, None)
    })
    .expect("get_all hydrates artwork");
    let image_of = |id: &str| {
        outcome
            .items
            .iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("{id} present"))
            .image
            .clone()
    };

    assert_eq!(
        image_of("ar-found"),
        ItemImage {
            image_hash: "h1".to_string(),
            image_absent: false,
            blur_hash: "blur".to_string(),
            thumb_hash: "th".to_string(),
            dominant_color: "#ff0000".to_string(),
            image_width: 800,
            image_height: 600,
        }
    );
    assert_eq!(
        image_of("ar-absent"),
        ItemImage {
            image_hash: String::new(),
            image_absent: true,
            ..ItemImage::default()
        },
        "known-absent sets image_absent and keeps the zero derived fields"
    );
    assert_eq!(
        image_of("ar-unresolved"),
        ItemImage::default(),
        "unresolved is the ZERO image — the kind-NAME row did not match"
    );
    assert!(!image_of("ar-unresolved").image_absent);
    assert_eq!(outcome.artwork_error, None);

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Go's swallow-and-log path (`artwork_hydration.go:58-68`): with `item_artwork`
/// gone the read is still `Ok`, the failure text arrives through
/// `ReadOutcome::artwork_error`, and every image stays zero. `ArtistRepo::get`
/// discards that failure exactly as Go's `Get` does.
#[test]
fn artwork_failure_is_reported_by_get_all_and_discarded_by_get() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");

    lease(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", [])
            .expect("drop item_artwork")
    });

    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, None)
    })
    .expect("the read survives a failed artwork lookup");
    assert_eq!(outcome.items.len(), 1);
    let error = outcome
        .artwork_error
        .as_deref()
        .expect("Go's swallowed log.Error is surfaced as text");
    assert!(
        error.contains("item_artwork"),
        "the text names the table: {error}"
    );
    assert_eq!(
        outcome.items[0].image,
        ItemImage::default(),
        "an empty lookup map leaves every image zero (unresolved)"
    );

    // Get discards the same failure and still returns the record.
    let got = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("Get swallows the artwork failure");
    assert_eq!(got.image, ItemImage::default());

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `hydrate_items` issues NO query for an empty slice
/// (`artwork_hydration.go:59-61`), so a zero-row read never touches
/// `item_artwork` — even when the table is missing.
#[test]
fn empty_slice_issues_no_artwork_query() {
    let (store, path) = fresh_store();
    lease(&store, |conn| {
        conn.execute("DROP TABLE item_artwork", [])
            .expect("drop item_artwork")
    });
    let outcome = lease(&store, |conn| {
        ArtistRepo::new(conn).get_all(&None, None, None)
    })
    .expect("zero artists, zero artwork statements");
    assert!(outcome.items.is_empty());
    assert_eq!(outcome.artwork_error, None, "no ids ⇒ no statement");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

// ---------------------------------------------------------------------------
// write-map invariance
// ---------------------------------------------------------------------------

/// The keys `put_cols` derives — 16 struct columns + the 2 `PostMapArgs` derived
/// columns, minus an empty `sort_artist_name`/`mbz_artist_id`
/// (`artist_repository.go:113-120`) — so 18 with both set and 16 with both empty
/// (Go's UPDATE map is 17/15 because `filterUpdateValues` re-adds `id`,
/// `sql_base_repository.go:612-616`). Hydration adds NO key, because every
/// hydrated field is `structs:"-"` in Go (`model/artist.go:15-16`, `:27-30`).
#[test]
fn write_map_key_count_is_derived_and_unchanged_by_hydration() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(
        &store,
        "ar-1",
        &[
            ("sort_artist_name", "Van Halen, Eddie"),
            ("mbz_artist_id", "mb-1"),
            ("similar_artists", r#"[{"id":"a","name":"B"},{"zzz":1}]"#),
            ("average_rating", "4.5"),
        ],
    );
    seed_library_artist(&store, 1, "ar-1", CELL_WITH_TOTAL);
    seed_state(&store, KIND_AR, "ar-1", "h1");
    seed_image(&store, "h1", 800, 600, "blur", "#ff0000");

    let hydrated = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("hydrated record");
    assert!(!hydrated.stats.is_empty(), "stats hydrated");
    assert_eq!(
        (hydrated.size, hydrated.song_count, hydrated.album_count),
        (10, 1, 1)
    );
    assert_eq!(hydrated.image.image_hash, "h1", "artwork hydrated");
    assert_eq!(hydrated.annotation.average_rating, 4.5);
    assert_eq!(hydrated.similar.len(), 2);

    let cols = artist_repo::put_cols(&hydrated);
    let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        cols.len(),
        18,
        "derived key count with sort/mbz set: {names:?}"
    );

    // None of the hydrated (structs:"-") state is a write key.
    for banned in [
        "stats",
        "size",
        "album_count",
        "song_count",
        "annotation",
        "image",
        "image_hash",
        "image_absent",
        "blur_hash",
        "thumb_hash",
        "dominant_color",
        "image_width",
        "image_height",
        "similar",
        "average_rating",
        "library_stats_json",
        "play_count",
        "play_date",
        "rating",
        "rated_at",
        "starred",
        "starred_at",
    ] {
        assert!(
            !names.contains(&banned),
            "{banned} must not be a write key: {names:?}"
        );
    }
    // The derived columns ARE keys (`PostMapArgs` `:109-110`), and `id` is the
    // 16th struct key (`model/artist.go:18`), matching `album_repo::album_row`
    // which also emits `id` (`album_repo.rs:507`).
    assert!(names.contains(&"id"));
    assert!(names.contains(&"full_text"));
    assert!(names.contains(&"search_normalized"));
    assert!(names.contains(&"similar_artists"));

    // An unwritten record with the same persisted columns yields the SAME count.
    let written = ArtistRecord {
        id: "ar-1".to_string(),
        name: hydrated.name.clone(),
        sort_artist_name: hydrated.sort_artist_name.clone(),
        order_artist_name: hydrated.order_artist_name.clone(),
        mbz_artist_id: hydrated.mbz_artist_id.clone(),
        biography: hydrated.biography.clone(),
        small_image_url: hydrated.small_image_url.clone(),
        medium_image_url: hydrated.medium_image_url.clone(),
        large_image_url: hydrated.large_image_url.clone(),
        external_url: hydrated.external_url.clone(),
        similar_artists: hydrated.similar_artists.clone(),
        external_info_updated_at: hydrated.external_info_updated_at,
        missing: hydrated.missing,
        uploaded_image: hydrated.uploaded_image.clone(),
        created_at: hydrated.created_at,
        updated_at: hydrated.updated_at,
        ..ArtistRecord::default()
    };
    assert_eq!(
        artist_repo::put_cols(&written).len(),
        18,
        "hydration cannot change the key count"
    );
    // And the two maps agree key-for-key, not just in count.
    assert_eq!(
        artist_repo::put_cols(&written),
        cols,
        "the hydrated and unwritten write maps are identical"
    );

    // Both empty ⇒ 16 (Go deletes them so an empty value never overwrites).
    let bare = ArtistRecord {
        id: "ar-1".to_string(),
        ..ArtistRecord::default()
    };
    let bare_cols = artist_repo::put_cols(&bare);
    let bare_names: Vec<&str> = bare_cols.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(bare_names.len(), 16, "{bare_names:?}");
    assert!(!bare_names.contains(&"sort_artist_name"));
    assert!(!bare_names.contains(&"mbz_artist_id"));

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `put_sql` renders BYTE-IDENTICAL UPDATE and INSERT legs (SQL text AND args)
/// for a hydrated record vs a clone whose hydration-only fields are reset.
#[test]
fn put_sql_renders_identically_for_a_hydrated_and_a_reset_record() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(
        &store,
        "ar-1",
        &[
            ("sort_artist_name", "Van Halen, Eddie"),
            ("mbz_artist_id", "mb-1"),
            ("similar_artists", r#"[{"id":"a","name":"B"}]"#),
            ("biography", "bio"),
            ("uploaded_image", "up"),
        ],
    );
    seed_library_artist(&store, 1, "ar-1", CELL_WITH_TOTAL);
    seed_state(&store, KIND_AR, "ar-1", "h1");
    seed_image(&store, "h1", 800, 600, "blur", "#ff0000");

    let hydrated = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("hydrated record");
    let now = nd_db::time::parse(T1).expect("T1 parses");
    let ((update1, update_args1), (insert1, insert_args1)) =
        artist_repo::put_sql(&hydrated, now, &[]);

    let reset = ArtistRecord {
        stats: BTreeMap::new(),
        size: 0,
        album_count: 0,
        song_count: 0,
        annotation: nd_model::annotation::Annotation::default(),
        image: ItemImage::default(),
        similar: Vec::new(),
        ..hydrated.clone()
    };
    assert_ne!(
        hydrated, reset,
        "the two records DO differ in hydration state"
    );
    let ((update2, update_args2), (insert2, insert_args2)) = artist_repo::put_sql(&reset, now, &[]);
    assert_eq!(update1, update2, "UPDATE text unchanged by hydration");
    assert_eq!(
        update_args1, update_args2,
        "UPDATE args unchanged by hydration"
    );
    assert_eq!(insert1, insert2, "INSERT text unchanged by hydration");
    assert_eq!(
        insert_args1, insert_args2,
        "INSERT args unchanged by hydration"
    );
    // Derived, not guessed: put_cols emits 18 keys here (sort/mbz both set);
    // `put_sql` drops `created_at` from the UPDATE leg
    // (`selectUpdateColumns`, `sql_base_repository.go:605`), so 17 SET args + the
    // trailing `id` predicate = 18. The INSERT leg carries all 18.
    assert_eq!(artist_repo::put_cols(&hydrated).len(), 18);
    assert_eq!(update_args1.len(), 18, "{update1}");
    assert_eq!(insert_args1.len(), 18, "{insert1}");

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The raw `similar_artists` column text survives a read→write cycle, whereas Go
/// re-marshals from the decoded list (`PostMapArgs` `:101-106`) and therefore
/// DROPS unknown JSON fields. The port's read→write is lossless here; Go's is the
/// lossier direction.
#[test]
fn raw_similar_artists_text_survives_a_read_then_write_cycle() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    let now = nd_db::time::parse(T1).expect("T1 parses");
    let record = ArtistRecord {
        id: "ar-1".to_string(),
        name: "Eddie Van Halen".to_string(),
        order_artist_name: "Eddie Van Halen".to_string(),
        // An unknown field inside one entry: Go's read drops it and PostMapArgs
        // re-marshals without it, so a Go read→write cycle LOSES it.
        similar_artists: r#"[{"id":"a","name":"B","zzz":1}]"#.to_string(),
        created_at: Some(now),
        updated_at: Some(now),
        ..ArtistRecord::default()
    };
    let (_update, insert_stmt) = artist_repo::put_sql(&record, now, &[]);
    lease(&store, |conn| {
        conn.execute(
            &insert_stmt.0,
            rusqlite::params_from_iter(insert_stmt.1.iter()),
        )
        .expect("insert")
    });
    seed_library_artist(&store, 1, "ar-1", "{}");

    let got = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("reads back");
    assert_eq!(got.similar.len(), 1, "id/name only");
    assert_eq!(got.similar[0].id, "a");
    assert_eq!(got.similar[0].name, "B");
    assert_eq!(
        got.similar_artists, record.similar_artists,
        "the raw column text is preserved, so this direction is lossless"
    );
    // And the raw text is exactly what the write map emits again.
    let written = artist_repo::put_cols(&got)
        .into_iter()
        .find(|(c, _)| c == "similar_artists")
        .map(|(_, val)| val)
        .expect("similar_artists is a write key");
    assert_eq!(written, Value::Text(record.similar_artists.clone()));
    // Go's re-marshalled form of the same decoded list (omitempty on both keys).
    assert_eq!(
        artist_repo::marshal_similar_artists(&[(
            got.similar[0].id.clone(),
            got.similar[0].name.clone()
        )]),
        r#"[{"id":"a","name":"B"}]"#,
        "Go's form drops `zzz` — the lossier direction"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The hydrated record keeps Go's `Put` stamping behaviour after hydration:
/// an UNFILTERED save stamps `updated_at` and keeps a stored `created_at`
/// (`artist_repository.go:240-242`), and a FILTERED save writes only the columns it
/// names (`sql_base_repository.go:593-610`).
#[test]
fn hydrated_record_round_trips_through_the_write_path() {
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    let now = nd_db::time::parse(T1).expect("T1 parses");
    let record = ArtistRecord {
        id: "ar-1".to_string(),
        name: "Eddie Van Halen".to_string(),
        order_artist_name: "Eddie Van Halen".to_string(),
        similar_artists: "[]".to_string(),
        ..ArtistRecord::default()
    };
    let (update, insert) = artist_repo::put_sql(&record, now, &[]);
    // No existing row ⇒ the UPDATE affects 0 rows and the INSERT leg runs.
    lease(&store, |conn| {
        let n = conn
            .execute(&update.0, rusqlite::params_from_iter(update.1.iter()))
            .expect("update affects nothing on a fresh table");
        assert_eq!(
            n, 0,
            "Go's put falls through to the INSERT when the UPDATE hits nothing"
        );
        conn.execute(&insert.0, rusqlite::params_from_iter(insert.1.iter()))
            .expect("insert")
    });
    seed_library_artist(&store, 1, "ar-1", CELL_WITH_TOTAL);

    let got = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("get");
    assert_eq!((got.size, got.song_count, got.album_count), (10, 1, 1));
    assert_eq!(
        got.stats,
        BTreeMap::from([(
            Role::from("albumartist"),
            ArtistStats {
                song_count: 2,
                album_count: 3,
                size: 40
            }
        )])
    );
    assert_eq!(got.created_at, Some(now), "Put stamps created_at");
    assert_eq!(
        got.updated_at,
        Some(now),
        "Put stamps updated_at = CreatedAt"
    );

    // A second save with a FILTERED selection (biography only) writes that column and
    // leaves BOTH stamps alone. Go parity (DB-08): `selectUpdateColumns`
    // (sql_base_repository.go:593-610) keeps exactly the requested columns minus
    // created_at/birth_time, so `updated_at` is re-stamped ONLY when the caller selects it
    // — as the scanner's list does (scanner/phase_1_folders.go:403-405) but a
    // biography/external-info update does not (artist_repository.go:247-253). The previous
    // Rust renderer appended `updated_at` to every filtered SET, which this expectation now
    // forbids: an external-info refresh must not clobber the scanner's stamp, because
    // `artist.updated_at` is the RefreshStats(allArtists=false) cutoff
    // (artist_repository.go:479).
    let later = nd_db::time::parse("2025-05-05 05:00:00+00:00").expect("later parses");
    let mut edited = got.clone();
    edited.biography = "Born in Pasadena".to_string();
    let ((update2, update_args2), _insert2) = artist_repo::put_sql(&edited, later, &["biography"]);
    let matched = lease(&store, |conn| {
        conn.execute(&update2, rusqlite::params_from_iter(update_args2.iter()))
            .expect("update runs")
    });
    assert_eq!(
        matched, 1,
        "the UPDATE leg matched the existing row, so the preserved stamp below is the output of a real filtered write, not a zero-row no-op"
    );
    let again = lease(&store, |conn| {
        ArtistRepo::new(conn).get("ar-1", &None, None)
    })
    .expect("get 2");
    assert_eq!(
        again.biography, "Born in Pasadena",
        "the selected column landed"
    );
    assert_eq!(
        again.created_at,
        Some(now),
        "created_at is never overwritten"
    );
    assert_eq!(
        again.updated_at,
        Some(now),
        "a selection that does not name updated_at leaves the stored stamp at the earlier value even though `later` was passed as the caller's clock"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Go renders `JOIN` for `Join(...)` (`squirrel select.go:298-299`) where the
/// committed artist builders emit `INNER JOIN` — semantics identical, text
/// differs. Pinned here for the artist's own builders so a future "unify the
/// JOIN wording" edit cannot silently change the JOIN TYPE: `library_artist` is
/// an INNER join in Go too (`applyLibraryFilterToArtistQuery` `:195` uses
/// `.Join`, never `.LeftJoin`), and that is what makes an artist with no junction
/// row invisible to `Get`.
#[test]
fn artist_builders_render_the_junction_join_as_inner_not_left() {
    let (get, _) = artist_repo::get_sql("ar-1", &None, None);
    let (all, _) = artist_repo::get_all_sql(&None, None, None);
    let (count, _) = artist_repo::count_all_sql(&None, None);
    for (name, sql) in [
        ("get_sql", &get),
        ("get_all_sql", &all),
        ("count_all_sql", &count),
    ] {
        assert!(
            sql.contains("INNER JOIN library_artist on library_artist.artist_id = artist.id"),
            "{name}: {sql}"
        );
        assert!(!sql.contains("LEFT JOIN library_artist"), "{name}: {sql}");
    }
    // The annotation join IS a LEFT join (`withAnnotation`,
    // `sql_annotations.go:68-72`), so the two join kinds stay distinguishable.
    let ann = artist_repo::annotation_join("u-1").expect("a real user gets a join");
    let (joined, _) = artist_repo::get_sql("ar-1", &None, Some(&ann));
    assert!(joined.contains(" LEFT JOIN annotation ON"), "{joined}");
    assert!(!joined.contains("INNER JOIN annotation"), "{joined}");
}

/// The ORDER BY leg of `applyOptions` is NOT rendered by the artist selects.
/// Go's `newSelect` runs `applyOptions` (`sql_base_repository.go:139-146`), and
/// `applyOptions` renders `OrderBy(buildSortOrder(sort, order))` for a non-empty
/// sort (`:157-158`). `selectArtist` goes through it on `GetAll` (`:270`),
/// `getAllIDs` (`:284`) and `Search` (`:664`), but this crate's artist builders
/// hand-render the tail (`artist_repo.rs`'s `pagination_fragment`, which emits
/// only LIMIT/OFFSET), so `QueryOptions.sort`/`order` are SILENTLY IGNORED and
/// row order is SQLite's, not the caller's. Pinned here so nobody reads the
/// module doc as though the sort leg existed. Wiring it is a separate lane: the
/// clause-ordered tail cannot simply grow an ORDER BY, because `search_sql` reuses
/// the base select and pushes its own `WHERE`/`ORDER BY` AFTER the tail
/// (`artist_repo.rs`'s search arms append ` WHERE …` and ` ORDER BY …` after
/// `GROUP BY`), so a clause-ordered refactor has to happen with the search path.
#[test]
fn get_all_sql_ignores_the_sort_leg_until_options_are_wired() {
    let options = QueryOptions {
        sort: "name".to_string(),
        order: "desc".to_string(),
        max: 10,
        offset: 5,
        ..QueryOptions::default()
    };
    let (sql, args) = artist_repo::get_all_sql(&None, Some(&options), None);
    assert!(
        !sql.contains("ORDER BY"),
        "the sort leg is not rendered: {sql}"
    );
    assert!(
        sql.ends_with(" GROUP BY artist.id LIMIT 10 OFFSET 5"),
        "filters/pagination only: {sql}"
    );
    assert!(args.is_empty(), "the tail contributes no args: {args:?}");

    // The filter leg IS expressed, so the asymmetry is only the sort.
    let (filtered_sql, filter_args) =
        artist_repo::get_all_sql(&None, Some(&filtered("Beatles")), None);
    assert!(
        filtered_sql.contains(" WHERE ") && !filtered_sql.contains("ORDER BY"),
        "{filtered_sql}"
    );
    assert_eq!(filter_args.len(), 1);

    // `sort_mapping` exists but is unwired in this crate's artist selects
    // (`artist_repo.rs:1390`); the only `build_sort_order` in the workspace is
    // player-specific (`player_repo.rs:624`).
    assert_eq!(
        artist_repo::sort_mapping("name"),
        Some("order_artist_name".to_string()),
        "the mapping is present, so an unwired mapping is the gap, not a missing mapping"
    );

    // Upstream caveat, executed: the stats-backed sort keys reference `artist.stats`,
    // which the multi-library migration DROPPED
    // (`db/migrations/20250701010108_add_multi_library_support.go:59`) and which the
    // fresh schema's artist DDL does not have (`schema_ddl.rs:48`). So those keys
    // would fail on a fresh DB in Go too — wiring the sort leg would need the
    // junction's stats, not `artist.stats`.
    assert_eq!(
        artist_repo::sort_mapping("song_count"),
        Some("stats->>'total'->>'m'".to_string()),
        "the Go mapping names the dropped column (artist_repository.go:151)"
    );
    let (store, path) = fresh_store();
    seed_library(&store, 1);
    insert_artist_raw(&store, "ar-1", &[]);
    seed_library_artist(&store, 1, "ar-1", "{}");
    let err = on(&store, |conn| {
        conn.query_row(
            "SELECT artist.id FROM artist ORDER BY stats->>'total'->>'m'",
            [],
            |row| row.get::<_, String>(0),
        )
    })
    .expect_err("a stats-backed sort must fail on the fresh schema");
    let text = err.to_string();
    assert!(
        text.contains("stats"),
        "the dropped column is what the mapping names: {text}"
    );

    drop(store);
    std::fs::remove_file(&path).unwrap();
}
