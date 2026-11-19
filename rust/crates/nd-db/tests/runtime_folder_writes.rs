//! DB-07 — the EXECUTED folder Put and the executed missing/touch primitives
//! (`persistence/folder_repository.go:255-274`, `mediafile_repository.go:455-489`,
//! `album_repository.go:360-387`) through the DB-00 typed transaction seam.
//!
//! Go truth under test, SOURCE-VERIFIED at the pinned lines and then EXECUTED-Rust
//! against a fresh schema (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp
//! file per invocation, `_foreign_keys=on` — Go's own DSN parameter, `consts/consts.go:14`):
//! * folder `Put` (`folder_repository.go:255-259`) is the `sql_base_repository.go:618-642`
//!   upsert — UPDATE by id FIRST, INSERT only on a zero-row match — and writes the
//!   CALLER's `UpdateAt`, stamped at entry-construction time
//!   (`scanner/folder_entry.go:94-103` → `model.NewFolder`, `model/folder.go:65-74`),
//!   never at commit time. `created_at` is immutable on the UPDATE leg
//!   (`selectUpdateColumns`, `sql_base_repository.go:593-610`).
//! * folder `MarkMissing` (`:261-274`) checks `err` ONLY and discards the count, so a
//!   zero-row chunk is a successful no-op and the loop continues.
//! * `MediaFile.MarkMissing` (`mediafile_repository.go:455-470`) has the
//!   `if err != nil || c == 0 { return err }` quirk (`:463-466`): with `err == nil` that
//!   returns NIL — SUCCESS — while also EXITING the chunk loop, so a zero-row chunk
//!   silently skips every LATER chunk. Pinned in both orders by
//!   `media_mark_missing_zero_row_first_chunk_returns_ok_and_skips_later_chunks`.
//! * `MediaFile.MarkMissingByFolder` (`:472-489`) carries the `Eq{"missing": !missing}`
//!   gate (`:479`), so already-flagged tracks are neither re-stamped nor counted, and
//!   checks `err` only like the folder variant — a zero-row FIRST chunk is a successful
//!   no-op whose loop CONTINUES, the opposite of the direct-media stop. Both shapes are
//!   pinned with 200 unknown ids followed by a real 201st id, in both flag directions and
//!   both orders, by `mark_media_missing_by_folder_gates_and_touch_album_ids_are_scoped`
//!   and the folder zero-first block of `missing_primitives_chunk_at_200_and_flip_both_directions`.
//! * `Album.Touch` (`album_repository.go:360-373`) is scoped to the SUPPLIED ids with no
//!   library predicate; `Album.TouchByMissingFolder` (`:376-387`) is GLOBAL — no id list,
//!   no library predicate — so it reaches albums of OTHER libraries whose `folder_ids`
//!   reference a missing folder. Both scopes are separate functions here and neither is
//!   narrowed (`nav-db07-db08-decisions.md` §A3, parent decision).
//! * Folder reads keep Go's write/read asymmetry: `hydrate::row_to_folder` returns the Go
//!   zero `updated_at` because `ScanStruct` maps by field name, while
//!   `get_folder_update_info` reads the stored column.
//!
//! Every assertion runs a PRODUCTION `*_in_tx` function through
//! `SQLStore::try_with_tx_sync` (never a hand-written UPDATE under test), reads back
//! through the production `FolderRepo`, and asserts per-COLUMN rather than a snapshot
//! string, so a rotated bind order fails rather than passing vacuously. Library rows are
//! seeded through DB-05's production `put_in_tx`, never a hand-built INSERT. There is no
//! durable Go oracle for folder Put / mark-missing / touch (`testdata/` holds none;
//! `docs/rust-port/subplans/db-runtime.md` DB-07 point 8), so nothing here is labeled a
//! Go golden — claims are SRC plus EXECUTED-Rust.
//!
//! Chunk counts come from the RENDERER the wrapper executes (`mark_missing_sql` and
//! friends return one `Sql` per chunk, and each wrapper runs exactly that Vec), never
//! from an inference off affected-row counts: `0/1/1/2` chunks for `0/1/200/201` ids is
//! measured, and both chunks' effects are asserted on their own rows with distinct
//! stamps. rusqlite's `hooks` feature (an authorizer-based statement counter) is NOT
//! enabled in this crate's manifest and this lane may not edit it, so statement counts
//! are taken from the renderer length — recorded as a coverage gap in the lane report.
//!
//! Test-local DDL (the poisoned-id trigger) is allowed as in
//! `runtime_library_writes.rs:768-776`; SQLite forbids bound variables in a trigger body,
//! so the poisoned id is spelled in the DDL. `nd-dbschema` is untouched.

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::album_repo::{
    touch_albums_in_tx, touch_albums_with_missing_folders_globally_in_tx, touch_sql,
    AlbumTouchError,
};
use nd_db::folder_repo::{
    mark_folders_missing_in_tx, mark_missing_sql, put_folder_in_tx, FolderError, FolderMarkError,
    FolderRecord, FolderRepo, FolderUpdateInfo, FolderWriteError, FolderWriteResult,
};
use nd_db::library_repo::{put_in_tx, LibraryColumn, LibraryRecord, LibraryWriteError};
use nd_db::mediafile_repo::{
    mark_media_missing_by_folder_in_tx, mark_media_missing_in_tx, mark_missing_by_folder_sql,
    mark_missing_sql as media_mark_missing_sql, MediaMarkError,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::{is_busy, SQLStore};
use nd_db::{marshal, time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clocks. `Utc::now()` is never passed to a wrapper anywhere in
/// this file, so a wrapper that stamped the commit clock would store a value no test
/// could name.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, min, s, nanos)
            .unwrap(),
    )
}

/// The scan stamp the Put is asked to persist: 2019, i.e. far OLDER than the commit
/// clock of a test running today. Non-vacuity for the caller-stamp rule.
fn scan_stamp() -> DateTime<Utc> {
    at(2019, 1, 2, 3, 4, 5, 6_000_000)
}

/// A stamp that is neither the scan stamp nor any other stamp in this file, so an
/// "unchanged" assertion cannot be satisfied by an accidental collision.
fn prior_stamp() -> DateTime<Utc> {
    at(2018, 5, 6, 7, 8, 9, 10_000_000)
}

fn other_stamp() -> DateTime<Utc> {
    at(2021, 9, 10, 11, 12, 13, 14_000_000)
}

fn flip_stamp() -> DateTime<Utc> {
    at(2022, 3, 4, 5, 6, 7, 8_000_000)
}

fn unflip_stamp() -> DateTime<Utc> {
    at(2023, 4, 5, 6, 7, 8, 9_000_000)
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled (the
/// `runtime_library_writes.rs:62-74` fixture shape).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-folder-writes-{label}-"),
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

/// Commit one folder Put through the typed seam exactly as a caller would: the caller's
/// scan stamp and the caller's column restriction.
fn put_committed(
    store: &SQLStore,
    record: &FolderRecord,
    stamp: &DateTime<Utc>,
    cols: &[&str],
) -> FolderWriteResult {
    store
        .try_with_tx_sync::<FolderWriteResult, FolderWriteError>(|tx| {
            put_folder_in_tx(tx, record, stamp, cols)
        })
        .unwrap_or_else(|e| panic!("put_folder_in_tx({}) failed: {e}", record.id))
}

/// Commit one folder `MarkMissing`, returning the affected-row total.
fn folders_missing(
    store: &SQLStore,
    missing: bool,
    ids: &[String],
    stamp: &DateTime<Utc>,
) -> usize {
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| -> Result<usize, FolderMarkError> {
            mark_folders_missing_in_tx(tx, missing, ids, stamp)
        })
        .unwrap_or_else(|e| panic!("mark_folders_missing_in_tx failed: {e}"))
}

/// Commit one media-file `MarkMissing`, returning the affected-row total.
fn media_missing(store: &SQLStore, missing: bool, ids: &[String], stamp: &DateTime<Utc>) -> usize {
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| -> Result<usize, MediaMarkError> {
            mark_media_missing_in_tx(tx, missing, ids, stamp)
        })
        .unwrap_or_else(|e| panic!("mark_media_missing_in_tx failed: {e}"))
}

/// Commit one `MarkMissingByFolder`, returning the affected-row total.
fn media_missing_by_folder(
    store: &SQLStore,
    missing: bool,
    folder_ids: &[String],
    stamp: &DateTime<Utc>,
) -> usize {
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| -> Result<usize, MediaMarkError> {
            mark_media_missing_by_folder_in_tx(tx, missing, folder_ids, stamp)
        })
        .unwrap_or_else(|e| panic!("mark_media_missing_by_folder_in_tx failed: {e}"))
}

/// Commit one scoped `Touch`, returning the affected-row total.
fn touch_ids(store: &SQLStore, ids: &[String], stamp: &DateTime<Utc>) -> usize {
    // The closure's annotation is the alias itself: `AlbumTouchError` IS
    // `DbOperationError<Infallible>`, so the executor's E is Infallible and any failure
    // this helper reports is a SQLite failure.
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| -> Result<usize, AlbumTouchError> {
            touch_albums_in_tx(tx, ids, stamp)
        })
        .unwrap_or_else(|e| panic!("touch_albums_in_tx failed: {e}"))
}

/// Commit the GLOBAL `TouchByMissingFolder`, returning the affected-row total.
fn touch_globally(store: &SQLStore, stamp: &DateTime<Utc>) -> usize {
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| -> Result<usize, AlbumTouchError> {
            touch_albums_with_missing_folders_globally_in_tx(tx, stamp)
        })
        .unwrap_or_else(|e| panic!("touch_albums_with_missing_folders_globally_in_tx failed: {e}"))
}

/// Run a raw block on one pooled lease, panicking on lease or statement failure.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (album/media_file rows, poisoned-id triggers). Folder rows
/// come from `put_folder_in_tx` and library rows from DB-05's `put_in_tx`, never from a
/// hand-built folder/library INSERT.
fn fixture_insert(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

/// Run several fixture inserts in ONE transaction, so a 200-row seed does not cost 200
/// autocommit transactions. A failure here aborts the seed outright.
fn fixture_batch(store: &SQLStore, statements: &[(String, Vec<SqlValue>)]) {
    lease(store, |conn| {
        // One explicit transaction so a 200-row seed is not 200 autocommit writes.
        conn.execute("BEGIN", []).expect("fixture BEGIN");
        for (sql, args) in statements {
            match conn.execute(sql.as_str(), rusqlite::params_from_iter(args.iter())) {
                Ok(_) => {}
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    panic!("{sql} --{args:?}--: {e}");
                }
            }
        }
        conn.execute("COMMIT", []).expect("fixture COMMIT");
    });
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
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

/// A DB-05 library row created through the PRODUCTION `put_in_tx` — this file's own rule
/// (copied from `runtime_library_writes.rs:84-85`): library rows are never hand-built.
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

/// Minimal `album` rows for the touch primitives (production album writes are DB-09).
/// `folder_ids` is raw TEXT so a fixture can spell `'null'`, `'[]'`, a nested array, or a
/// malformed cell; `None` stores SQL NULL.
fn album_fixture(id: &str, library_id: i64, folder_ids: Option<&str>) -> (String, Vec<SqlValue>) {
    (
        "INSERT INTO album (id, name, library_id, folder_ids, imported_at) VALUES (?, ?, ?, ?, ?)"
            .to_string(),
        vec![
            text(id),
            text(id),
            integer(library_id),
            folder_ids.map_or(SqlValue::Null, text),
            text(&time::to_text(&prior_stamp())),
        ],
    )
}

fn seed_albums(store: &SQLStore, rows: &[(String, i64, Option<&str>)]) {
    fixture_batch(
        store,
        &rows
            .iter()
            .map(|(id, lib, folders)| album_fixture(id, *lib, folders.as_deref()))
            .collect::<Vec<_>>(),
    );
}

/// Minimal `media_file` rows for the missing primitives (production media writes are
/// DB-10). `missing`/`updated_at` are spelled explicitly so "unchanged" is measurable.
fn media_fixture(id: &str, library_id: i64, folder_id: &str) -> (String, Vec<SqlValue>) {
    (
        "INSERT INTO media_file (id, title, library_id, folder_id, missing, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 0, ?, ?)"
            .to_string(),
        vec![
            text(id),
            text(id),
            integer(library_id),
            text(folder_id),
            text(&time::to_text(&prior_stamp())),
            text(&time::to_text(&prior_stamp())),
        ],
    )
}

fn seed_media(store: &SQLStore, rows: &[(String, i64, String)]) {
    fixture_batch(
        store,
        &rows
            .iter()
            .map(|(id, lib, folder)| media_fixture(id, *lib, folder))
            .collect::<Vec<_>>(),
    );
}

/// A folder row written through the PRODUCTION Put (in one transaction for the whole
/// batch), optionally flagged missing through the PRODUCTION `mark_folders_missing_in_tx`
/// in a second transaction.
fn seed_folders(store: &SQLStore, rows: &[(&str, i64)], missing: &[String]) {
    store
        .try_with_tx_sync::<usize, FolderWriteError>(|tx| {
            let mut count = 0usize;
            for (id, library_id) in rows {
                put_folder_in_tx(tx, &folder_record(id, *library_id), &prior_stamp(), &[])?;
                count += 1;
            }
            Ok(count)
        })
        .unwrap_or_else(|e| panic!("seed_folders failed: {e}"));
    if !missing.is_empty() {
        folders_missing(store, true, missing, &prior_stamp());
    }
}

/// The stored `missing` flag of one row.
fn flag(store: &SQLStore, table: &str, id: &str) -> i64 {
    scalar::<i64>(
        store,
        &format!("SELECT missing FROM {table} WHERE id = '{id}'"),
    )
}

/// The stored timestamp of one row, as canonical TEXT.
fn stamp_of(store: &SQLStore, table: &str, column: &str, id: &str) -> String {
    scalar::<String>(
        store,
        &format!("SELECT {column} FROM {table} WHERE id = '{id}'"),
    )
}

/// The stored TEXT of one row; a SQL NULL cell comes back as an explicit marker so a
/// test can tell NULL from the empty string.
fn cell_text(store: &SQLStore, table: &str, column: &str, id: &str) -> String {
    scalar::<String>(
        store,
        &format!("SELECT coalesce({column}, '<NULL>') FROM {table} WHERE id = '{id}'"),
    )
}

/// `FolderRepo::get` through the production read side on a leased connection.
fn get_folder(store: &SQLStore, id: &str) -> Result<FolderRecord, FolderError> {
    lease(store, |conn| FolderRepo::new(conn).get(id, None))
}

/// `FolderRepo::get_folder_update_info` through the production read side — the read that
/// DOES see the stored `updated_at`.
fn update_info(
    store: &SQLStore,
    lib_id: i64,
) -> std::collections::BTreeMap<String, FolderUpdateInfo> {
    lease(store, |conn| {
        FolderRepo::new(conn)
            .get_folder_update_info(lib_id, &[])
            .unwrap_or_else(|e| panic!("get_folder_update_info({lib_id}): {e}"))
    })
}

/// The extended SQLite code, when the failure carries one — the assertion that the seam
/// kept the concrete cause rather than a stringified wrapper.
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// A folder record whose every field carries a DIFFERENT value, so a rotated positional
/// argument list cannot land on a value that happens to match another column.
/// `created_at`/`updated_at` are set deliberately: the Put overrides `updated_at` and
/// keeps the record's `created_at` on the UPDATE leg.
fn folder_record(id: &str, library_id: i64) -> FolderRecord {
    FolderRecord {
        id: id.to_string(),
        library_id,
        path: format!("path-{id}"),
        name: format!("name-{id}"),
        parent_id: format!("parent-{id}"),
        num_audio_files: 11,
        num_playlists: 22,
        image_files: Some(vec![format!("cover-{id}.jpg")]),
        images_updated_at: at(2020, 1, 2, 3, 4, 5, 111_000_000),
        hash: format!("hash-{id}"),
        missing: false,
        updated_at: at(2020, 1, 2, 3, 4, 6, 222_000_000),
        created_at: Some(at(2020, 1, 2, 3, 4, 7, 333_000_000)),
        library_path: String::new(),
    }
}

fn ids(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}-{i}")).collect()
}

/// INSERT leg then UPDATE leg, every field read from its OWN column, and the caller's
/// stamp surviving even though it predates the commit clock by years.
#[test]
fn folder_put_preserves_caller_stamp_and_created_hash_images() {
    let (db, path) = fresh_store("put");
    seed_library(&db, 1, "one");
    let stamp = scan_stamp();

    // Act 1: no row exists → Go's INSERT arm (`sql_base_repository.go:635-641`).
    let record = folder_record("f-put", 1);
    let inserted = put_committed(&db, &record, &stamp, &[]);
    assert_eq!(
        inserted,
        FolderWriteResult {
            id: "f-put".to_string(),
            inserted: true,
        },
        "an update-miss is the insert fallback, not a miss"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM folder"), 1);
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-put"),
        time::to_text(&stamp)
    );
    assert_eq!(
        stamp_of(&db, "folder", "created_at", "f-put"),
        time::to_text(&record.created_at.expect("the record carries created_at")),
        "the record's own created_at is stored, not a clock this function reads"
    );
    // Every other field lands in its own column (rotation detector).
    assert_eq!(
        scalar::<i64>(&db, "SELECT library_id FROM folder WHERE id='f-put'"),
        1
    );
    assert_eq!(cell_text(&db, "folder", "path", "f-put"), "path-f-put");
    assert_eq!(cell_text(&db, "folder", "name", "f-put"), "name-f-put");
    assert_eq!(
        cell_text(&db, "folder", "parent_id", "f-put"),
        "parent-f-put"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT num_audio_files FROM folder WHERE id='f-put'"),
        11
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT num_playlists FROM folder WHERE id='f-put'"),
        22
    );
    assert_eq!(cell_text(&db, "folder", "hash", "f-put"), "hash-f-put");
    assert_eq!(
        cell_text(&db, "folder", "image_files", "f-put"),
        r#"["cover-f-put.jpg"]"#
    );
    assert_eq!(
        stamp_of(&db, "folder", "images_updated_at", "f-put"),
        time::to_text(&record.images_updated_at),
        "images_updated_at round-trips exactly, including its fractional seconds"
    );
    assert_eq!(
        flag(&db, "folder", "f-put"),
        0,
        "missing written from the record"
    );

    // Act 2: an existing row → UPDATE leg; `image_files: None` is Go's nil arm, which
    // PostMapArgs renders as the literal "[]" and never NULL.
    let mut no_images = folder_record("f-put", 1);
    no_images.image_files = None;
    no_images.hash = "hash-updated".to_string();
    no_images.num_audio_files = 33;
    no_images.created_at = Some(at(2030, 12, 31, 23, 59, 59, 0)); // must NOT land
    no_images.missing = true;
    let updated = put_committed(&db, &no_images, &stamp, &[]);
    assert_eq!(
        updated,
        FolderWriteResult {
            id: "f-put".to_string(),
            inserted: false,
        },
        "the UPDATE leg matched a row, so no INSERT ran"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder"),
        1,
        "the update leg must not add a second row"
    );
    assert_eq!(
        cell_text(&db, "folder", "image_files", "f-put"),
        "[]",
        "Go's PostMapArgs nil arm is the literal JSON array, not NULL"
    );
    assert_eq!(cell_text(&db, "folder", "hash", "f-put"), "hash-updated");
    assert_eq!(
        scalar::<i64>(&db, "SELECT num_audio_files FROM folder WHERE id='f-put'"),
        33
    );
    assert_eq!(
        flag(&db, "folder", "f-put"),
        1,
        "missing flips from the record"
    );
    assert_eq!(
        stamp_of(&db, "folder", "created_at", "f-put"),
        time::to_text(&record.created_at.expect("created_at")),
        "created_at is immutable on the UPDATE leg (selectUpdateColumns drops it)"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-put"),
        time::to_text(&stamp),
        "the CALLER's 2019 scan stamp is stored although the commit clock is later"
    );
    assert!(
        stamp < Utc::now(),
        "non-vacuity: the stored stamp predates now(), so commit-clock stamping would \
         have produced a different cell"
    );

    // Act 3: a record with no created_at gets the caller's stamp on the INSERT leg
    // (Go's `NewFolder` arm, model/folder.go:65-74).
    let mut unstamped = folder_record("f-unstamped", 1);
    unstamped.created_at = None;
    put_committed(&db, &unstamped, &stamp, &[]);
    assert_eq!(
        stamp_of(&db, "folder", "created_at", "f-unstamped"),
        time::to_text(&stamp),
        "created_at falls back to the caller's stamp when the record carries none"
    );

    // Act 4: a restricted selection writes ONLY the selected column. `"UpdatedAt"` is
    // to_snake_case-normalized by the seam, exactly like Go's camelCase entry.
    let mut renamed = folder_record("f-unstamped", 1);
    renamed.name = "name-selected".to_string();
    renamed.hash = "hash-must-not-move".to_string();
    put_committed(&db, &renamed, &other_stamp(), &["UpdatedAt"]);
    assert_eq!(
        cell_text(&db, "folder", "name", "f-unstamped"),
        "name-f-unstamped",
        "an unselected column must keep its stored value"
    );
    assert_eq!(
        cell_text(&db, "folder", "hash", "f-unstamped"),
        "hash-f-unstamped",
        "hash is not in the selection"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-unstamped"),
        time::to_text(&other_stamp()),
        "the selected column took the caller's stamp"
    );

    // Act 5: an empty id is refused BEFORE any statement runs — the port has no DB-side
    // random-id arm (Go's `id == ""` generates one, `sql_base_repository.go:635-638`).
    let empty_id = folder_record("", 1);
    let err = db
        .try_with_tx_sync::<FolderWriteResult, FolderWriteError>(|tx| {
            put_folder_in_tx(tx, &empty_id, &stamp, &[])
        })
        .expect_err("an empty id is a contract violation, not a generated id");
    assert!(
        matches!(err, DbRuntimeError::Domain(FolderWriteError::EmptyId)),
        "the refusal is its own typed domain arm, not a builder failure and not a SQLite error: {err:?}"
    );
    assert!(
        !matches!(err, DbRuntimeError::Domain(FolderWriteError::Builder(_))),
        "the empty-id refusal must not be reported as a statement-builder failure"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder"),
        2,
        "nothing was written, and no random id appeared"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder WHERE id = ''"),
        0
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// Chunk boundaries of the folder `MarkMissing` loop, both flag directions, the
/// unknown-id no-op, and a decoy row of ANOTHER library.
#[test]
fn missing_primitives_chunk_at_200_and_flip_both_directions() {
    let (db, path) = fresh_store("chunks");
    seed_library(&db, 1, "one");
    seed_library(&db, 2, "two");
    let targets = ids("f-chunk", 201);
    let mut rows: Vec<(&str, i64)> = targets.iter().map(|id| (id.as_str(), 1)).collect();
    rows.push(("f-decoy", 2));
    seed_folders(&db, &rows, &[]);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM folder"), 202);

    // Empty ids: the chunker yields no chunk, so NO statement runs (Go `:263`).
    assert_eq!(mark_missing_sql(true, &[], &scan_stamp()).len(), 0);
    assert_eq!(folders_missing(&db, true, &[], &scan_stamp()), 0);

    // 1 id → 1 chunk, 1 row, and the caller's stamp in its own column.
    assert_eq!(
        mark_missing_sql(true, &[targets[0].clone()], &scan_stamp()).len(),
        1
    );
    assert_eq!(
        folders_missing(&db, true, &[targets[0].clone()], &scan_stamp()),
        1
    );
    assert_eq!(flag(&db, "folder", "f-chunk-0"), 1);
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-0"),
        time::to_text(&scan_stamp()),
        "the caller's stamp reaches the column, Go's `Set(\"updated_at\", …)` (`:266`)"
    );

    // 200 ids → exactly ONE chunk (`slices.Chunk(ids, 200)`, `:263`), 200 rows.
    assert_eq!(
        mark_missing_sql(true, &targets[..200], &scan_stamp()).len(),
        1
    );
    assert_eq!(
        folders_missing(&db, true, &targets[..200], &scan_stamp()),
        200
    );

    // 201 ids → TWO chunks and 201 rows, i.e. the second chunk really runs.
    assert_eq!(mark_missing_sql(true, &targets, &scan_stamp()).len(), 2);
    assert_eq!(folders_missing(&db, true, &targets, &flip_stamp()), 201);
    assert_eq!(
        flag(&db, "folder", "f-chunk-200"),
        1,
        "the 201st row is marked"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-200"),
        time::to_text(&flip_stamp()),
        "the second chunk's row carries THIS call's stamp, proving both chunks applied"
    );

    // Unknown ids: Ok with 0 rows. Go checks `err` only and discards the count
    // (`folder_repository.go:268-271`), so a zero-row chunk is a successful no-op and
    // — unlike MediaFile.MarkMissing — the loop keeps going.
    let unknown = ids("f-unknown", 201);
    assert_eq!(mark_missing_sql(true, &unknown, &scan_stamp()).len(), 2);
    assert_eq!(folders_missing(&db, true, &unknown, &scan_stamp()), 0);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder WHERE missing = 1"),
        201,
        "the unknown ids touched nothing; the earlier 201 remain marked"
    );

    // Un-flip: `missing = false` clears the flag AND re-stamps (Go sets both columns).
    assert_eq!(folders_missing(&db, false, &targets, &unflip_stamp()), 201);
    assert_eq!(flag(&db, "folder", "f-chunk-0"), 0);
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-0"),
        time::to_text(&unflip_stamp())
    );

    // The other library's decoy kept its flag and its original stamp all along: it was
    // never in a supplied id list, NOT because some predicate excluded it.
    assert_eq!(
        flag(&db, "folder", "f-decoy"),
        0,
        "the decoy is still not missing"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-decoy"),
        time::to_text(&prior_stamp()),
        "the decoy's stamp never moved"
    );

    // Caller-trusted scope, asserted POSITIVELY: a supplied cross-library id IS updated.
    // No hidden library predicate exists to protect it (parent decision §A3).
    assert_eq!(
        folders_missing(&db, true, &["f-decoy".to_string()], &other_stamp()),
        1,
        "the DB applies the supplied ids; ownership is future SC typed authority"
    );
    assert_eq!(flag(&db, "folder", "f-decoy"), 1);

    // The count is SQLite's affected-row count, NOT the supplied-id count: a chunk with
    // 201 ids where only the library-2 decoy exists reports 1. Go's `executeSQL` gets
    // `c` from `res.RowsAffected()` (`sql_base_repository.go:343-353`) and never infers
    // it from `len(chunk)`.
    let mut mixed = ids("f-nonexistent", 200);
    mixed.push("f-decoy".to_string());
    assert_eq!(mark_missing_sql(true, &mixed, &scan_stamp()).len(), 2);
    fixture_insert(&db, "UPDATE folder SET missing = 0", &[]);
    assert_eq!(
        folders_missing(&db, true, &mixed, &scan_stamp()),
        1,
        "a mixed chunk counts the rows that actually changed"
    );

    // ---- Genuine zero-row FIRST chunk followed by a matching 201st id, both flags. ----
    // Go's folder `MarkMissing` checks `err` only (`folder_repository.go:268-271`), so a
    // chunk matching nothing is a successful no-op and the loop CONTINUES into chunk 2.
    // Direct-media marking does the opposite (`if rows == 0 { break; }`), which is pinned
    // separately in `media_mark_missing_zero_row_first_chunk_returns_ok_and_skips_later_chunks`;
    // asserting both shapes is what makes the asymmetry non-vacuous.
    fixture_insert(&db, "UPDATE folder SET missing = 0", &[]);
    let folder_zero_first = {
        let mut v = ids("f-absent", 200);
        v.push("f-chunk-1".to_string());
        v
    };
    assert_eq!(
        mark_missing_sql(true, &folder_zero_first, &scan_stamp()).len(),
        2,
        "201 folder ids render TWO statements"
    );
    assert_eq!(
        folders_missing(&db, true, &folder_zero_first, &scan_stamp()),
        1,
        "chunk 1 matched 0 rows and chunk 2 still ran: total 1, and Ok"
    );
    assert_eq!(
        flag(&db, "folder", "f-chunk-1"),
        1,
        "chunk 2's folder is flagged"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-1"),
        time::to_text(&scan_stamp()),
        "chunk 2's folder carries THIS call's caller stamp"
    );

    // Opposite flag through the same zero-first order: the folder variant has NO current-
    // flag gate (unlike MarkMissingByFolder), so the un-flip applies and re-stamps.
    assert_eq!(
        mark_missing_sql(false, &folder_zero_first, &other_stamp()).len(),
        2
    );
    assert_eq!(
        folders_missing(&db, false, &folder_zero_first, &other_stamp()),
        1,
        "the un-flip also continues past the zero-row chunk"
    );
    assert_eq!(flag(&db, "folder", "f-chunk-1"), 0);
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-1"),
        time::to_text(&other_stamp()),
        "chunk 2 re-stamped with the un-flip call's stamp"
    );

    // Reverse order (matching chunk first, zero-row chunk second): chunk 1's write stands,
    // the total is 1, and the result is Ok.
    let folder_matching_first = {
        let mut v = vec!["f-chunk-1".to_string()];
        v.extend(ids("f-absent", 200));
        v
    };
    assert_eq!(
        mark_missing_sql(true, &folder_matching_first, &flip_stamp()).len(),
        2
    );
    assert_eq!(
        folders_missing(&db, true, &folder_matching_first, &flip_stamp()),
        1
    );
    assert_eq!(flag(&db, "folder", "f-chunk-1"), 1);
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-chunk-1"),
        time::to_text(&flip_stamp())
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The Go quirk of `MediaFile.MarkMissing` (`mediafile_repository.go:462-466`):
/// `err != nil || c == 0` with `err == nil` returns NIL — SUCCESS — and exits the chunk
/// loop, so a zero-row first chunk skips every later chunk. The returned total is the
/// accumulated count of the chunks that ran, so a zero-row first chunk reports 0.
#[test]
fn media_mark_missing_zero_row_first_chunk_returns_ok_and_skips_later_chunks() {
    let (db, path) = fresh_store("media-zero-chunk");
    seed_library(&db, 1, "one");
    let real = ["mf-real-1", "mf-real-2", "mf-real-3"];
    seed_media(
        &db,
        &real
            .iter()
            .map(|id| (id.to_string(), 1_i64, "f-1".to_string()))
            .collect::<Vec<_>>(),
    );

    let unknown = ids("mf-unknown", 200);
    let real_ids: Vec<String> = real.iter().map(|id| id.to_string()).collect();

    // Order A: chunk 1 is 200 unknown ids (matches nothing), chunk 2 holds real ids.
    // Go returns nil from INSIDE the loop, so the real rows are never marked.
    let mut mixed = unknown.clone();
    mixed.extend(real_ids.clone());
    assert_eq!(media_mark_missing_sql(true, &mixed, &scan_stamp()).len(), 2);
    let outcome = media_missing(&db, true, &mixed, &scan_stamp());
    assert_eq!(
        outcome, 0,
        "the total is 0: the loop returned at the first chunk"
    );
    for id in &real {
        assert_eq!(flag(&db, "media_file", id), 0, "{id} must stay unmarked");
        assert_eq!(
            stamp_of(&db, "media_file", "updated_at", id),
            time::to_text(&prior_stamp()),
            "{id}'s stamp must not move: a naive implementation that kept looping would \
             have marked it"
        );
    }

    // Order B (reverse): chunk 1 matches (3 real ids), chunk 2 is all unknown. Chunk 1's
    // rows ARE marked and the result is still Ok.
    let mut reverse = real_ids.clone();
    reverse.extend(unknown.clone());
    let outcome = media_missing(&db, true, &reverse, &other_stamp());
    assert_eq!(outcome, 3, "the first chunk's rows are marked");
    for id in &real {
        assert_eq!(flag(&db, "media_file", id), 1, "{id} is marked by chunk 1");
        assert_eq!(
            stamp_of(&db, "media_file", "updated_at", id),
            time::to_text(&other_stamp())
        );
    }

    // Both orders returned Ok: no `NoRows` variant exists and none is invented. The type
    // proves the domain arm is uninhabited rather than merely unused.
    let witness: DbOperationError<Infallible> = DbOperationError::sqlite(
        SqliteError::InvalidParameterName("type witness".to_string()),
    );
    match witness {
        DbOperationError::Sqlite(_) => {}
        DbOperationError::Domain(unreachable) => match unreachable {},
    }
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// `MarkMissingByFolder`'s `Eq{"missing": !missing}` gate (`mediafile_repository.go:479`)
/// through the tx wrapper: already-flagged tracks are neither re-stamped nor counted, and
/// the opposite direction still applies.
#[test]
fn mark_media_missing_by_folder_gates_and_touch_album_ids_are_scoped() {
    let (db, path) = fresh_store("gate-and-scope");
    seed_library(&db, 1, "one");
    seed_library(&db, 2, "two");
    seed_folders(&db, &[("f-gate", 1), ("f-decoy", 2)], &[]);
    seed_media(
        &db,
        &[
            ("mf-a".to_string(), 1_i64, "f-gate".to_string()),
            ("mf-b".to_string(), 1_i64, "f-gate".to_string()),
            ("mf-decoy".to_string(), 2_i64, "f-decoy".to_string()),
        ],
    );
    seed_albums(
        &db,
        &[
            ("a-1".to_string(), 1_i64, Some(r#"["f-gate"]"#)),
            ("a-2".to_string(), 1_i64, Some(r#"["f-gate"]"#)),
            ("a-decoy".to_string(), 2_i64, Some(r#"["f-decoy"]"#)),
        ],
    );

    let folder_ids = vec!["f-gate".to_string()];

    // Mark: both tracks of the folder flip and take the caller's stamp.
    assert_eq!(
        media_missing_by_folder(&db, true, &folder_ids, &scan_stamp()),
        2
    );
    assert_eq!(flag(&db, "media_file", "mf-a"), 1);
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-a"),
        time::to_text(&scan_stamp())
    );

    // Re-apply the SAME flag: the gate (`AND missing = ?` bound to `!missing`) matches
    // nothing, so 0 rows and the existing stamp does NOT move.
    assert_eq!(
        media_missing_by_folder(&db, true, &folder_ids, &other_stamp()),
        0
    );
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-a"),
        time::to_text(&scan_stamp()),
        "the gate protects the existing stamp"
    );

    // Un-flip: the gate now matches the flagged rows, so both flip back and re-stamp.
    assert_eq!(
        media_missing_by_folder(&db, false, &folder_ids, &unflip_stamp()),
        2
    );
    assert_eq!(flag(&db, "media_file", "mf-a"), 0);
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-a"),
        time::to_text(&unflip_stamp())
    );

    // ---- Executed by-folder chunk boundary with a GENUINE zero-row first chunk. ----
    // 200 unknown folder ids (chunk 1 matches nothing) followed by a real 201st folder
    // (chunk 2 matches). Go's `MarkMissingByFolder` checks `err` ONLY (`:482-484`), so the
    // loop CONTINUES past the zero-row chunk and the later folder IS applied. Copying
    // `MarkMissing`'s `if rows == 0 { break; }` here would fail on the count, the flag, and
    // the stamp below — the asymmetry is discriminated, not assumed.
    // `mf-last` starts unflagged (fixture default), so `missing = true` passes the gate.
    seed_folders(&db, &[("f-last", 1)], &[]);
    seed_media(&db, &[("mf-last".to_string(), 1_i64, "f-last".to_string())]);

    let zero_first = {
        let mut v = ids("f-nonexistent", 200);
        v.push("f-last".to_string());
        v
    };
    assert_eq!(
        mark_missing_by_folder_sql(true, &zero_first, &scan_stamp()).len(),
        2,
        "201 folder ids render TWO statements, so chunk 2 really runs"
    );
    assert_eq!(
        media_missing_by_folder(&db, true, &zero_first, &scan_stamp()),
        1,
        "total = chunk 1's 0 rows + chunk 2's 1 row: the loop continued past the zero chunk"
    );
    assert_eq!(
        flag(&db, "media_file", "mf-last"),
        1,
        "chunk 2's track is flagged"
    );
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-last"),
        time::to_text(&scan_stamp()),
        "chunk 2's track carries THIS call's caller stamp"
    );

    // Same zero-first order, opposite flag: the gate (`missing = !missing`, `:479`) now
    // matches the flagged row, so chunk 2 un-flips it. Zero-first CONTINUE holds in both
    // directions.
    assert_eq!(
        mark_missing_by_folder_sql(false, &zero_first, &other_stamp()).len(),
        2
    );
    assert_eq!(
        media_missing_by_folder(&db, false, &zero_first, &other_stamp()),
        1,
        "the un-flip also survives the zero-row first chunk"
    );
    assert_eq!(flag(&db, "media_file", "mf-last"), 0);
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-last"),
        time::to_text(&other_stamp()),
        "chunk 2 re-stamped with the un-flip call's stamp"
    );

    // Reverse order (matching chunk first, zero-row chunk second): the earlier chunk's
    // write stands and the result is still Ok — no error and no rollback of chunk 1.
    let matching_first = {
        let mut v = vec!["f-last".to_string()];
        v.extend(ids("f-nonexistent", 200));
        v
    };
    assert_eq!(
        mark_missing_by_folder_sql(true, &matching_first, &flip_stamp()).len(),
        2
    );
    assert_eq!(
        media_missing_by_folder(&db, true, &matching_first, &flip_stamp()),
        1
    );
    assert_eq!(flag(&db, "media_file", "mf-last"), 1);
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-last"),
        time::to_text(&flip_stamp())
    );

    // Executed 0/1 boundary for this primitive: empty ids render NO statement and are
    // Ok(0); one already-flagged folder is a 0-row no-op through the gate.
    assert_eq!(
        mark_missing_by_folder_sql(true, &[], &scan_stamp()).len(),
        0
    );
    assert_eq!(media_missing_by_folder(&db, true, &[], &scan_stamp()), 0);
    assert_eq!(
        mark_missing_by_folder_sql(true, &["f-last".to_string()], &scan_stamp()).len(),
        1
    );
    assert_eq!(
        media_missing_by_folder(&db, true, &["f-last".to_string()], &scan_stamp()),
        0,
        "already flagged, so the gate matches nothing"
    );

    // A single unknown folder id is a successful NO-OP (one chunk, zero rows).
    assert_eq!(
        mark_missing_by_folder_sql(true, &["f-nonexistent-0".to_string()], &scan_stamp()).len(),
        1
    );
    assert_eq!(
        media_missing_by_folder(&db, true, &["f-nonexistent-0".to_string()], &scan_stamp()),
        0
    );

    // The other library's track was never under a supplied folder id.
    assert_eq!(flag(&db, "media_file", "mf-decoy"), 0);
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-decoy"),
        time::to_text(&prior_stamp())
    );

    // ---- Album.Touch: scoped to the SUPPLIED ids (`album_repository.go:360-373`). ----
    // Empty ids → no statement, Ok(0) (Go's `len(ids) == 0` guard, `:361-363`).
    assert_eq!(touch_sql(&[], &scan_stamp()).len(), 0);
    assert_eq!(touch_ids(&db, &[], &scan_stamp()), 0);

    // Two supplied ids → 1 chunk, 2 rows; the other library's decoy stays put.
    assert_eq!(
        touch_sql(&["a-1".to_string(), "a-2".to_string()], &scan_stamp()).len(),
        1
    );
    assert_eq!(
        touch_ids(&db, &["a-1".to_string(), "a-2".to_string()], &scan_stamp()),
        2,
        "Go logs this count at `:370`; the port returns it for the caller"
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "a-1"),
        time::to_text(&scan_stamp())
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "a-decoy"),
        time::to_text(&prior_stamp()),
        "the decoy is untouched because it was NOT supplied — no library predicate exists"
    );

    // Unknown ids → Ok with 0 rows, 2 chunks for 201 ids. No error, no NotFound.
    let unknown = ids("a-unknown", 201);
    assert_eq!(touch_sql(&unknown, &scan_stamp()).len(), 2);
    assert_eq!(touch_ids(&db, &unknown, &scan_stamp()), 0);

    // 201 REAL ids → 2 chunks and 201 rows, so the second chunk ran.
    let bulk = ids("a-bulk", 201);
    let rows: Vec<(String, i64, Option<&str>)> = bulk
        .iter()
        .map(|id| (id.clone(), 1_i64, Some("[]")))
        .collect();
    seed_albums(&db, &rows);
    assert_eq!(touch_sql(&bulk, &scan_stamp()).len(), 2);
    assert_eq!(touch_ids(&db, &bulk, &other_stamp()), 201);
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "a-bulk-200"),
        time::to_text(&other_stamp()),
        "the 201st album is touched by the second chunk"
    );

    // Caller-trusted scope, asserted POSITIVELY: a supplied cross-library album IS touched.
    assert_eq!(touch_ids(&db, &["a-decoy".to_string()], &scan_stamp()), 1);
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "a-decoy"),
        time::to_text(&scan_stamp())
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The GLOBAL touch (`album_repository.go:376-387`) keeps Go's exact predicate, which
/// includes albums of OTHER libraries whose `folder_ids` reference a missing folder.
/// NULL / `'[]'` / nested-array cells keep SQLite's actual behavior.
#[test]
fn touch_albums_with_missing_folders_globally_in_tx_reports_full_scope() {
    let (db, path) = fresh_store("touch-global");
    seed_library(&db, 1, "one");
    seed_library(&db, 2, "two");
    seed_folders(
        &db,
        &[
            ("f-missing-lib1", 1),
            ("f-missing-lib2", 2),
            ("f-present-lib1", 1),
        ],
        &["f-missing-lib1".to_string(), "f-missing-lib2".to_string()],
    );
    seed_albums(
        &db,
        &[
            ("al-lib1".to_string(), 1_i64, Some(r#"["f-missing-lib1"]"#)),
            ("al-lib2".to_string(), 2_i64, Some(r#"["f-missing-lib2"]"#)),
            // Cross-library: a library-2 album pointing at a library-1 missing folder.
            ("al-cross".to_string(), 2_i64, Some(r#"["f-missing-lib1"]"#)),
            // Cells that must NOT be touched.
            ("al-null-json".to_string(), 1_i64, Some("null")),
            ("al-empty".to_string(), 1_i64, Some("[]")),
            (
                "al-nested".to_string(),
                1_i64,
                Some(r#"[["f-missing-lib1"]]"#),
            ),
            (
                "al-nested-tail".to_string(),
                1_i64,
                Some(r#"["x",["f-missing-lib1"]]"#),
            ),
            (
                "al-present".to_string(),
                1_i64,
                Some(r#"["f-present-lib1"]"#),
            ),
        ],
    );

    let outcome = touch_globally(&db, &scan_stamp());
    assert_eq!(
        outcome, 3,
        "Go's GLOBAL predicate: lib1 + lib2 + the cross-library album, no library filter"
    );
    for id in ["al-lib1", "al-lib2", "al-cross"] {
        assert_eq!(
            stamp_of(&db, "album", "imported_at", id),
            time::to_text(&scan_stamp()),
            "{id} is touched"
        );
    }
    for id in [
        "al-null-json",
        "al-empty",
        "al-nested",
        "al-nested-tail",
        "al-present",
    ] {
        assert_eq!(
            stamp_of(&db, "album", "imported_at", id),
            time::to_text(&prior_stamp()),
            "{id} keeps its stamp (executed SQLite behavior, not an assumption)"
        );
    }
    // Executed facts behind those five: the TEXT 'null' cell PASSES `IS NOT NULL` and
    // json_each yields one NULL value, which joins nothing; `'[]'` yields no rows; a
    // nested array yields the SERIALIZED inner array as TEXT, which never equals a folder
    // id. A real SQL NULL cannot even be stored here — `album.folder_ids` is
    // `jsonb default '[]' not null` (`nd-dbschema/src/schema_ddl.rs:58`, same as Go's
    // DDL), which is why Go's `NotEq{"folder_ids": nil}` arm never excludes a row in
    // practice. Asserted, not assumed.
    let null_rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        seed_albums(&db, &[("al-sql-null".to_string(), 1_i64, None)])
    }));
    assert!(
        null_rejected.is_err(),
        "a SQL NULL folder_ids is a NOT NULL violation in this schema"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM album WHERE folder_ids IS NULL"),
        0,
        "no NULL cell exists, so `IS NOT NULL` excludes nothing"
    );
    assert_eq!(
        cell_text(&db, "album", "folder_ids", "al-null-json"),
        "null"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT json_valid(folder_ids) FROM album WHERE id='al-null-json'"
        ),
        1
    );

    // It is ONE statement with no id list, like Go (`:377-381`), and it re-stamps rows
    // that are already touched.
    let outcome = touch_globally(&db, &other_stamp());
    assert_eq!(
        outcome, 3,
        "the global touch is unconditional on prior state"
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "al-lib1"),
        time::to_text(&other_stamp())
    );

    // Zero matches → Ok(0), not an error.
    fixture_insert(&db, "UPDATE folder SET missing = 0", &[]);
    assert_eq!(touch_globally(&db, &flip_stamp()), 0);

    // The reverse direction is NOT the global touch's job: clearing album flags is
    // nobody's primitive here, so assert the touched rows kept the newer stamp.
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "al-cross"),
        time::to_text(&other_stamp())
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// A malformed `folder_ids` cell makes Go's predicate a HARD SQLite failure
/// (`malformed JSON`, which carries NO extended constraint code), and inside the typed
/// transaction the earlier legs of the same body roll back with it.
#[test]
fn malformed_folder_ids_json_is_a_hard_error_and_rolls_back_earlier_legs() {
    let (db, path) = fresh_store("malformed-json");
    seed_library(&db, 1, "one");
    seed_folders(
        &db,
        &[("f-missing", 1), ("f-clean", 1)],
        &["f-missing".to_string()],
    );
    seed_media(&db, &[("mf-1".to_string(), 1_i64, "f-clean".to_string())]);
    seed_albums(
        &db,
        &[
            ("al-valid".to_string(), 1_i64, Some(r#"["f-missing"]"#)),
            ("al-bad".to_string(), 1_i64, Some("{not json")),
        ],
    );

    // Every primitive in this body has an `Infallible` domain type, so the executor's
    // E is Infallible: any failure surfaced here is a SQLite failure.
    let failed = db.try_with_tx_sync::<usize, Infallible>(|tx| {
        // Earlier legs succeed inside the body: a folder flag, a media flag, and a
        // scoped touch of a valid album.
        mark_folders_missing_in_tx(tx, true, &["f-clean".to_string()], &scan_stamp())?;
        mark_media_missing_in_tx(tx, true, &["mf-1".to_string()], &scan_stamp())?;
        touch_albums_in_tx(tx, &["al-valid".to_string()], &scan_stamp())?;
        // The global touch then hits the malformed cell.
        touch_albums_with_missing_folders_globally_in_tx(tx, &scan_stamp())
    });
    let err = failed.expect_err("the malformed cell fails the whole statement");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert!(
                source.to_string().contains("malformed JSON"),
                "the concrete SQLite message is retained: {source}"
            );
            // §A4 executed: this failure carries NO extended constraint code, so the
            // assertion is on the concrete error class, never on SQLITE_CONSTRAINT_*.
            match source {
                SqliteError::SqliteFailure(code, _) => {
                    assert_eq!(
                        code.code,
                        ffi::ErrorCode::Unknown,
                        "SQLite's plain SQLITE_ERROR class (rustc-ffi maps it to Unknown)"
                    );
                    assert_eq!(
                        code.extended_code,
                        ffi::SQLITE_ERROR,
                        "no extended code at all, so a SQLITE_CONSTRAINT_* match is impossible"
                    );
                }
                other => panic!("expected a SqliteFailure, got {other:?}"),
            }
            assert!(!is_busy(source), "a statement error is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    // Rollback, read on a fresh lease: nothing from the body survived.
    assert_eq!(
        flag(&db, "folder", "f-clean"),
        0,
        "the folder flag written before the failure rolled back"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-clean"),
        time::to_text(&prior_stamp())
    );
    assert_eq!(
        flag(&db, "media_file", "mf-1"),
        0,
        "the media flag rolled back"
    );
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-1"),
        time::to_text(&prior_stamp())
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "al-valid"),
        time::to_text(&prior_stamp()),
        "the scoped touch rolled back too"
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "al-bad"),
        time::to_text(&prior_stamp()),
        "the malformed row was never touched (the statement touched 0 rows)"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The folder base read keeps Go's zero `UpdateAt` (dbx maps by field name and no SELECT
/// emits `update_at`), while `get_folder_update_info` reads the stored column — the
/// asymmetry §A5 freezes. Load-bearing: "fixing" the base read breaks this test.
#[test]
fn folder_updateinfo_reads_stamp_while_base_read_keeps_go_zero() {
    let (db, path) = fresh_store("update-info");
    seed_library(&db, 1, "one");
    let stamp = scan_stamp();
    put_committed(&db, &folder_record("f-info", 1), &stamp, &[]);

    let base = get_folder(&db, "f-info").expect("the row is readable");
    assert_eq!(
        base.updated_at,
        nd_db::hydrate::go_zero_time(),
        "the base read returns Go's zero time.Time, NOT the stored column"
    );
    assert!(
        nd_db::hydrate::is_go_zero(&base.updated_at),
        "and that is what Go's IsZero() sees"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-info"),
        time::to_text(&stamp),
        "the COLUMN holds the caller's stamp — the read, not the storage, is zero"
    );

    let info = update_info(&db, 1);
    let info = info
        .get("f-info")
        .expect("the map is keyed by folder id, not by path (`:199`)");
    assert_eq!(
        info.updated_at, stamp,
        "get_folder_update_info reads the stored updated_at"
    );
    assert_eq!(info.hash, "hash-f-info");
    assert_eq!(info.image_files, Some(vec!["cover-f-info.jpg".to_string()]));
    assert_eq!(info.images_updated_at, at(2020, 1, 2, 3, 4, 5, 111_000_000));
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// A `RAISE(ABORT)` trigger keyed to a poisoned folder id in the SECOND chunk of a 201-id
/// list aborts mid-loop, so the earlier chunk and every prior primitive of the same
/// transaction roll back — verified on a fresh lease.
#[test]
fn missing_chunk_failure_rolls_back_all_primitives() {
    let (db, path) = fresh_store("chunk-rollback");
    seed_library(&db, 1, "one");
    seed_library(&db, 2, "two");
    let ok_ids: Vec<String> = ids("f-ok", 200);
    let mut rows: Vec<(&str, i64)> = ok_ids.iter().map(|id| (id.as_str(), 1_i64)).collect();
    rows.push(("f-poison", 1));
    rows.push(("f-other", 2));
    seed_folders(&db, &rows, &[]);
    seed_media(&db, &[("mf-1".to_string(), 1_i64, "f-ok-0".to_string())]);
    seed_albums(&db, &[("al-1".to_string(), 1_i64, Some("[]"))]);

    // SQLite forbids bound variables in a trigger body, so the id is spelled in the
    // fixture DDL. Test-local DDL only: crates/nd-dbschema is untouched.
    fixture_insert(
        &db,
        "CREATE TRIGGER fixture_folder_chunk_fails BEFORE UPDATE ON folder \
         FOR EACH ROW WHEN NEW.id = 'f-poison' \
         BEGIN SELECT RAISE(ABORT, 'fixture: poisoned folder id'); END",
        &[],
    );

    // The poisoned id is the 201st id, i.e. in chunk 2, so chunk 1 already ran.
    let mut targets = ids("f-ok", 200);
    targets.push("f-poison".to_string());
    assert_eq!(mark_missing_sql(true, &targets, &scan_stamp()).len(), 2);

    // All three primitives carry an `Infallible` domain type, so E is Infallible and the
    // trigger failure arrives as a Body-stage SQLite failure.
    let failed = db.try_with_tx_sync::<usize, Infallible>(|tx| {
        touch_albums_in_tx(tx, &["al-1".to_string()], &scan_stamp())?;
        mark_media_missing_in_tx(tx, true, &["mf-1".to_string()], &scan_stamp())?;
        mark_folders_missing_in_tx(tx, true, &targets, &scan_stamp())
    });
    let err = failed.expect_err("the trigger aborts chunk 2");
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
                "the raw SQLite cause survives the seam"
            );
            assert!(!is_busy(source), "a trigger abort is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    assert_eq!(
        flag(&db, "folder", "f-poison"),
        0,
        "the poisoned row is not marked"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder WHERE missing = 1"),
        0,
        "chunk 1's 200 rows rolled back with chunk 2's failure"
    );
    assert_eq!(
        flag(&db, "media_file", "mf-1"),
        0,
        "the media flag rolled back"
    );
    assert_eq!(
        stamp_of(&db, "media_file", "updated_at", "mf-1"),
        time::to_text(&prior_stamp()),
        "the media import timestamp never moved"
    );
    assert_eq!(
        stamp_of(&db, "album", "imported_at", "al-1"),
        time::to_text(&prior_stamp()),
        "the earlier touch rolled back too"
    );
    for id in ids("f-ok", 200) {
        assert_eq!(
            stamp_of(&db, "folder", "updated_at", &id),
            time::to_text(&prior_stamp()),
            "{id} keeps its original stamp after the rollback"
        );
    }
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-other"),
        time::to_text(&prior_stamp()),
        "the other library's folder is unchanged"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });
    assert_eq!(db.idle_count().expect("pool state"), 2);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The `EmptyUpdateSet` builder arm is a typed DOMAIN failure that runs no statement,
/// while SQLite failures stay on the `Sqlite` arm with their extended code intact.
#[test]
fn empty_update_set_builder_arm_is_typed_and_sql_failures_stay_concrete() {
    let (db, path) = fresh_store("builder-arm");
    seed_library(&db, 1, "one");

    // A selection that resolves to nothing settable after the created_at/birth_time
    // drop: Go renders a no-op `SET id = ?` and errors nowhere; the port errors with the
    // concrete MarshalError and never emits degenerate SQL.
    let err = db
        .try_with_tx_sync::<FolderWriteResult, FolderWriteError>(|tx| {
            put_folder_in_tx(
                tx,
                &folder_record("f-none", 1),
                &scan_stamp(),
                &["created_at"],
            )
        })
        .expect_err("nothing is settable after the drop");
    match err {
        DbRuntimeError::Domain(FolderWriteError::Builder(inner)) => {
            assert_eq!(
                inner,
                marshal::MarshalError::EmptyUpdateSet,
                "the domain arm carries the concrete MarshalError, not a string"
            );
        }
        other => panic!("expected a builder-domain failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder"),
        0,
        "the refused Put wrote nothing: no degenerate `SET  WHERE` statement ran"
    );

    // The SQLite arm stays concrete: an FK violation on library_id arrives as a raw
    // rusqlite::Error with its extended code, exactly like Go's unwrapped error.
    let failed = db.try_with_tx_sync::<FolderWriteResult, FolderWriteError>(|tx| {
        put_folder_in_tx(tx, &folder_record("f-fk", 999), &scan_stamp(), &[])
    });
    let err = failed.expect_err("library 999 does not exist");
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(stage, TransactionStage::Body);
            assert_eq!(
                extended_code(&source),
                Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                "Go returns this error raw and unwrapped (`folder_repository.go:257-258`)"
            );
        }
        DbRuntimeError::Domain(inner) => panic!("an FK failure is not a domain failure: {inner}"),
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM folder"),
        0,
        "the FK failure rolled back the whole body"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// Bind-order guard for the chunked primitives, asserted by ROW state rather than SQL
/// text: with all SET values bound BEFORE the ids (DB-01's rule) a chunk marks exactly
/// the intended rows. The ids-first pre-fix order bound an id into `missing` and the
/// stamp into `WHERE id IN (…)`, which cannot produce these row states.
#[test]
fn chunk_bind_order_keeps_set_values_before_ids() {
    let (db, path) = fresh_store("bind-order");
    seed_library(&db, 1, "one");
    seed_folders(&db, &[("f-1", 1), ("f-2", 1), ("5", 1)], &[]);
    seed_media(
        &db,
        &[
            ("mf-1".to_string(), 1_i64, "f-1".to_string()),
            ("5".to_string(), 1_i64, "f-1".to_string()),
        ],
    );

    // The numeric-looking decoy id `5` is the row a mis-bound stamp used to hit.
    let stamp = scan_stamp();
    assert_eq!(
        folders_missing(&db, true, &["f-1".to_string(), "f-2".to_string()], &stamp),
        2
    );
    assert_eq!(
        flag(&db, "folder", "5"),
        0,
        "the numeric decoy folder is NOT marked"
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "5"),
        time::to_text(&prior_stamp()),
        "a mis-bound stamp would have written a numeric updated_at here"
    );
    // Positive control for that claim: binding a raw epoch integer into this `datetime`
    // column really does produce an INTEGER cell (NUMERIC affinity, DB-02's measured
    // result), so the TEXT check above is a real guard and not a tautology.
    fixture_insert(
        &db,
        "UPDATE folder SET updated_at = 1725000000 WHERE id = '5'",
        &[],
    );
    assert_eq!(
        scalar::<String>(&db, "SELECT typeof(updated_at) FROM folder WHERE id='5'"),
        "integer",
        "an integer bind lands as INTEGER, the shape a mis-bound stamp would leave"
    );
    fixture_insert(
        &db,
        "UPDATE folder SET updated_at = ? WHERE id = '5'",
        &[text(&time::to_text(&prior_stamp()))],
    );
    assert_eq!(
        stamp_of(&db, "folder", "updated_at", "f-1"),
        time::to_text(&stamp),
        "the stamp is in its own column, canonical TEXT"
    );
    assert_eq!(
        scalar::<String>(&db, "SELECT typeof(updated_at) FROM folder WHERE id='f-1'"),
        "text",
        "missing and updated_at sit in the right slots"
    );

    assert_eq!(media_missing(&db, true, &["mf-1".to_string()], &stamp), 1);
    assert_eq!(
        flag(&db, "media_file", "5"),
        0,
        "the numeric decoy track is NOT marked"
    );
    assert_eq!(flag(&db, "media_file", "mf-1"), 1);
    assert_eq!(
        scalar::<String>(
            &db,
            "SELECT typeof(updated_at) FROM media_file WHERE id='mf-1'"
        ),
        "text"
    );

    // The by-folder primitive binds its gate AFTER the ids (`AND missing = ?` last).
    assert_eq!(
        media_missing_by_folder(&db, true, &["f-1".to_string()], &stamp),
        1,
        "only the track under f-1 flipped"
    );
    assert_eq!(flag(&db, "media_file", "mf-1"), 1);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
