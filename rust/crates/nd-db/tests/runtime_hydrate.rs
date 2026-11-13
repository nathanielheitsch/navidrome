//! S-D3a golden — the shared hydration seam EXECUTED against a real SQLite
//! DB: fresh schema via `nd_dbschema`, one isolated temp file per test
//! (harness shape copied from `tests/runtime_insert_alignment.rs`).
//!
//! Go truth under test: `dbUser`/`PostScan`
//! (`persistence/user_repository.go:30-43`) promoted onto the S-D3 shared
//! seam (`nd-db/src/hydrate.rs`), plus the seam's own NULL/sentinel/
//! absent-column rules (`docs/rust-port/hydrate-scan-contract.md` §4a/§4c).
//! `_foreign_keys=on` is Go's `db/db.go:55` DSN parameter.

use nd_db::hydrate::{self, HydrateError, RowGet};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::user_repo::{UserError, UserRepo, USER_TABLE};
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled so `SEEDEDRAND`/`NATSORT` are registered per
/// connection (`schema-contract.md` §4).
fn store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-hydrate-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one repository call on a lease; lease/store errors panic, the
/// repository's `UserError` is handed back untouched for assertion.
fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, UserError>,
) -> Result<T, UserError> {
    // The closure's Ok wraps the repository Result, so the inner
    // `UserError` crosses the rusqlite-typed lease boundary untouched
    // (the same shape `tests/runtime_user_repo.rs` uses).
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// A library row (`name`/`path` are `not null unique` in the fresh DDL).
fn insert_library(store: &SQLStore, name: &str, path: &str, default_new_users: bool) -> i64 {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "INSERT INTO library(name, path, remote_path, default_new_users) VALUES(?,?,?,?)",
                rusqlite::params![name, path, "", Value::from(default_new_users)],
            )?;
            conn.query_row("SELECT id FROM library WHERE name = ?", [name], |r| {
                r.get::<_, i64>(0)
            })
        })
        .unwrap_or_else(|e| panic!("library insert failed: {e}"))
}

/// A user row written straight to SQL.
fn insert_user(store: &SQLStore, id: &str, user_name: &str, is_admin: bool, updated_at: &str) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {USER_TABLE}(id, user_name, name, email, password, is_admin, \
                     created_at, updated_at) VALUES(?,?,?,?,?,?,?,?)"
                ),
                rusqlite::params![
                    id,
                    user_name,
                    user_name,
                    format!("{user_name}@x.y"),
                    "CIPHER",
                    Value::from(is_admin),
                    T0,
                    updated_at
                ],
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("user insert failed: {e}"));
}

/// A user WITH libraries, read through the repo: the seam's two phases
/// (`row_to_user` → `user_post_scan`) must hydrate `Libraries` from the
/// select's `libraries_json` aggregate.
#[test]
fn user_with_libraries_hydrates_them_from_the_seam() {
    // Arrange:
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", false);
    let jazz = insert_library(&store, "Jazz", "/jazz", true);
    insert_user(&store, "u1", "admin", true, T1);
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("u1", &[rock, jazz])
    })
    .unwrap();
    // Act:
    let got = on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap();
    // Assert: the libraries are hydrated (id and name at minimum).
    let ids: Vec<i64> = got.libraries.iter().map(|l| l.id).collect();
    assert_eq!(ids, vec![rock, jazz]);
    assert!(got
        .libraries
        .iter()
        .any(|l| l.name == "Rock" && l.path == "/rock"));
    assert!(got
        .libraries
        .iter()
        .any(|l| l.name == "Jazz" && l.path == "/jazz"));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A user WITHOUT libraries: the select's `COALESCE(…, '[]')` makes
/// `libraries_json` the literal `"[]"`, which must decode to an EMPTY
/// (non-nil) vec, not an error.
#[test]
fn user_without_libraries_decodes_the_coalesced_empty_set() {
    // Arrange:
    let (store, path) = store();
    insert_library(&store, "Rock", "/rock", false);
    insert_user(&store, "solo", "solo", false, T1);
    // Act:
    let got = on(&store, |conn| UserRepo::new(conn).get("solo")).unwrap();
    // Assert: empty, and the error path was not taken (unwrap above).
    assert!(got.libraries.is_empty());
    assert_eq!(got.libraries.capacity(), 0, "the vec, not a nil stand-in");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A malformed `libraries_json` written directly via SQL must surface as a
/// decode failure carrying Go's EXACT prefix, not a panic. Exercised
/// through the seam on a row shaped like the select's (the same decode the
/// read path runs), surfaced the same way `UserRow::into_record` maps it.
#[test]
fn malformed_libraries_json_yields_decoded_with_gos_prefix() {
    // Arrange: a user whose `libraries_json` is corrupted via direct SQL.
    let (store, path) = store();
    insert_user(&store, "broken", "broken", false, T1);
    let bogus = "[{\"id\": 7, oops]";
    let row_probe = format!(
        "SELECT {USER_TABLE}.*, '{bogus}' AS libraries_json FROM {USER_TABLE} WHERE id = 'broken'"
    );
    // Act: raw row, then the decode phase; surface the seam's error the
    // same way `into_record` does (`into_sqlite`: Decoded → conversion
    // failure carrying the Go prefix).
    let err = store
        .with_connection_sync(|conn| {
            let mut stmt = conn.prepare(&row_probe)?;
            let row = stmt.query_row([], hydrate::row_to_user)?;
            Ok(hydrate::user_post_scan(row).map_err(|err| err.into_sqlite()))
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
        .unwrap_err();
    // Assert: the repo error is the sqlite conversion failure Go's
    // ScanStruct error takes, with Go's prefix up front.
    let rusqlite::Error::FromSqlConversionFailure(_, _, msg) = err else {
        panic!("expected a conversion failure, got {err:?}");
    };
    assert!(
        msg.to_string()
            .starts_with("parsing user libraries from db: "),
        "{msg}"
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The seam's own error type: `Decoded` carries Go's prefix verbatim and
/// the sqlite variant round-trips through it transparently.
#[test]
fn hydrate_error_carries_gos_prefix_verbatim() {
    // Act:
    let err = HydrateError::Decoded(
        "parsing user libraries from db: invalid type: map, expected a sequence at line 1 column 2"
            .to_string(),
    );
    // Assert: the Display IS the stored string — prefix verbatim, nothing
    // added, nothing rewritten.
    assert_eq!(
        err.to_string(),
        "parsing user libraries from db: invalid type: map, expected a sequence at line 1 column 2"
    );
    // Act: the into_sqlite mapping the repo uses.
    let sqlite =
        HydrateError::Decoded("parsing user libraries from db: nope".to_string()).into_sqlite();
    // Assert: a conversion failure whose payload is the Go-prefixed text.
    let rusqlite::Error::FromSqlConversionFailure(_, ty, msg) = sqlite else {
        panic!("expected a conversion failure, got {sqlite:?}");
    };
    assert_eq!(ty, rusqlite::types::Type::Text);
    assert_eq!(msg.to_string(), "parsing user libraries from db: nope");
}

/// RowGet NULL parity, executed: a NULL into a required getter errors;
/// NULL into `opt_dt` yields `None` (Go's nil pointer). A non-NULL value the
/// driver cannot parse — the `0000-00-00 00:00:00` sentinel and outright
/// garbage — yields `Some(Go's zero time)`: the driver assigns `time.Time{}`
/// with NO error (`go-sqlite3@v1.14.52/sqlite3.go:2832-2842`) and
/// `database/sql` allocates a pointer for it (`convert.go:495-496`).
#[test]
fn row_get_null_parity_and_sentinel() {
    let (store, path) = store();
    store
        .with_connection_sync(|conn| {
            conn.execute_batch(
                "CREATE TABLE probe (
                    id TEXT, flag INTEGER, num INTEGER, real_col REAL,
                    t TEXT, opt_t TEXT);
                 INSERT INTO probe VALUES ('p', NULL, NULL, NULL, NULL, NULL);
                 INSERT INTO probe VALUES ('s', 0, 1, 1.5, '0000-00-00 00:00:00', '2024-01-02 03:04:05+00:00');
                 INSERT INTO probe VALUES ('g', 1, 2, 2.5, 'not a date', 'nope');",
            )?;
            // Act / Assert: NULLs into the required getters all error.
            conn.query_row("SELECT * FROM probe WHERE id = 'p'", [], |row| {
                assert!(row.get_text("id").is_ok(), "id itself is present");
                assert!(row.get_text("t").is_err(), "NULL text must error");
                assert!(row.get_bool("flag").is_err(), "NULL bool must error");
                assert!(row.get_i32("num").is_err(), "NULL i32 must error");
                assert!(row.get_i64("num").is_err(), "NULL i64 must error");
                assert!(row.get_f64("real_col").is_err(), "NULL f64 must error");
                // Act / Assert: the opt getters yield None on NULL.
                assert_eq!(row.get_opt_text("t")?, None);
                assert_eq!(row.get_opt_f64("real_col")?, None);
                assert_eq!(row.get_opt_dt("t")?, None, "NULL is absent");
                Ok(())
            })
            .expect("NULL row reads");
            // Act / Assert: the sentinel scans as a POINTER TO Go's zero time,
            // a real date as itself.
            conn.query_row("SELECT * FROM probe WHERE id = 's'", [], |row| {
                assert_eq!(
                    row.get_opt_dt("t")?,
                    Some(hydrate::go_zero_time()),
                    "the sentinel is a pointer to time.Time{{}}, not an absent value"
                );
                assert!(
                    hydrate::is_go_zero(&row.get_dt("t")?),
                    "the same text scans into a non-pointer field as Go's zero"
                );
                let got = row.get_opt_dt("opt_t")?.expect("real date");
                assert_eq!(nd_db::time::to_text(&got), "2024-01-02 03:04:05+00:00");
                assert!(!hydrate::is_go_zero(&got), "a real stamp is not zero");
                Ok(())
            })
            .expect("sentinel row reads");
            // Act / Assert: garbage is NOT a scan error — Go's driver reports no
            // error and hands back the zero time.
            conn.query_row("SELECT * FROM probe WHERE id = 'g'", [], |row| {
                assert_eq!(
                    row.get_opt_dt("t")?,
                    Some(hydrate::go_zero_time()),
                    "unparseable text is Go's zero time, with no error"
                );
                assert_eq!(
                    row.get_opt_dt("opt_t")?,
                    Some(hydrate::go_zero_time()),
                    "unparseable text is Go's zero time, with no error"
                );
                assert!(
                    row.get_dt("t").is_ok(),
                    "a non-pointer field scans it too (Go's zero)"
                );
                Ok(())
            })
            .expect("garbage row reads");
            Ok(())
        })
        .unwrap_or_else(|e| panic!("probe failed: {e}"));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The absent-column tolerance rule: a `query_row` on a SELECT that lacks
/// the optional `libraries_json` column must surface a rusqlite error, not
/// panic, and `has_column` reports the absence for the opt-by-name path
/// the S-D3c annotation reader will use.
#[test]
fn absent_optional_column_is_an_error_not_a_panic() {
    let (store, path) = store();
    insert_user(&store, "plain", "plain", false, T1);
    // Act: the same user columns the read path needs, minus libraries_json.
    let sql = format!(
        "SELECT id, user_name, name, email, is_admin, last_login_at, last_access_at, \
         created_at, updated_at, scrobble_filter, password, token_epoch \
         FROM {USER_TABLE} WHERE id = 'plain'"
    );
    let err = store
        .with_connection_sync(|conn| Ok(conn.query_row(&sql, [], hydrate::row_to_user)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
        .unwrap_err();
    // Assert: an honest rusqlite error, not a panic — the mapper reads
    // libraries_json last, so that is the missing column it reports.
    let rusqlite::Error::InvalidColumnName(name) = err else {
        panic!("expected missing-column error, got {err:?}");
    };
    assert_eq!(&*name, "libraries_json");
    // Act / Assert: has_column is the tolerant opt-by-name probe.
    store
        .with_connection_sync(|conn| {
            conn.query_row(&sql, [], |row| {
                assert!(!row.has_column("libraries_json"), "absent on light queries");
                assert!(row.has_column("id"));
                Ok(())
            })
        })
        .unwrap_or_else(|e| panic!("store lease failed: {e}"));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}
