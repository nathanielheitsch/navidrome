//! DB-08 — the EXECUTED artist Put (`persistence/artist_repository.go:239-253` via
//! `sql_base_repository.go:618-642`) through the DB-00 typed transaction seam.
//!
//! Go truth under test, SOURCE-VERIFIED at the pinned lines and then EXECUTED-Rust against
//! a fresh schema (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp file per
//! invocation, `_foreign_keys=on` — Go's own DSN parameter, `consts/consts.go:14`):
//! * `Put` is the UPDATE-first/INSERT-fallback upsert: UPDATE by id FIRST, INSERT only on a
//!   zero-row match (`sql_base_repository.go:624-641`). A zero-row UPDATE is the insert
//!   fallback (the new record with a predefined id), never a `NotFound`.
//! * `selectUpdateColumns` (`:593-610`) keeps EXACTLY the requested columns (all when none),
//!   minus `created_at`/`birth_time`. So `updated_at` is written ONLY when the caller selects
//!   it. `Put` stamps `CreatedAt = UpdatedAt = time.Now()` on the MODEL
//!   (`artist_repository.go:240-241`), which reaches the row on the UNFILTERED save;
//!   `UpdateExternalInfo` (`:247-253`, the 7-column list) never names `updated_at`, and the
//!   scanner's list does (`scanner/phase_1_folders.go:403-405`). This is the divergence DB-08
//!   fixes: the previous renderer appended `updated_at` to every filtered SET, which made an
//!   external-info refresh clobber the scanner's stamp and re-enter the
//!   `RefreshStats(allArtists=false)` cutoff (`artist_repository.go:479`).
//! * `filterUpdateValues` (`:612-616`) ALWAYS re-adds `id`, so a selection that resolves to
//!   nothing settable renders `UPDATE artist SET id = ? WHERE id = ?` in Go. Executed
//!   (sqlite3 3.54.0): 1 row affected for an existing id, 0 for a missing one, no error. The
//!   port emits exactly that statement — not `SET  WHERE` (a parse error) and not
//!   `marshal::MarshalError::EmptyUpdateSet`, which belongs to the shared map seam.
//! * `dbArtist.PostMapArgs` (`:100-121`) derives `full_text`/`search_normalized` and DELETES
//!   an empty `sort_artist_name`/`mbz_artist_id`, so an empty value never overwrites the
//!   stored one. Go's `structs.Map` does emit those keys empty (`EXEC-GO`,
//!   `nav-db07-db08-decisions.md` §B3) — the delete is the whole parity mechanism, mirrored
//!   by `put_cols`' `retain`.
//! * The `library_artist` link is NOT written here: Go's scanner calls `artistRepo.Put` then
//!   `libraryRepo.AddArtist` (`scanner/phase_1_folders.go:401-412`), so DB-05's
//!   `add_artist_in_tx` stays the caller's separate statement in the SAME transaction.
//!
//! Every assertion runs a PRODUCTION function through `SQLStore::try_with_tx_sync` (never a
//! hand-written UPDATE), reads back through the production `ArtistRepo::get` or a per-column
//! scalar probe, and asserts per-COLUMN rather than a snapshot string, so a rotated bind order
//! fails rather than passing vacuously. Library rows come from DB-05's production `put_in_tx`,
//! never a hand-built INSERT. `ArtistRepo::get` INNER-joins `library_artist`, so a test that
//! reads back asserts the link row EXISTS first — otherwise a missing junction would make
//! every read-back pass vacuously as `NotFound` (`artist_repository.go:255-263`, proven by
//! `runtime_hydrate_artist.rs::get_reports_not_found_for_unknown_id_ungranted_user_and_no_junction_row`).
//! The absence case is pinned once, deliberately, by
//! `artist_without_a_link_row_is_invisible_to_get`.
//!
//! There is no durable Go oracle for artist Put (`testdata/` holds none), so nothing here is
//! labeled a Go golden — claims are SRC plus EXECUTED-Rust. rusqlite's `hooks` feature is not
//! enabled in this manifest and this lane may not edit it, so statement counts are taken from
//! the renderer, recorded as a coverage gap in the lane report.
//!
//! Test-local DDL (the poisoned-id trigger) is allowed as in
//! `runtime_library_writes.rs:768-776`; SQLite forbids bound variables in a trigger body, so
//! the poisoned id is spelled in the DDL. `nd-dbschema` is untouched.

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::artist_repo::{
    put_artist_in_tx, put_sql, ArtistColumn, ArtistError, ArtistRecord, ArtistRepo,
    ArtistWriteError, ArtistWriteResult, EXTERNAL_INFO_COLS,
};
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::{is_busy, SQLStore};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clocks. `Utc::now()` is never passed to `put_artist_in_tx` anywhere
/// in this file, so an implementation that stamped the commit clock would store a value no
/// test could name — and every "unchanged" assertion below would be vacuous.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap(),
    )
}

/// The scanner's save stamp: 2019, i.e. far OLDER than the commit clock of a test running
/// today, so a commit-clock stamp cannot accidentally satisfy a "stamped" assertion.
fn scan_stamp() -> DateTime<Utc> {
    at(2019, 1, 2, 3, 4, 5)
}

/// A stamp older than [`scan_stamp`]: the `updated_at` an external-info refresh must leave alone.
fn prior_stamp() -> DateTime<Utc> {
    at(2018, 5, 6, 7, 8, 9)
}

/// A stamp newer than every other stamp here, used for the external-info refresh.
fn external_stamp() -> DateTime<Utc> {
    at(2021, 9, 10, 11, 12, 13)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled (the
/// `runtime_folder_writes.rs:124-136` fixture shape).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-artist-writes-{label}-"),
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
    std::fs::remove_file(&path).unwrap();
}

/// Commit one artist Put through the typed seam exactly as a caller would: the caller's stamp
/// and the caller's typed column restriction.
fn put_committed(
    store: &SQLStore,
    record: &ArtistRecord,
    stamp: &DateTime<Utc>,
    cols: &[ArtistColumn],
) -> ArtistWriteResult {
    store
        .try_with_tx_sync::<ArtistWriteResult, ArtistWriteError>(|tx| {
            put_artist_in_tx(tx, record, stamp, cols)
        })
        .unwrap_or_else(|e| panic!("put_artist_in_tx({}) failed: {e}", record.id))
}

/// Commit an artist Put AND the DB-05 `library_artist` link in ONE transaction — Go's scanner
/// order (`scanner/phase_1_folders.go:401-412`). `Ok(false)` is the deliberate case: an empty
/// typed slice is Go's unfiltered save, which is what the very first insert leg needs.
fn put_and_link(
    store: &SQLStore,
    record: &ArtistRecord,
    stamp: &DateTime<Utc>,
    lib: i64,
    cols: &[ArtistColumn],
) -> ArtistWriteResult {
    store
        .try_with_tx_sync::<ArtistWriteResult, ArtistWriteError>(|tx| {
            let result = put_artist_in_tx(tx, record, stamp, cols)?;
            add_artist_in_tx(tx, lib, &result.id).map_err(escalate)?;
            Ok(result)
        })
        .unwrap_or_else(|e| panic!("put + add_artist_in_tx({}) failed: {e}", record.id))
}

/// `add_artist_in_tx`'s `Infallible` domain type escalated to the artist callback's error
/// type: `Infallible` has no inhabitant, so the `Domain` arm is unreachable.
fn escalate(err: DbOperationError<Infallible>) -> DbOperationError<ArtistWriteError> {
    match err {
        DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
        DbOperationError::Domain(unreachable) => match unreachable {},
    }
}

/// Run a raw block on one pooled lease, panicking on lease or statement failure.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (the poison trigger). Library rows come from DB-05's production
/// `put_in_tx` and artist rows from `put_artist_in_tx`, never a hand-built INSERT.
fn fixture_insert(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

/// One scalar probe (counts, `typeof`, raw stored text).
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// One nullable stored column of the artist row, read RAW. `hydrate::row_to_artist` drops
/// `full_text`/`search_normalized` (they have no destination field on `dbArtist`), so the
/// derived columns can only be asserted against the cell, not against a record field.
fn stored_column(store: &SQLStore, column: &str, id: &str) -> Option<String> {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT {column} FROM artist WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or_else(|e| panic!("{column} for {id}: {e}"))
    })
}

fn typeof_column(store: &SQLStore, column: &str, id: &str) -> String {
    lease(store, |conn| {
        conn.query_row(
            &format!("SELECT typeof({column}) FROM artist WHERE id = ?"),
            [text(id)],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|e| panic!("typeof {column} for {id}: {e}"))
    })
}

/// A DB-05 library row created through the PRODUCTION `put_in_tx` — this file's own rule:
/// library rows are never hand-built (`library.library_id` FKs are ON).
fn seed_library(store: &SQLStore, id: i64, tag: &str) {
    let record = LibraryRecord {
        id,
        name: format!("lib-{tag}"),
        path: format!("/lib-{tag}"),
        remote_path: String::new(),
        last_scan_at: prior_stamp(),
        last_scan_started_at: prior_stamp(),
        full_scan_in_progress: false,
        updated_at: prior_stamp(),
        created_at: prior_stamp(),
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
            put_in_tx(tx, &record, &prior_stamp(), &[] as &[LibraryColumn])
        })
        .unwrap_or_else(|e| panic!("seed_library({id}) failed: {e}"));
}

/// Count the `library_artist` link rows for one artist. Asserted BEFORE every `get` read-back:
/// `ArtistRepo::get` INNER-joins that table, so a missing junction turns a successful write
/// into `NotFound` and would make the read-back vacuous.
fn link_count(store: &SQLStore, artist_id: &str) -> i64 {
    scalar::<i64>(
        store,
        &format!("SELECT count(*) FROM library_artist WHERE artist_id = '{artist_id}'"),
    )
}

/// The production read side. Panics on a `NotFound`, which is exactly why [`link_count`]
/// belongs in front of every call.
fn read_back(store: &SQLStore, id: &str) -> ArtistRecord {
    lease(store, |conn| {
        ArtistRepo::new(conn)
            .get(id, &None, None)
            .unwrap_or_else(|e| {
                panic!(
                    "ArtistRepo::get({id}): {e} (link row present: {})",
                    link_count(store, id)
                )
            })
    })
}

/// An artist record whose every field carries a DIFFERENT value, so a rotated positional
/// argument list cannot land on a value that happens to match another column.
fn artist_record(id: &str) -> ArtistRecord {
    ArtistRecord {
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
        external_info_updated_at: Some(prior_stamp()),
        missing: false,
        uploaded_image: format!("upload-{id}"),
        created_at: Some(prior_stamp()),
        updated_at: Some(prior_stamp()),
        ..ArtistRecord::default()
    }
}

/// The extended SQLite code, when the failure carries one — the assertion that the seam kept
/// the concrete cause rather than a stringified wrapper.
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// Go's scanner column list (`scanner/phase_1_folders.go:403-405`) as the typed set.
fn scanner_columns() -> [ArtistColumn; 7] {
    [
        ArtistColumn::Name,
        ArtistColumn::MbzArtistId,
        ArtistColumn::SortArtistName,
        ArtistColumn::OrderArtistName,
        ArtistColumn::FullText,
        ArtistColumn::SearchNormalized,
        ArtistColumn::UpdatedAt,
    ]
}

/// Go's `UpdateExternalInfo` column list (`artist_repository.go:250-253`) as the typed set:
/// seven columns, `updated_at` NOT among them.
fn external_info_columns() -> [ArtistColumn; 7] {
    [
        ArtistColumn::Biography,
        ArtistColumn::SmallImageUrl,
        ArtistColumn::MediumImageUrl,
        ArtistColumn::LargeImageUrl,
        ArtistColumn::SimilarArtists,
        ArtistColumn::ExternalUrl,
        ArtistColumn::ExternalInfoUpdatedAt,
    ]
}

/// INSERT on a predefined-id miss writes every column, including the derived ones; the
/// scanner's 7-column selection then updates exactly those and leaves the rest of the row
/// alone. Production `put_artist_in_tx` + production `ArtistRepo::get` throughout.
#[test]
fn artist_put_insert_then_scanner_columns_update_only_their_own() {
    let (db, path) = fresh_store("scanner");
    seed_library(&db, 1, "scanner");

    // Act: the very first save of a brand-new artist with a predefined id. Go's `put` runs
    // the UPDATE (0 rows) and then the INSERT (`sql_base_repository.go:624-641`).
    let record = artist_record("ar-1");
    let first = put_and_link(&db, &record, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert!(
        first.inserted,
        "a predefined id that matches nothing takes the INSERT leg, not a NotFound"
    );
    assert_eq!(first.id, "ar-1");
    assert_eq!(
        link_count(&db, "ar-1"),
        1,
        "the link row exists, so get is meaningful"
    );

    // Assert: all columns stored, both stamps = the caller's stamp (`Put`, :240-241), the
    // derived columns present, and `external_info_updated_at` round-tripped.
    let got = read_back(&db, "ar-1");
    assert_eq!(got.name, "Name-ar-1");
    assert_eq!(got.sort_artist_name, "Sort-ar-1");
    assert_eq!(got.order_artist_name, "Order-ar-1");
    assert_eq!(got.mbz_artist_id, "mbz-ar-1");
    assert_eq!(got.biography, "Bio-ar-1");
    assert_eq!(got.uploaded_image, "upload-ar-1");
    assert_eq!(got.similar_artists, "[]");
    assert!(
        !got.missing,
        "missing is written from the record (Go emits it, model/artist.go:41)"
    );
    assert_eq!(
        got.created_at,
        Some(prior_stamp()),
        "the INSERT keeps the record's own created_at; the caller stamp is only the fallback \
         for a record that carries none"
    );
    assert_eq!(
        got.updated_at,
        Some(scan_stamp()),
        "unfiltered save carries updated_at (Put stamps CreatedAt = UpdatedAt)"
    );
    assert_eq!(
        stored_column(&db, "external_info_updated_at", "ar-1").as_deref(),
        Some(time::to_text(&prior_stamp()).as_str())
    );
    // The derived columns are `PostMapArgs`' work (`:109-110`), written by the same statement.
    assert_eq!(
        stored_column(&db, "full_text", "ar-1").as_deref(),
        Some(nd_db::artist_repo::format_full_text("Name-ar-1", "Sort-ar-1").as_str()),
        "full_text = formatFullText(name, sortArtistName) (`:109`), computed in the same statement"
    );
    assert!(
        !stored_column(&db, "search_normalized", "ar-1")
            .unwrap_or_default()
            .is_empty(),
        "NormalizeForFTS(name) is written, not left at the DDL default"
    );

    // Act: the scanner's 7-column save with a NEWER stamp, changing name and sort.
    let scanner_stamp = at(2020, 6, 7, 8, 9, 10);
    let edited = ArtistRecord {
        name: "Beatles".to_string(),
        sort_artist_name: "Beatles, The".to_string(),
        order_artist_name: "Order-ar-1".to_string(),
        mbz_artist_id: "mbz-changed".to_string(),
        ..record.clone()
    };
    let second = put_and_link(&db, &edited, &scanner_stamp, 1, &scanner_columns());
    assert!(!second.inserted, "the second save matched the UPDATE leg");
    assert_eq!(
        link_count(&db, "ar-1"),
        1,
        "the idempotent link did not duplicate"
    );

    // Assert: the selected columns moved…
    let again = read_back(&db, "ar-1");
    assert_eq!(again.name, "Beatles");
    assert_eq!(again.sort_artist_name, "Beatles, The");
    assert_eq!(again.mbz_artist_id, "mbz-changed");
    assert_eq!(
        again.updated_at,
        Some(scanner_stamp),
        "the scanner selects updated_at explicitly, so it moves to the caller's stamp"
    );
    assert_eq!(
        stored_column(&db, "full_text", "ar-1").as_deref(),
        Some(nd_db::artist_repo::format_full_text("Beatles", "Beatles, The").as_str()),
        "full_text is re-derived in the same statement, not left stale. The same authority the \
         module golden pins (`format_full_text(\"Eddie Van Halen\", \"Van Halen, Eddie\") == \
         \" eddie halen van\"`): sanitized, lowercased, deduplicated, sorted tokens."
    );
    // …and the columns OUTSIDE that selection are untouched, including biography.
    assert_eq!(
        again.biography, "Bio-ar-1",
        "biography is not in the scanner list"
    );
    assert_eq!(again.medium_image_url, "medium-ar-1");
    assert_eq!(again.uploaded_image, "upload-ar-1");
    assert_eq!(
        again.created_at,
        Some(prior_stamp()),
        "created_at is immutable on the UPDATE leg"
    );
    assert_eq!(
        stored_column(&db, "external_info_updated_at", "ar-1").as_deref(),
        Some(time::to_text(&prior_stamp()).as_str()),
        "external_info_updated_at is not in the scanner list"
    );

    cleanup(db, path);
}

/// `PostMapArgs` deletes an empty `sort_artist_name`/`mbz_artist_id` (`:113-120`), so an empty
/// value does not overwrite the stored one — asserted per column, and asserted to be
/// NON-vacuous by a non-empty record in the same test writing both columns.
#[test]
fn artist_empty_sort_and_mbz_do_not_clear_existing_values() {
    let (db, path) = fresh_store("empty-cols");
    seed_library(&db, 1, "empty");

    // Arrange: a row whose sort/mbz columns are non-empty.
    let full = artist_record("ar-1");
    put_and_link(&db, &full, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert_eq!(
        stored_column(&db, "sort_artist_name", "ar-1").as_deref(),
        Some("Sort-ar-1")
    );
    assert_eq!(
        stored_column(&db, "mbz_artist_id", "ar-1").as_deref(),
        Some("mbz-ar-1")
    );

    // Act: the scanner's save with BOTH values empty (Go's `structs.Map` emits them empty and
    // PostMapArgs deletes them, so they are absent from SET).
    let emptied = ArtistRecord {
        sort_artist_name: String::new(),
        mbz_artist_id: String::new(),
        ..full.clone()
    };
    let result = put_and_link(&db, &emptied, &scan_stamp(), 1, &scanner_columns());
    assert!(
        !result.inserted,
        "the UPDATE leg ran, so the surviving values are its output"
    );

    // Assert: the stored values survive.
    assert_eq!(
        stored_column(&db, "sort_artist_name", "ar-1").as_deref(),
        Some("Sort-ar-1"),
        "an empty sort_artist_name never overwrites the stored one"
    );
    assert_eq!(
        stored_column(&db, "mbz_artist_id", "ar-1").as_deref(),
        Some("mbz-ar-1"),
        "an empty mbz_artist_id never overwrites the stored one"
    );

    // Non-vacuity: the same selection WITH non-empty values does write them.
    let rewritten = ArtistRecord {
        sort_artist_name: "Beatles, The".to_string(),
        mbz_artist_id: "mbz-new".to_string(),
        ..full.clone()
    };
    put_and_link(&db, &rewritten, &scan_stamp(), 1, &scanner_columns());
    assert_eq!(
        stored_column(&db, "sort_artist_name", "ar-1").as_deref(),
        Some("Beatles, The"),
        "the column is writable, so the assertions above are not a frozen row"
    );
    assert_eq!(
        stored_column(&db, "mbz_artist_id", "ar-1").as_deref(),
        Some("mbz-new")
    );

    // An INSERT with empty values omits the columns entirely, so the DDL default lands.
    let fresh = ArtistRecord {
        id: "ar-2".to_string(),
        name: "Fresh".to_string(),
        sort_artist_name: String::new(),
        mbz_artist_id: String::new(),
        ..ArtistRecord::default()
    };
    let inserted = put_and_link(&db, &fresh, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert!(inserted.inserted);
    assert_eq!(
        stored_column(&db, "sort_artist_name", "ar-2").as_deref(),
        Some("")
    );
    assert_eq!(
        stored_column(&db, "mbz_artist_id", "ar-2").as_deref(),
        Some("")
    );

    cleanup(db, path);
}

/// Go parity, the point of DB-08: `UpdateExternalInfo`'s 7-column selection
/// (`artist_repository.go:250-253`) writes `external_info_updated_at` and leaves BOTH
/// `created_at` and `updated_at` at their stored values. `artist.updated_at` is the
/// `RefreshStats(allArtists=false)` cutoff (`:479`), so this is behavior, not cosmetics.
#[test]
fn artist_external_info_selection_advances_its_own_stamp_not_updated_at() {
    let (db, path) = fresh_store("external");
    seed_library(&db, 1, "external");

    // Arrange: the unfiltered INSERT stamps updated_at with the caller's clock (2019) and
    // keeps the record's created_at (2018). Both are OLDER than the refresh's stamp, and the
    // refresh must move neither.
    let record = artist_record("ar-1");
    put_and_link(&db, &record, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert_eq!(
        stored_column(&db, "updated_at", "ar-1").as_deref(),
        Some(time::to_text(&scan_stamp()).as_str()),
        "the stored updated_at is the scanner's stamp, which the refresh must preserve"
    );

    // Act: the refresh — biography + external_info_updated_at, `updated_at` NOT selected.
    let refreshed = ArtistRecord {
        biography: "Born in Pasadena".to_string(),
        external_info_updated_at: Some(external_stamp()),
        ..record.clone()
    };
    let result = put_and_link(
        &db,
        &refreshed,
        &external_stamp(),
        1,
        &external_info_columns(),
    );
    assert!(!result.inserted, "the existing row matched the UPDATE leg");

    // Assert: the selected columns moved.
    let got = read_back(&db, "ar-1");
    assert_eq!(got.biography, "Born in Pasadena");
    assert_eq!(
        got.external_info_updated_at,
        Some(external_stamp()),
        "the external provider's own throttle stamp advanced (`core/external/provider.go:235,286`)"
    );
    // Assert: BOTH row stamps stayed put — the divergence this lane removed.
    assert_eq!(
        stored_column(&db, "updated_at", "ar-1").as_deref(),
        Some(time::to_text(&scan_stamp()).as_str()),
        "a filtered save that does not select updated_at leaves it alone \
         (selectUpdateColumns, sql_base_repository.go:599-604)"
    );
    assert_eq!(
        stored_column(&db, "created_at", "ar-1").as_deref(),
        Some(time::to_text(&prior_stamp()).as_str()),
        "created_at is dropped from the UPDATE set (`:601`)"
    );
    assert_eq!(got.created_at, Some(prior_stamp()));
    assert_eq!(got.updated_at, Some(scan_stamp()));

    // Non-vacuity: selecting UpdatedAt in the same shape DOES move it.
    let mut columns = external_info_columns().to_vec();
    columns.push(ArtistColumn::UpdatedAt);
    put_and_link(&db, &refreshed, &external_stamp(), 1, &columns);
    assert_eq!(
        stored_column(&db, "updated_at", "ar-1").as_deref(),
        Some(time::to_text(&external_stamp()).as_str()),
        "the same record with UpdatedAt selected writes the stamp, so the assertion above \
         is about the selection and not about the record"
    );
    // The legacy &[&str] seam normalizes camelCase exactly as Go's toSnakeCase does (:596-598).
    let ((normalized, _), _) = put_sql(&refreshed, external_stamp(), &["UpdatedAt"]);
    assert!(normalized.contains("updated_at = ?"));

    cleanup(db, path);
}

/// Explicit-id miss → INSERT; second call → UPDATE; `created_at` identical across both;
/// `updated_at` moves only when requested; the caller's record is never mutated.
#[test]
fn artist_explicit_id_miss_inserts_then_updates_without_touching_the_input() {
    let (db, path) = fresh_store("insert-update");
    seed_library(&db, 1, "insert-update");

    let record = artist_record("ar-1");
    let before = record.clone();

    // Act 1: no row → INSERT leg, stamps from the caller's clock.
    let first = put_and_link(&db, &record, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert!(first.inserted);
    assert_eq!(
        stored_column(&db, "created_at", "ar-1").as_deref(),
        Some(time::to_text(&prior_stamp()).as_str()),
        "the INSERT keeps the record's own created_at rather than substituting now"
    );

    // Act 2: same id, `created_at` absent from the record → the INSERT fallback would use the
    // caller stamp, but the UPDATE leg runs first and `created_at` is dropped from SET.
    let unstamped = ArtistRecord {
        created_at: None,
        updated_at: None,
        biography: "Second".to_string(),
        ..record.clone()
    };
    let second = put_and_link(
        &db,
        &unstamped,
        &scan_stamp(),
        1,
        &[ArtistColumn::Biography],
    );
    assert!(!second.inserted, "the UPDATE leg matched, so no INSERT ran");
    let got = read_back(&db, "ar-1");
    assert_eq!(got.biography, "Second");
    assert_eq!(
        got.created_at,
        Some(prior_stamp()),
        "created_at is identical across the insert and the update"
    );
    assert_eq!(
        got.updated_at,
        Some(scan_stamp()),
        "a biography-only save does not stamp updated_at, so it stays at the INSERT's stamp"
    );

    // Assert: the input records were not mutated — the stamps travel as bound values only.
    assert_eq!(
        record, before,
        "put_artist_in_tx borrows and never mutates the record"
    );
    assert_eq!(unstamped.created_at, None);
    assert_eq!(unstamped.updated_at, None);

    // An INSERT of an unstamped record falls back to the caller's clock (Go, :240).
    let fresh = ArtistRecord {
        id: "ar-9".to_string(),
        name: "Fresh".to_string(),
        ..ArtistRecord::default()
    };
    let inserted = put_and_link(&db, &fresh, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert!(inserted.inserted);
    assert_eq!(
        stored_column(&db, "created_at", "ar-9").as_deref(),
        Some(time::to_text(&scan_stamp()).as_str()),
        "record-absent created_at falls back to the caller's stamp, never the commit clock"
    );
    assert_eq!(typeof_column(&db, "created_at", "ar-9"), "text");

    cleanup(db, path);
}

/// A selection that resolves to nothing settable runs Go's no-op UPDATE
/// (`filterUpdateValues` re-adds `id`, `sql_base_repository.go:612-616`) rather than emitting
/// `SET  WHERE` or inventing a domain error: 1 row affected for an existing id (`inserted:
/// false`, nothing written), 0 for a missing one (full INSERT). Executed in both orders.
#[test]
fn artist_empty_resolved_selection_is_the_go_noop_update_not_invalid_sql() {
    let (db, path) = fresh_store("noop");
    seed_library(&db, 1, "noop");

    // Arrange: a row with a non-empty sort_artist_name, and a record whose value is EMPTY, so
    // the only requested column is dropped by PostMapArgs and the SET resolves to nothing.
    let full = artist_record("ar-1");
    put_and_link(&db, &full, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    let snapshot: Vec<Option<String>> = ["name", "biography", "sort_artist_name", "updated_at"]
        .iter()
        .map(|c| stored_column(&db, c, "ar-1"))
        .collect();
    let emptied = ArtistRecord {
        sort_artist_name: String::new(),
        ..full.clone()
    };

    // Act: the typed selection is `SortArtistName` alone → SET empty → Go's `SET id = ?`.
    let result = put_committed(
        &db,
        &emptied,
        &external_stamp(),
        &[ArtistColumn::SortArtistName],
    );
    assert!(
        !result.inserted,
        "the no-op UPDATE matched 1 row (executed parity: sqlite3 reports 1 for an existing id)"
    );

    // Assert: nothing changed, including the stamps, and the row count is still 1.
    let after: Vec<Option<String>> = ["name", "biography", "sort_artist_name", "updated_at"]
        .iter()
        .map(|c| stored_column(&db, c, "ar-1"))
        .collect();
    assert_eq!(after, snapshot, "the no-op UPDATE wrote nothing");
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artist"), 1);

    // Act: the same empty selection on a MISSING id → 0 rows → full INSERT, no error.
    let missing = ArtistRecord {
        id: "ar-new".to_string(),
        name: "Missing Artist".to_string(),
        sort_artist_name: String::new(),
        mbz_artist_id: String::new(),
        ..ArtistRecord::default()
    };
    let inserted = put_committed(
        &db,
        &missing,
        &scan_stamp(),
        &[ArtistColumn::SortArtistName],
    );
    assert!(
        inserted.inserted,
        "Go's INSERT fallback runs on the zero-row no-op UPDATE"
    );
    assert_eq!(
        stored_column(&db, "name", "ar-new").as_deref(),
        Some("Missing Artist")
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artist"), 2);

    // The legacy &[&str] renderer expresses the same case with `created_at` alone (Go's own
    // colsToUpdate is free-form text), and its statement is executable, not degenerate.
    let ((noop_sql, noop_args), _) = put_sql(&full, external_stamp(), &["created_at"]);
    assert_eq!(noop_sql, "UPDATE artist SET id = ? WHERE id = ?");
    assert_eq!(noop_args.len(), 2, "SET id binds once, WHERE id binds once");
    let matched = lease(&db, |conn| {
        conn.execute(&noop_sql, rusqlite::params_from_iter(noop_args.iter()))
            .expect("the no-op UPDATE executes against the real schema")
    });
    assert_eq!(
        matched, 1,
        "RowsAffected parity with Go's executeSQL count for an existing id"
    );

    // An empty typed slice is NOT the no-op: it is Go's unfiltered save (all columns).
    let unfiltered = put_committed(&db, &full, &external_stamp(), &[] as &[ArtistColumn]);
    assert!(!unfiltered.inserted);
    assert_eq!(
        stored_column(&db, "updated_at", "ar-1").as_deref(),
        Some(time::to_text(&external_stamp()).as_str()),
        "the unfiltered SET includes updated_at (Go's Put stamps it)"
    );

    cleanup(db, path);
}

/// `ArtistRepo::get` INNER-joins `library_artist`, so an artist Put with no junction row is
/// INVISIBLE (`artist_repository.go:191-203`, `:255-263`). Pinned deliberately, once, so
/// every other read-back here — which asserts the link row first — cannot pass vacuously.
#[test]
fn artist_without_a_link_row_is_invisible_to_get() {
    let (db, path) = fresh_store("no-link");
    seed_library(&db, 1, "no-link");

    let record = artist_record("ar-orphan");
    let result = put_committed(&db, &record, &scan_stamp(), &[] as &[ArtistColumn]);
    assert!(result.inserted, "the row is written");
    assert_eq!(
        stored_column(&db, "name", "ar-orphan").as_deref(),
        Some("Name-ar-orphan"),
        "the artist row exists at the raw level"
    );
    assert_eq!(link_count(&db, "ar-orphan"), 0);

    let error = lease(&db, |conn| {
        ArtistRepo::new(conn).get("ar-orphan", &None, None)
    })
    .expect_err("no junction row means the INNER join matches nothing");
    assert!(
        matches!(error, ArtistError::NotFound),
        "the invisible-write case is NotFound, not a statement failure: {error:?}"
    );

    // Link it, and the same read succeeds — proving the failure above came from the join.
    db.try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, 1, "ar-orphan"))
        .expect("add_artist_in_tx");
    assert_eq!(link_count(&db, "ar-orphan"), 1);
    assert_eq!(read_back(&db, "ar-orphan").name, "Name-ar-orphan");

    cleanup(db, path);
}

/// A link failure in the caller's transaction rolls back the EARLIER artist Put, keeps the
/// concrete `SQLITE_CONSTRAINT_TRIGGER` cause and the `Body` stage, is not classified as busy,
/// and publishes no result. Fixture trigger only; `nd-dbschema` untouched.
#[test]
fn artist_link_trigger_failure_rolls_back_the_earlier_put() {
    let (db, path) = fresh_store("rollback");
    seed_library(&db, 1, "rollback");
    let poison = "ar-poisoned";
    // SQLite forbids bound variables inside a trigger body, so the id is spelled in the DDL.
    fixture_insert(
        &db,
        &format!(
            "CREATE TRIGGER fixture_artist_link_fails BEFORE INSERT ON library_artist \
             FOR EACH ROW WHEN NEW.artist_id = '{poison}' \
             BEGIN SELECT RAISE(ABORT, 'fixture: artist link leg fails'); END"
        ),
        &[],
    );

    let record = artist_record(poison);
    let failed = db.try_with_tx_sync::<ArtistWriteResult, ArtistWriteError>(|tx| {
        let result = put_artist_in_tx(tx, &record, &scan_stamp(), &[] as &[ArtistColumn])?;
        add_artist_in_tx(tx, 1, &result.id).map_err(escalate)?;
        Ok(result)
    });
    let err = failed.expect_err("the link leg aborts the transaction");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_CONSTRAINT_TRIGGER),
                "the raw SQLite cause survives the seam, unwrapped (Go adds no fmt.Errorf, :243-244)"
            );
            assert!(!is_busy(source), "a constraint failure is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM artist"),
        0,
        "the artist row written before the failure rolled back"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library_artist"), 0);
    assert!(
        matches!(err, DbRuntimeError::Sqlite { .. }),
        "the failed Put's ArtistWriteResult is advisory: it was never published"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable");
    });

    // Drop the fixture trigger and re-run the same write: it succeeds with the same id.
    fixture_insert(&db, "DROP TRIGGER fixture_artist_link_fails", &[]);
    let ok = put_and_link(&db, &record, &scan_stamp(), 1, &[] as &[ArtistColumn]);
    assert!(ok.inserted);
    assert_eq!(ok.id, poison);
    assert_eq!(link_count(&db, poison), 1);
    assert_eq!(read_back(&db, poison).name, format!("Name-{poison}"));

    cleanup(db, path);
}

/// An empty id is refused before any statement runs — a DOCUMENTED deviation from Go, which
/// generates a random id (`sql_base_repository.go:635-638`). No row, no link, no panic.
#[test]
fn artist_empty_id_is_refused_before_any_statement() {
    let (db, path) = fresh_store("empty-id");
    seed_library(&db, 1, "empty-id");

    let record = ArtistRecord {
        id: String::new(),
        name: "No Id".to_string(),
        ..ArtistRecord::default()
    };
    let failed = db.try_with_tx_sync::<ArtistWriteResult, ArtistWriteError>(|tx| {
        put_artist_in_tx(tx, &record, &scan_stamp(), &[] as &[ArtistColumn])
    });
    let err = failed.expect_err("an empty id is a contract violation, not a random id");
    assert!(
        matches!(err, DbRuntimeError::Domain(ArtistWriteError::EmptyId)),
        "the domain arm carries the concrete cause: {err:?}"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM artist"),
        0,
        "Go's random-id INSERT never ran"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library_artist"), 0);

    cleanup(db, path);
}

/// Adversarial names and unicode are BOUND, never interpolated: no injection, and the derived
/// `full_text`/`search_normalized` stay the deterministic `PostMapArgs` outputs. The FTS index
/// is maintained by the schema's own triggers (`nd-dbschema/src/schema_ddl.rs:88-90`) — this
/// lane writes no FTS statement — and one artist Put is one statement pair, never chunked.
#[test]
fn artist_adversarial_and_unicode_names_are_bound_and_fts_consistent() {
    let (db, path) = fresh_store("unicode");
    seed_library(&db, 1, "unicode");

    // Values that would break a naive string-interpolated statement.
    let tricky = "'; DROP TABLE artist; -- \"\\%\u{130}Σ";
    let cases: &[(&str, &str)] = &[
        ("ar-quote", tricky),
        ("ar-sigma", "ΑΒΓΣ"),
        ("ar-diacritic", "Bjørk Ångström"),
    ];

    for (id, name) in cases {
        let record = ArtistRecord {
            id: (*id).to_string(),
            name: (*name).to_string(),
            sort_artist_name: String::new(),
            mbz_artist_id: String::new(),
            ..ArtistRecord::default()
        };
        let result = put_and_link(&db, &record, &scan_stamp(), 1, &scanner_columns());
        assert!(result.inserted, "{id}: first save takes the INSERT leg");
        assert_eq!(link_count(&db, id), 1);
        assert_eq!(
            stored_column(&db, "name", id).as_deref(),
            Some(*name),
            "the value is bound verbatim, quotes and backslashes intact"
        );
        // The derived columns are the port's own authorities, unchanged by this lane.
        assert_eq!(
            stored_column(&db, "full_text", id).as_deref(),
            Some(nd_db::artist_repo::format_full_text(name, "").as_str()),
            "full_text = format_full_text(name, sort) in the same statement"
        );
        assert_eq!(
            stored_column(&db, "search_normalized", id).as_deref(),
            Some(nd_db::artist_repo::search_normalized(name).as_str()),
            "search_normalized = NormalizeForFTS(name)"
        );
        assert_eq!(
            stored_column(&db, "external_info_updated_at", id),
            None,
            "Go's nil *time.Time is NULL, not the old 0 sentinel"
        );
    }

    // The table survived the injection attempt, and each row is one row.
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM artist"), 3);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library_artist"), 3);

    // FTS consistency through the schema triggers only: a name that tokenizes to `drop` and
    // `table` is findable, and the raw unicode row is present with its own rowid.
    let fts_rows: Vec<i64> = lease(&db, |conn| {
        let mut stmt = conn
            .prepare("SELECT rowid FROM artist_fts WHERE artist_fts MATCH ? ORDER BY rowid")
            .expect("prepare FTS match");
        stmt.query_map([text("drop")], |row| row.get::<_, i64>(0))
            .expect("query FTS")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("FTS rows")
    });
    assert_eq!(
        fts_rows.len(),
        1,
        "exactly the injected-name row matches `drop`"
    );
    let expected_rowid = scalar::<i64>(&db, "SELECT rowid FROM artist WHERE id = 'ar-quote'");
    assert_eq!(fts_rows[0], expected_rowid, "the trigger indexed that row");

    // An UPDATE that changes the name re-indexes through the SAME trigger pair (no explicit
    // FTS write anywhere in this lane), which is what keeps the index consistent.
    let renamed = ArtistRecord {
        id: "ar-quote".to_string(),
        name: "Renamed Artist".to_string(),
        ..ArtistRecord::default()
    };
    put_and_link(&db, &renamed, &scan_stamp(), 1, &scanner_columns());
    let stale: Vec<i64> = lease(&db, |conn| {
        let mut stmt = conn
            .prepare("SELECT rowid FROM artist_fts WHERE artist_fts MATCH ?")
            .expect("prepare");
        stmt.query_map([text("drop")], |row| row.get::<_, i64>(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows")
    });
    assert!(
        stale.is_empty(),
        "the old cell was deleted from the index by the UPDATE trigger"
    );
    assert_eq!(
        stored_column(&db, "name", "ar-quote").as_deref(),
        Some("Renamed Artist")
    );

    cleanup(db, path);
}

/// The legacy `&[&str]` renderer stays compatible: camelCase entries normalize like Go's
/// `toSnakeCase` (`sql_base_repository.go:596-598`), a `created_at`-only selection is Go's
/// valid no-op UPDATE, and no caller text reaches the SET identifiers.
#[test]
fn artist_legacy_renderer_selections_stay_compatible() {
    let record = ArtistRecord {
        id: "ar-1".to_string(),
        name: "X".to_string(),
        biography: "b".to_string(),
        ..ArtistRecord::default()
    };
    // Go's exact external-info list (the public const) renders the 7 columns, no updated_at.
    let ((update, args), (insert, _)) = put_sql(&record, scan_stamp(), &EXTERNAL_INFO_COLS);
    assert!(
        !update.contains(" updated_at = ?"),
        "the 7-column external-info SET carries no updated_at column \
         (external_info_updated_at is a different column)"
    );
    assert_eq!(args.len(), 8);
    assert!(insert.contains("updated_at"), "the INSERT leg still stamps");

    // An unmatched entry cannot inject SQL: identifiers come from put_cols only.
    let ((injection, _), _) = put_sql(&record, scan_stamp(), &["name) WHERE 1=1; --"]);
    assert_eq!(injection, "UPDATE artist SET id = ? WHERE id = ?");
}
