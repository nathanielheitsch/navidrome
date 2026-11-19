//! DB-05 — the EXECUTED library writes (`persistence/library_repository.go:72-134`,
//! `152-188`) through the DB-00 typed transaction seam.
//!
//! Go truth under test: `libraryRepository.Put` runs the ID-1 path guard (`:73-81`),
//! stamps `UpdatedAt` (`:84`), then either inserts with an autoassigned id (`:85-88`)
//! or updates the `selectUpdateColumns` set and inserts on a zero-row update
//! (`:89-111`), and finally assigns every admin to every library in the same
//! statement (`:117-128`). `ScanBegin`/`ScanEnd`/`SetScannedPID` (`:162-188`) check no
//! affected-row count, and `AddArtist` (`:152-160`) is an idempotent FK-enforced link.
//!
//! Go facts here are SOURCE-VERIFIED at the pinned lines (plus
//! `dbx@v1.12.0/model_query.go:62-114` and `builder.go:166-198` for the insert leg);
//! every behavioral claim below is then EXECUTED against a real SQLite DB built from
//! the fresh DDL (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp file
//! per invocation, `_foreign_keys=on` — the same DSN parameter Go's default path
//! carries, `consts/consts.go:14`), written
//! through the production `nd_db::library_repo` write API and read back through the
//! production `LibraryRepo`. No durable Go oracle directory exists for this package
//! (`docs/rust-port/subplans/db-runtime.md` DB-05 point 8), so expectations are pinned
//! against Go source plus the parent's `/tmp/sd3g1_oracle*` findings; nothing here is
//! labeled a Go golden, and executed-Rust claims are labeled EXECUTED.
//!
//! Column ORDER is deliberately NOT asserted: dbx sorts its column map alphabetically
//! and backtick-quotes it, so the Rust renderer's declaration order and Go's executed
//! text differ cosmetically while both bind positionally against their own list
//! (`decisions.md` W1). Every insert test therefore distinguishes all fields and
//! asserts each stored value in its OWN column, which is what makes a positional
//! rotation fail rather than pass vacuously.

use std::convert::Infallible;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use nd_db::library_repo::{
    add_artist_in_tx, put_in_tx, scan_begin_in_tx, scan_end_in_tx, set_scanned_pid_in_tx,
    LibraryColumn, LibraryError, LibraryRecord, LibraryRepo, LibraryWriteError, LibraryWriteResult,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::{is_busy, SQLStore};
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Deterministic caller clock: `now` is supplied by the caller and sampled outside
/// any retry loop, so stamps are exact rather than wall-clock dependent.
fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32, nanos: u32) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, min, s, nanos)
            .unwrap(),
    )
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation, freshly migrated, pooled (the
/// `runtime_hydrate_library_property_tag.rs` fixture shape).
fn fresh_store(label: &str) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-library-writes-{label}-"),
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

/// Run one closure on a leased connection; store failures panic, the repository's own
/// error value comes back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (users, artists, poisoned cells). Library rows themselves
/// are ALWAYS created through `put_in_tx`, never by hand-built INSERTs.
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

/// `user` row (`schema_ddl.rs:57`): `created_at`/`updated_at` are `not null`, so both
/// are seeded explicitly.
fn seed_user(store: &SQLStore, id: &str, name: &str, is_admin: bool) {
    fixture_insert(
        store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(id),
            text(id),
            text(name),
            text(&format!("{id}@x.y")),
            text("CIPHER"),
            integer(i64::from(is_admin)),
            text(&time::to_text(&at(2024, 1, 1, 0, 0, 0, 0))),
            text(&time::to_text(&at(2024, 1, 2, 0, 0, 0, 0))),
        ],
    );
}

/// An `artist` row for the `library_artist` FK (production artist writes are DB-08).
fn seed_artist(store: &SQLStore, id: &str) {
    fixture_insert(
        store,
        "INSERT INTO artist (id, name) VALUES (?, ?)",
        &[text(id), text(id)],
    );
}

/// A record whose every field carries a DIFFERENT value, so a rotated positional
/// argument list cannot land on a value that happens to match another column.
fn record(id: i64, tag: &str) -> LibraryRecord {
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

/// Commit one `Put` through the typed seam exactly as a caller would.
fn put_committed(
    store: &SQLStore,
    record_in: &LibraryRecord,
    now: &DateTime<Utc>,
    cols: &[LibraryColumn],
) -> LibraryWriteResult {
    store
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, record_in, now, cols)
        })
        .unwrap_or_else(|e| panic!("put_in_tx({:?}) failed: {e}", record_in.name))
}

/// Read one library back through the production repository on a leased connection.
fn get(store: &SQLStore, id: i64) -> Result<LibraryRecord, LibraryError> {
    lease(store, |conn| LibraryRepo::new(conn).get(id))
}

/// `user_library` membership, ordered — the raw view of the assignment leg.
fn links(store: &SQLStore) -> Vec<(String, i64)> {
    lease(store, |conn| {
        let mut stmt = conn
            .prepare("SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
}

/// `scan_in_progress` through the production read side.
fn scan_in_progress(store: &SQLStore) -> bool {
    lease(store, |conn| {
        LibraryRepo::new(conn)
            .scan_in_progress()
            .unwrap_or_else(|e| panic!("scan_in_progress: {e}"))
    })
}

/// Lift a link/scan seam's `Infallible` domain type into the caller's own `E`, which
/// also proves that seam has no domain arm to translate.
fn escalate(err: DbOperationError<Infallible>) -> DbOperationError<LibraryWriteError> {
    match err {
        DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
        DbOperationError::Domain(unreachable) => match unreachable {},
    }
}

/// The extended SQLite code, when the failure carries one — the assertion that the
/// seam kept the concrete cause rather than a stringified wrapper.
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// The insert leg, executed: autoincrement (`id` omitted) and the explicit-id
/// fallback, read back column by column.
#[test]
fn library_auto_and_explicit_id_insert_match_all_columns() {
    let (db, path) = fresh_store("insert");

    // Act: Go's `l.ID == 0` arm (:85-88).
    let first = record(0, "one");
    let now = at(2024, 7, 1, 12, 0, 0, 1_000_000);
    let result = put_committed(&db, &first, &now, &[]);
    assert_eq!(
        result,
        LibraryWriteResult {
            id: 1,
            inserted: true
        },
        "the generated id is this connection's last_insert_rowid, not a re-query"
    );

    // Assert: EXECUTED rotation detector — every column read in its own column.
    let row = get(&db, 1).expect("row 1 exists");
    assert_eq!(row.id, 1);
    assert_eq!(row.name, "name-one");
    assert_eq!(row.path, "/path-one");
    assert_eq!(row.remote_path, "remote-one");
    assert_eq!(
        row.last_scan_at, first.last_scan_at,
        "the record's scan stamps"
    );
    assert_eq!(row.last_scan_started_at, first.last_scan_started_at);
    assert!(row.full_scan_in_progress);
    assert_eq!(row.total_songs, 11);
    assert_eq!(row.total_albums, 12);
    assert_eq!(row.total_artists, 13);
    assert_eq!(row.total_folders, 14);
    assert_eq!(row.total_files, 15);
    assert_eq!(row.total_missing_files, 16);
    assert_eq!(row.total_size, 17);
    assert_eq!(row.total_duration, 18.5);
    assert!(row.default_new_users);
    assert_eq!(row.pid_album, "pidalbum-one");
    assert_eq!(row.pid_track, "pidtrack-one");
    assert_eq!(row.scanned_pid_album, "scannedalbum-one");
    assert_eq!(row.scanned_pid_track, "scannedtrack-one");

    // Both stamps come from the caller's clock, not the record's distinct values.
    assert_eq!(row.updated_at, now);
    assert_eq!(row.created_at, now);
    assert_ne!(row.created_at, first.created_at);
    assert_ne!(row.updated_at, first.updated_at);

    // Storage classes are the crate-wide TEXT convention, EXECUTED.
    assert_eq!(
        scalar::<String>(&db, "SELECT typeof(last_scan_at) FROM library WHERE id = 1"),
        "text"
    );
    assert_eq!(
        scalar::<String>(
            &db,
            "SELECT typeof(last_scan_started_at) FROM library WHERE id = 1"
        ),
        "text"
    );
    assert_eq!(
        scalar::<String>(
            &db,
            "SELECT typeof(total_duration) FROM library WHERE id = 1"
        ),
        "real"
    );
    assert_eq!(
        scalar::<f64>(&db, "SELECT total_duration FROM library WHERE id = 1"),
        18.5
    );
    // `remote_path` binds the record's String, never NULL (a NULL breaks the read).
    assert_eq!(
        scalar::<String>(&db, "SELECT typeof(remote_path) FROM library WHERE id = 1"),
        "text"
    );

    // The input record is `&`-borrowed: Go mutates `l.UpdatedAt`/`l.CreatedAt` and
    // fills `l.ID` in place, the port passes stamps as bound values and returns the id
    // instead, so the caller's record is untouched.
    assert_eq!(
        first,
        record(0, "one"),
        "put_in_tx does not write back into the caller's record"
    );

    // Act: the second autoincrement insert proves the generated id advances.
    let second = record(0, "two");
    assert_eq!(
        put_committed(&db, &second, &now, &[]),
        LibraryWriteResult {
            id: 2,
            inserted: true
        }
    );
    assert_eq!(get(&db, 2).expect("row 2").path, "/path-two");

    // Act: Go's non-zero-id arm on a MISSING row (:106-111) — the ID-999 insert
    // pinned by persistence/library_repository_test.go:115-137.
    let explicit = record(999, "explicit");
    assert_eq!(
        put_committed(&db, &explicit, &now, &[]),
        LibraryWriteResult {
            id: 999,
            inserted: true
        },
        "a zero-row update is an insert with the explicit id, not a miss"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT id FROM library WHERE id = 999"),
        999
    );
    assert_eq!(get(&db, 999).expect("row 999").name, "name-explicit");

    // Act: the same id now EXISTS, so the UPDATE leg runs and `inserted` flips.
    assert_eq!(
        put_committed(&db, &explicit, &now, &[]),
        LibraryWriteResult {
            id: 999,
            inserted: false
        }
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library"),
        3,
        "the update leg inserted no second row"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The UPDATE leg: `created_at` is structurally immutable, `updated_at` is always
/// stamped, a typed partial column set touches only its columns, and the counters are
/// not writable through `Put`.
#[test]
fn library_partial_update_preserves_created_and_protected_path() {
    let (db, path) = fresh_store("partial");
    let insert_now = at(2024, 7, 1, 12, 0, 0, 0);
    let created = record(42, "keep");
    put_committed(&db, &created, &insert_now, &[]);
    let before = get(&db, 42).expect("row 42");

    // Act: one typed column only, with every OTHER field changed in the record.
    let mut changed = created.clone();
    changed.name = "renamed".to_string();
    changed.path = "/changed-but-not-selected".to_string();
    changed.remote_path = "remote-changed".to_string();
    changed.default_new_users = false;
    changed.pid_album = "pidalbum-changed".to_string();
    changed.pid_track = "pidtrack-changed".to_string();
    changed.total_songs = 999;
    changed.created_at = at(2030, 1, 1, 0, 0, 0, 0);
    let update_now = at(2024, 8, 2, 13, 0, 0, 5_000_000);
    assert_eq!(
        put_committed(&db, &changed, &update_now, &[LibraryColumn::Name]),
        LibraryWriteResult {
            id: 42,
            inserted: false
        }
    );

    let after = get(&db, 42).expect("row 42 after partial update");
    assert_eq!(after.name, "renamed", "the selected column moved");
    assert_eq!(
        after.path, before.path,
        "an unselected column keeps its stored value even though the record differs"
    );
    assert_eq!(after.remote_path, before.remote_path);
    assert_eq!(after.default_new_users, before.default_new_users);
    assert_eq!(after.pid_album, before.pid_album);
    assert_eq!(after.pid_track, before.pid_track);
    assert_eq!(
        after.created_at, before.created_at,
        "created_at is immutable"
    );
    assert_eq!(after.updated_at, update_now, "updated_at is always stamped");
    assert_eq!(
        after.total_songs, before.total_songs,
        "the counters stay owned by DB-17 stats"
    );
    assert_eq!(after.total_duration, before.total_duration);

    // Act: an empty column list is Go's empty variadic list — all six columns set.
    put_committed(&db, &changed, &update_now, &[]);
    let all = get(&db, 42).expect("row 42 after full update");
    assert_eq!(all.path, "/changed-but-not-selected");
    assert_eq!(all.remote_path, "remote-changed");
    assert!(!all.default_new_users);
    assert_eq!(all.pid_album, "pidalbum-changed");
    assert_eq!(all.pid_track, "pidtrack-changed");
    assert_eq!(all.created_at, before.created_at, "still immutable");
    assert_eq!(all.updated_at, update_now);
    assert_eq!(all.total_songs, before.total_songs);

    // EXECUTED: only the selected columns' cells changed in the first update.
    let stamps: (String, String) = lease(&db, |conn| {
        conn.query_row(
            "SELECT created_at, updated_at FROM library WHERE id = 42",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    });
    assert_eq!(stamps.0, time::to_text(&insert_now));
    assert_eq!(stamps.1, time::to_text(&update_now));
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The ID-1 path guard (`:73-81`): an existing library 1 rejects a different path even
/// when `Path` is not a selected column, and creating library 1 stays legal.
#[test]
fn library_id1_path_guard_rejects_filtered_change_and_allows_create() {
    let (db, path) = fresh_store("guard");
    let now = at(2024, 7, 1, 12, 0, 0, 0);

    // Creating library 1 is allowed (Go's comment: "if we are creating it, it's ok").
    let first = record(1, "libone");
    assert_eq!(
        put_committed(&db, &first, &now, &[]),
        LibraryWriteResult {
            id: 1,
            inserted: true
        }
    );
    assert_eq!(get(&db, 1).expect("library 1").path, "/path-libone");

    // A Put that keeps the path is accepted, including a filtered one.
    let same = record(1, "libone");
    assert_eq!(
        put_committed(&db, &same, &now, &[LibraryColumn::Name]),
        LibraryWriteResult {
            id: 1,
            inserted: false
        }
    );

    // A different path is refused BEFORE any statement, even with Path unselected:
    // Go's guard precedes selectUpdateColumns.
    let mut moved = record(1, "libone");
    moved.name = "renamed-only".to_string();
    moved.path = "/moved".to_string();
    let failed = db.try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
        put_in_tx(tx, &moved, &now, &[LibraryColumn::Name])
    });
    match failed {
        Err(DbRuntimeError::Domain(LibraryWriteError::PathImmutable {
            id,
            stored,
            requested,
        })) => {
            assert_eq!(id, 1);
            assert_eq!(stored, "/path-libone");
            assert_eq!(requested, "/moved");
            assert_eq!(
                LibraryWriteError::PathImmutable {
                    id,
                    stored,
                    requested,
                }
                .to_string(),
                "validation error: path for library with ID 1 cannot be changed",
                "the wrapped model.ErrValidation text (:78, model/errors.go:12)"
            );
        }
        other => panic!("expected the PathImmutable domain arm, got {other:?}"),
    }

    // EXECUTED: zero statements ran — the name did NOT move and updated_at did not.
    let row = get(&db, 1).expect("library 1 unchanged");
    assert_eq!(row.name, "name-libone");
    assert_eq!(row.path, "/path-libone");
    assert_eq!(row.updated_at, now);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// Admin assignment (`library_repository.go:123-134`): every admin gains EVERY library
/// — the CROSS JOIN is not limited to the written row — re-`Put` is idempotent, and
/// non-admins stay out. The discriminating case is an admin created AFTER both
/// libraries exist: a Put on ONE library must still link it to the other library.
#[test]
fn library_admin_assignment_is_atomic_idempotent() {
    let (db, path) = fresh_store("admins");
    seed_user(&db, "adm1", "Admin One", true);
    seed_user(&db, "adm2", "Admin Two", true);
    seed_user(&db, "pleb", "Plain", false);
    let now = at(2024, 7, 1, 12, 0, 0, 0);

    put_committed(&db, &record(1, "alpha"), &now, &[]);
    put_committed(&db, &record(0, "beta"), &now, &[]);

    assert_eq!(
        links(&db),
        vec![
            ("adm1".to_string(), 1),
            ("adm1".to_string(), 2),
            ("adm2".to_string(), 1),
            ("adm2".to_string(), 2),
        ],
        "both admins reach BOTH libraries from the CROSS JOIN, and the non-admin is absent"
    );
    assert_eq!(
        lease(&db, |conn| LibraryRepo::new(conn)
            .get_users_with_library_access(2)
            .unwrap()
            .len()),
        2,
        "the production read side sees the same membership"
    );

    // Re-Put library 1: ON CONFLICT DO NOTHING keeps the count, nothing duplicates.
    put_committed(&db, &record(1, "alpha"), &now, &[LibraryColumn::Name]);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM user_library"),
        4,
        "idempotent re-assignment"
    );

    // An admin that exists only AFTER both libraries were written, then a Put that
    // touches ONE library. Assigning only the written row would leave (adm3, 1) absent
    // and give 5 links; the unscoped CROSS JOIN gives 6.
    seed_user(&db, "adm3", "Admin Three", true);
    put_committed(&db, &record(2, "beta"), &now, &[LibraryColumn::Name]);
    assert_eq!(
        links(&db),
        vec![
            ("adm1".to_string(), 1),
            ("adm1".to_string(), 2),
            ("adm2".to_string(), 1),
            ("adm2".to_string(), 2),
            ("adm3".to_string(), 1),
            ("adm3".to_string(), 2),
        ],
        "the late admin is linked to the library this Put did NOT touch"
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM user_library WHERE user_id = 'pleb'"
        ),
        0,
        "non-admins are never assigned"
    );
    assert_eq!(
        lease(&db, |conn| LibraryRepo::new(conn)
            .get_users_with_library_access(1)
            .unwrap()
            .len()),
        3,
        "the untouched library gained the late admin on the read side too"
    );

    // A third library is linked to all three admins by ITS OWN Put (all libs, all admins).
    put_committed(&db, &record(0, "gamma"), &now, &[]);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM user_library"), 9);

    // An empty path is a valid DB value here (the DDL allows ''); policy is BOOT's.
    let mut empty = record(0, "empty");
    empty.path = String::new();
    assert!(
        put_committed(&db, &empty, &now, &[]).inserted,
        "DB-05 rejects nothing but the ID-1 rule"
    );
    assert_eq!(get(&db, 4).expect("row 4").path, "");
    let empty_path = lease(&db, |conn| LibraryRepo::new(conn).get_path(4));
    assert_eq!(
        empty_path.expect("GetPath looks up presence, not non-emptiness"),
        "",
        "an empty path is a valid DB value; policy rejection belongs to BOOT"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The scan lifecycle, asserted through the production `scan_in_progress` predicate as
/// a clean idle → begin → end → idle → second-begin sequence on a store whose only row
/// came from `put_in_tx`. The fresh DDL sentinel's quirk is asserted separately, on its
/// own row, before that row is removed so it cannot pollute the aggregate counts.
#[test]
fn scan_lifecycle_methods_agree_with_idle_and_pid() {
    let (db, path) = fresh_store("scan");
    let now = at(2024, 7, 1, 12, 0, 0, 0);

    // EXECUTED control (separate from the lifecycle below): a hand-defaulted row gets
    // the DDL sentinel '0000-00-00 00:00:00', which Go's `<> zero` predicate counts as
    // in progress and which `Put` can never write — and the id-scoped legs never
    // normalize it.
    fixture_insert(
        &db,
        "INSERT INTO library (name, path) VALUES (?, ?)",
        &[text("hand"), text("/hand")],
    );
    assert!(
        scan_in_progress(&db),
        "the fresh DDL sentinel stays counted"
    );
    assert_eq!(
        scalar::<String>(
            &db,
            "SELECT last_scan_started_at FROM library WHERE name = 'hand'"
        ),
        "0000-00-00 00:00:00",
        "the sentinel text is the DDL default, not the canonical zero"
    );
    fixture_insert(
        &db,
        "INSERT INTO library (name, path) VALUES (?, ?)",
        &[text("hand2"), text("/hand2")],
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library WHERE last_scan_started_at <> '0001-01-01 00:00:00+00:00'"),
        2,
        "both hand-defaulted rows are counted by the predicate's own comparison"
    );
    fixture_insert(&db, "DELETE FROM library", &[]);
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library"),
        0,
        "the sentinel rows are gone, so the aggregate asserts below see only rows Put wrote"
    );

    // A row written by put_in_tx carries whatever stamps the record supplies — here a
    // fresh library's year-one zeros, which the idle predicate reads as NOT in progress
    // (unlike the DDL sentinel above). dbx inserts every field, so these are the
    // record's values, not `now`: a record whose last_scan_started_at is some real
    // instant is counted in progress by Go's predicate too.
    let mut fresh = record(0, "putidle");
    fresh.last_scan_at = time::zero();
    fresh.last_scan_started_at = time::zero();
    let id = put_committed(&db, &fresh, &now, &[]).id;
    assert_eq!(
        id, 3,
        "`autoincrement` never reuses the deleted sentinel rowids, so the first Put row is 3"
    );
    let idle_stamp: String = scalar(
        &db,
        "SELECT last_scan_started_at FROM library WHERE name = 'name-putidle'",
    );
    assert_eq!(idle_stamp, time::to_text(&time::zero()));
    assert_eq!(
        get(&db, id).expect("Put row").last_scan_started_at,
        time::zero(),
        "inherited record shape: the sentinel/zero decodes as year-one, not Option"
    );
    assert!(
        !scan_in_progress(&db),
        "EXECUTED: a store whose only row came from put_in_tx is idle"
    );

    let start = at(2024, 9, 1, 8, 0, 0, 10_000_000);
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_begin_in_tx(tx, id, &start, true))
        .expect("ScanBegin");
    assert!(scan_in_progress(&db), "the begun scan counts");
    let begun: (String, i64) = lease(&db, |conn| {
        conn.query_row(
            "SELECT last_scan_started_at, full_scan_in_progress FROM library WHERE id = ?1",
            [integer(id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    });
    assert_eq!(begun, (time::to_text(&start), 1), "exact caller TEXT stamp");

    let finish = at(2024, 9, 1, 9, 0, 0, 20_000_000);
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_end_in_tx(tx, id, &finish))
        .expect("ScanEnd");
    assert!(
        !scan_in_progress(&db),
        "EXECUTED: ScanEnd returns the whole store to idle"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library WHERE last_scan_started_at <> '0001-01-01 00:00:00+00:00'"),
        0,
        "ScanEnd resets to the same zero the idle predicate compares against, store-wide"
    );
    let ended = get(&db, id).expect("row after ScanEnd");
    assert_eq!(ended.last_scan_at, finish);
    assert_eq!(ended.last_scan_started_at, time::zero());
    assert!(!ended.full_scan_in_progress);

    // Second scan (full_scan false) is caught again, and the aggregate predicate goes
    // in progress a second time from the same idle store.
    let second = at(2024, 9, 1, 10, 0, 0, 0);
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_begin_in_tx(tx, id, &second, false))
        .expect("second ScanBegin");
    let again: (String, i64) = lease(&db, |conn| {
        conn.query_row(
            "SELECT last_scan_started_at, full_scan_in_progress FROM library WHERE id = ?1",
            [integer(id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    });
    assert_eq!(again, (time::to_text(&second), 0), "full_scan=false stored");
    assert!(
        scan_in_progress(&db),
        "EXECUTED: idle → begin → end → idle → second begin"
    );

    // The PID spec strings round-trip verbatim; no effective-PID logic lives here.
    db.try_with_tx_sync::<(), Infallible>(|tx| {
        set_scanned_pid_in_tx(tx, id, "%{ album }%", "%{ track }%")
    })
    .expect("SetScannedPID");
    let pid = get(&db, id).expect("row after PID");
    assert_eq!(pid.scanned_pid_album, "%{ album }%");
    assert_eq!(pid.scanned_pid_track, "%{ track }%");
    assert_eq!(
        pid.pid_album, "pidalbum-putidle",
        "the spec columns are distinct"
    );

    // Go checks no row count: a missing id is Ok and changes nothing.
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_begin_in_tx(tx, 9999, &start, true))
        .expect("ScanBegin on a missing id is a no-op, not NotFound");
    db.try_with_tx_sync::<(), Infallible>(|tx| scan_end_in_tx(tx, 9999, &finish))
        .expect("ScanEnd on a missing id is a no-op");
    db.try_with_tx_sync::<(), Infallible>(|tx| set_scanned_pid_in_tx(tx, 9999, "a", "t"))
        .expect("SetScannedPID on a missing id is a no-op");
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library"), 1);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// Rollback: a failure inside the admin-assignment leg leaves no library row and no
/// links, and a generated id is never published as a committed result. A fixture
/// trigger (test-local DDL only — production `nd-dbschema` is untouched) fails that
/// leg on a poisoned library id.
#[test]
fn library_assign_or_insert_failure_rolls_back() {
    let (db, path) = fresh_store("rollback");
    seed_user(&db, "adm", "Admin", true);
    seed_artist(&db, "ar-ok");
    let poison: i64 = 4242;
    // SQLite forbids bound variables inside a trigger body, so the id is spelled in
    // the fixture DDL itself. Test-local DDL only: crates/nd-dbschema is untouched.
    fixture_insert(
        &db,
        &format!(
            "CREATE TRIGGER fixture_assign_leg_fails BEFORE INSERT ON user_library \
             FOR EACH ROW WHEN NEW.library_id = {poison} \
             BEGIN SELECT RAISE(ABORT, 'fixture: assignment leg fails'); END"
        ),
        &[],
    );
    let now = at(2024, 7, 1, 12, 0, 0, 0);

    // Update-miss INSERT arm for the poisoned id: the row exists inside the tx, so the
    // CROSS JOIN emits it and the assignment leg aborts.
    let poisoned = record(poison, "poisoned");
    let failed = db.try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
        put_in_tx(tx, &poisoned, &now, &[])
    });
    let err = failed.expect_err("the assignment leg aborts");
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
            assert!(!is_busy(source), "a constraint failure is not retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library"),
        0,
        "the library row written before the failure rolled back"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM user_library"),
        0,
        "the partial assignment rolled back too"
    );
    assert!(
        matches!(err, DbRuntimeError::Sqlite { .. }),
        "the failed Put carries no LibraryWriteResult, so the generated/explicit id was \
         never published as committed"
    );
    assert!(
        matches!(get(&db, poison), Err(LibraryError::NotFound)),
        "the failed Put's id is advisory: after rollback the row is not readable"
    );
    lease(&db, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable");
    });
    assert_eq!(db.idle_count().expect("pool state"), 2);

    // Same tx, later leg: a caller FK failure after a successful Put rolls back the row
    // AND the links. add_artist_in_tx has no domain arm, so escalate() is the only
    // translation available.
    let row = record(0, "keepme");
    let combined = db.try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
        let result = put_in_tx(tx, &row, &now, &[])?;
        add_artist_in_tx(tx, result.id, "no-such-artist").map_err(escalate)?;
        Ok(result)
    });
    let err = combined.expect_err("the FK failure aborts the transaction");
    let source = match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            source
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    };
    assert_eq!(
        extended_code(source),
        Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        "raw SQLite retry class retained"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library"),
        0,
        "the earlier Put row is not committed"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM user_library"), 0);

    // Drop the poisoned fixture trigger and re-run the same Put successfully.
    fixture_insert(&db, "DROP TRIGGER fixture_assign_leg_fails", &[]);
    let result = put_committed(&db, &row, &now, &[]);
    assert!(result.inserted);
    assert_eq!(get(&db, result.id).expect("re-run row").name, "name-keepme");
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM user_library"), 1);

    // The link leg itself: idempotent, and both FK legs are hard errors.
    db.try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, result.id, "ar-ok"))
        .expect("AddArtist");
    db.try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, result.id, "ar-ok"))
        .expect("a duplicate link is a no-op (ON CONFLICT DO NOTHING)");
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library_artist"),
        1,
        "the duplicate wrote nothing and overwrote nothing"
    );
    let missing_artist =
        db.try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, result.id, "ghost"));
    let err = missing_artist.expect_err("a missing artist violates the FK");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    let missing_library =
        db.try_with_tx_sync::<(), Infallible>(|tx| add_artist_in_tx(tx, 9999, "ar-ok"));
    let err = missing_library.expect_err("a missing library violates the FK");
    assert!(
        matches!(err, DbRuntimeError::Sqlite { .. }),
        "Go returns the raw error unwrapped (:156-158)"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library_artist"),
        1,
        "neither failed link leg left a row"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The DDL unique constraints surface as typed SQLite failures with their extended
/// code intact (and are not busy-classified, so the executor does not retry them), and
/// `name`/`path` collisions are refused by the schema rather than by repository policy.
/// Each case varies exactly ONE column, so the index that rejects it is unambiguous.
#[test]
fn library_duplicate_name_and_path_surface_typed_sqlite_constraints() {
    let (db, path) = fresh_store("unique");
    let now = at(2024, 7, 1, 12, 0, 0, 0);
    put_committed(&db, &record(0, "first"), &now, &[]);

    // Same name, DIFFERENT path: only `library.name` can be the violated index.
    let mut same_name = record(0, "first");
    same_name.path = "/path-other".to_string();
    let err = db
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, &same_name, &now, &[])
        })
        .expect_err("library.name is declared `not null unique`");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            assert_eq!(
                extended_code(source),
                Some(ffi::SQLITE_CONSTRAINT_UNIQUE),
                "the extended code is retained, not stringified"
            );
            assert!(!is_busy(source), "a uniqueness failure is never retried");
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    // Same path, different name, on a NON-ID-1 row: no repository rule forbids it, so
    // the DDL's unique index is what rejects it (Go is in the same position).
    let mut same_path = record(0, "second");
    same_path.name = "name-other".to_string();
    same_path.path = "/path-first".to_string();
    let err = db
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, &same_path, &now, &[])
        })
        .expect_err("library.path is declared `not null unique`");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            assert_eq!(extended_code(source), Some(ffi::SQLITE_CONSTRAINT_UNIQUE));
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }

    // Nothing from the failed Puts reached the table, and no partial links exist.
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM library"), 1);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM user_library"), 0);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

/// The ID-1 guard lookup errors HARD. Go treats ANY `Get(1)` error as "row absent, so
/// creating it is ok" (`library_repository.go:75-80`), which swallows a scan failure and
/// lets the Put run its normal ID-1 branch — the UPDATE leg — repairing the damaged cell
/// instead of reporting it. The port propagates the failure — only the no-rows case means
/// missing — and this pins that documented divergence: the damaged cell stays damaged and
/// no write at all happens. (`id` is an `INTEGER PRIMARY KEY`, so a second library 1 was
/// never possible either way.)
#[test]
fn library_id1_guard_propagates_a_scan_failure_instead_of_treating_it_as_absent() {
    let (db, path) = fresh_store("guard-error");
    let now = at(2024, 7, 1, 12, 0, 0, 0);
    put_committed(&db, &record(1, "libone"), &now, &[]);

    // Poison a cell the record reader cannot scan: Go's non-pointer `RemotePath`
    // string rejects NULL (LibraryError::Sqlite documents that scan failure).
    fixture_insert(
        &db,
        "UPDATE library SET remote_path = NULL WHERE id = 1",
        &[],
    );

    let same = record(1, "libone");
    let err = db
        .try_with_tx_sync::<LibraryWriteResult, LibraryWriteError>(|tx| {
            put_in_tx(tx, &same, &now, &[])
        })
        .expect_err("a broken guard lookup is not silence");
    match &err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(*stage, TransactionStage::Body);
            assert!(!is_busy(source));
        }
        DbRuntimeError::Domain(arm) => panic!("expected the raw scan failure, got {arm:?}"),
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM library"),
        1,
        "no create and no repair: the guard failure aborted the Put before any statement"
    );
    assert_eq!(
        scalar::<Option<String>>(&db, "SELECT remote_path FROM library WHERE id = 1"),
        None,
        "EXECUTED: remote_path is still NULL, so the bypassed UPDATE leg never repaired it"
    );
    let row = get(&db, 1);
    assert!(
        matches!(row, Err(LibraryError::Sqlite(..))),
        "the read side still reports the same broken cell: {row:?}"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}
