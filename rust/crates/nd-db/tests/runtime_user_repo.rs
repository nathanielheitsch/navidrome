//! #05 milestone-2 golden — the user repository EXECUTED against a real SQLite
//! DB: fresh schema via `nd_dbschema`, one isolated temp file per test.
//!
//! Go truth under test: `persistence/user_repository.go` Get/GetAll/
//! FindByUsername/FindFirstAdmin/CountAll/Put/Delete/SetUserLibraries and the
//! `dbUser`/`PostScan` hydration (`:30-43`), plus the token-epoch revocation
//! (`:129-144`), whose behaviours `user_repository_test.go:696-746` pin. Row
//! mapping follows dbx `ScanStruct` (`rows.go:65-103`), established against the
//! real dbx v1.12.0 scanner rather than guessed.

use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_db::user_repo::{parse_libraries_json, UserError, UserPutValues, UserRepo, USER_TABLE};
use nd_dbschema::schema::apply_fresh_schema;

const T0: &str = "2024-01-01T00:00:00Z";
const T1: &str = "2024-02-02T00:00:00Z";
const T2: &str = "2024-03-03T00:00:00Z";

/// One isolated on-disk DB per invocation: a unique temp path (never a fixed
/// `/tmp` name two runs could share), freshly migrated, pooled through
/// `nd_dbschema::schema::open` so `SEEDEDRAND`/`NATSORT` are registered per
/// connection (`schema-contract.md` §4). `_foreign_keys=on` is Go's
/// `db/db.go:55` DSN parameter, so `user_library`'s CASCADE really runs.
fn store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-user-", ".db"));
    // A leftover file would make the unique-name assumption false; a missing
    // one is the expected first-run state, so existence is probed rather than
    // an error code matched.
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

/// Run one repository call on a lease. The checkout/store error is unwrapped
/// here; the repository's own `UserError` is handed back untouched so the
/// assertion can match on `NotFound`.
fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, UserError>,
) -> Result<T, UserError> {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// A library row. `name`/`path` are `not null unique` in the fresh DDL
/// (`schema_ddl.rs:52`), so each call takes its own pair.
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

/// A user row written straight to SQL, for the paths Put does not own (a
/// pre-bumped epoch, an older `updated_at` for the sort probe).
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

/// Assert a `SELECT count(*)` probe comes back with no matching rows.
fn assert_no_rows(store: &SQLStore, expr: &str) {
    let count = store
        .with_connection_sync(move |conn| conn.query_row(expr, [], |r| r.get::<_, i64>(0)))
        .unwrap_or_else(|e| panic!("{expr} failed: {e}"));
    assert_eq!(count, 0, "{expr} expected no rows");
}

fn values<'a>(
    id: &'a str,
    user_name: &'a str,
    name: &'a str,
    password: Option<&'a str>,
    is_admin: bool,
) -> UserPutValues<'a> {
    UserPutValues {
        id,
        user_name,
        name,
        email: "jdoe@example.com",
        password,
        is_admin,
        last_login_at: None,
        last_access_at: None,
        created_at: T0,
        updated_at: T1,
        scrobble_filter: "",
    }
}

fn ids(user: &nd_db::user_repo::UserRecord) -> Vec<i64> {
    user.libraries.iter().map(|library| library.id).collect()
}

/// Get (`user_repository.go:90-97`) plus `PostScan` (`:36-43`): every scanned
/// column, the libraries from the LEFT JOINed aggregate, NULL optional
/// timestamps as `None`.
#[test]
fn get_returns_the_row_and_hydrates_its_libraries() {
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
    // Assert:
    assert_eq!(got.id, "u1");
    assert_eq!(got.user_name, "admin");
    assert_eq!(got.email, "admin@x.y");
    assert!(got.is_admin);
    assert_eq!(got.last_login_at, None, "NULL is Go's nil *time.Time");
    assert_eq!(got.last_access_at, None);
    assert_eq!(got.created_at, T0);
    assert_eq!(got.updated_at, T1);
    assert_eq!(got.scrobble_filter, "");
    // Assert: stored ciphertext, untouched — decryption belongs to
    // FindByUsernameWithPassword (`:209-216`), i.e. nd-auth.
    assert_eq!(got.password, "CIPHER");
    assert_eq!(got.token_epoch, 0, "user_repository_test.go:712-717");
    // Assert: the hydrated library set.
    assert_eq!(ids(&got), vec![rock, jazz]);
    assert_eq!(got.libraries[0].name, "Rock");
    assert_eq!(got.libraries[0].path, "/rock");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A user with no associations reads `libraries_json = '[]'`: the COALESCE plus
/// `FILTER (WHERE library.id IS NOT NULL)` of `:66-81`. Without the filter the
/// LEFT JOIN's NULL row would render `[null]`.
#[test]
fn get_coalesces_a_user_without_libraries_to_the_empty_set() {
    // Arrange:
    let (store, path) = store();
    insert_library(&store, "Rock", "/rock", false);
    insert_user(&store, "solo", "solo", false, T1);
    // Act:
    let got = on(&store, |conn| UserRepo::new(conn).get("solo")).unwrap();
    // Assert:
    assert!(got.libraries.is_empty());
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `queryOne`'s `sql.ErrNoRows → model.ErrNotFound`
/// (`sql_base_repository.go:382-385`).
#[test]
fn get_and_find_first_admin_report_not_found() {
    // Arrange: no admin exists at all.
    let (store, path) = store();
    insert_user(&store, "plain", "plain", false, T1);
    // Act / Assert:
    assert!(matches!(
        on(&store, |conn| UserRepo::new(conn).get("nope")),
        Err(UserError::NotFound)
    ));
    assert!(matches!(
        on(&store, |conn| UserRepo::new(conn).find_first_admin()),
        Err(UserError::NotFound)
    ));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// FindByUsername (`:154-160`) — `model/user.go:65` requires case-insensitive
/// lookup, and the value stays bound: an injection-shaped username is a literal
/// that matches nothing.
#[test]
fn find_by_username_is_case_insensitive_and_binds_its_value() {
    // Arrange: a mixed-case user_name, as user_repository_test.go:31-46 uses.
    let (store, path) = store();
    insert_user(&store, "123", "AdMiN", true, T1);
    for probe in ["admin", "ADMIN", "AdMiN"] {
        // Act:
        let got = on(&store, |conn| UserRepo::new(conn).find_by_username(probe)).unwrap();
        // Assert:
        assert_eq!(got.id, "123", "{probe} must hit the same row");
    }
    // Act / Assert: bound, never interpolated.
    assert!(matches!(
        on(&store, |conn| UserRepo::new(conn)
            .find_by_username("' OR 1=1 --")),
        Err(UserError::NotFound)
    ));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// FindFirstAdmin (`:146-152`): `Eq{"user.is_admin": true}` plus
/// `QueryOptions{Sort:"updated_at", Max:1}`. `buildSortOrder`
/// (`sql_base_repository.go:199-229`) appends the caller's `Order`, and Go
/// passes none, so it defaults to `asc` — the statement is
/// `ORDER BY updated_at LIMIT 1`, and the FIRST admin in ascending
/// `updated_at` order wins: the EARLIEST-updated one. Executed against SQLite
/// to confirm (a `ORDER BY updated_at DESC` reading would pick the other row).
/// Callers use it as "an admin to act as" (`core/auth/auth.go:157`
/// `WithAdminUser`, `scanner/phase_4_playlists.go:56`), never as "the newest".
#[test]
fn find_first_admin_takes_the_earliest_updated_admin() {
    // Arrange: an older admin, a newer admin, and a non-admin as new as the
    // newest admin (so the is_admin predicate must also hold).
    let (store, path) = store();
    insert_user(&store, "old-admin", "old", true, T1);
    insert_user(&store, "new-admin", "new", true, T2);
    insert_user(&store, "young-user", "young", false, T2);
    // Act:
    let got = on(&store, |conn| UserRepo::new(conn).find_first_admin()).unwrap();
    // Assert: ascending order + LIMIT 1 => the earliest-updated admin.
    assert_eq!(got.id, "old-admin");
    assert!(got.is_admin);
    assert_eq!(got.updated_at, T1);
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// GetAll (`:99-105`) under the caller's options, each row hydrated; Go's
/// `queryAll` yields an empty slice when the page runs past the last row, not
/// `ErrNotFound`. CountAll (`:86-88`) counts distinct ids.
#[test]
fn get_all_orders_pages_and_hydrates_every_row() {
    // Arrange:
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", true);
    insert_user(&store, "b", "bee", false, T1);
    insert_user(&store, "a", "ace", false, T2);
    insert_user(&store, "c", "cee", true, T0);
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("c", &[rock])
    })
    .unwrap();
    // Act: the full page in user_name order.
    let all = on(&store, |conn| {
        UserRepo::new(conn).get_all(&["user.user_name"], None, None)
    })
    .unwrap();
    // Assert:
    let got: Vec<&str> = all.iter().map(|user| user.id.as_str()).collect();
    assert_eq!(got, vec!["a", "b", "c"]);
    assert_eq!(all[2].libraries.len(), 1, "GROUP BY collapses the join");
    assert!(all[0].libraries.is_empty());
    // Act: a bounded, offset page.
    let page = on(&store, |conn| {
        UserRepo::new(conn).get_all(&["user.user_name"], Some(2), Some(1))
    })
    .unwrap();
    // Assert:
    let paged: Vec<&str> = page.iter().map(|user| user.id.as_str()).collect();
    assert_eq!(paged, vec!["b", "c"]);
    // Act / Assert: offset past the last row is an empty Vec, not an error.
    assert!(on(&store, |conn| UserRepo::new(conn).get_all(
        &["user.user_name"],
        None,
        Some(99)
    ))
    .unwrap()
    .is_empty());
    // Act / Assert: CountAll.
    assert_eq!(
        on(&store, |conn| UserRepo::new(conn).count_all()).unwrap(),
        3
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Put's password arm (`:129-144`): the epoch bump rides the same UPDATE and
/// `RETURNING token_epoch` hands the new value back — the increment-per-change
/// sequence of `user_repository_test.go:718-731`, and the revocation nd-auth
/// revalidates at `core/auth/auth.go:147`.
#[test]
fn put_bumps_the_token_epoch_once_per_password_change() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "u1", false, T1);
    // Act: first change on an existing row, by the user themselves.
    let first = on(&store, |conn| {
        UserRepo::new(conn).put(
            &values("u1", "u1", "U One", Some("enc-one"), false),
            T0,
            true,
        )
    })
    .unwrap();
    // Assert: an existing row, and `RETURNING token_epoch` yields the POST-bump
    // value (SQLite returns the updated row), which is the epoch the caller's
    // own token must be re-minted with.
    assert!(!first.is_new_user);
    assert_eq!(first.token_epoch, Some(1));
    assert!(first.refresh_inflight_token);
    // Act: a second change.
    let second = on(&store, |conn| {
        UserRepo::new(conn).put(
            &values("u1", "u1", "U One", Some("enc-two"), false),
            T0,
            true,
        )
    })
    .unwrap();
    // Assert: exactly one bump per change.
    assert_eq!(second.token_epoch, Some(2));
    // Act: the stored row.
    let got = on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap();
    // Assert:
    assert_eq!(got.token_epoch, 2);
    assert_eq!(
        got.password, "enc-two",
        "the encrypted payload is written verbatim"
    );
    // Act: an admin resetting another user must not refresh the caller's token.
    let other = on(&store, |conn| {
        UserRepo::new(conn).put(
            &values("u1", "u1", "U One", Some("enc-three"), false),
            T0,
            false,
        )
    })
    .unwrap();
    // Assert:
    assert_eq!(other.token_epoch, Some(3));
    assert!(!other.refresh_inflight_token, "user_repository.go:180-185");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The password-less arm (`:122-128`): a plain UPDATE that leaves the epoch
/// alone (`user_repository_test.go:733-746`) and keeps the stored password
/// (`:59-66`).
#[test]
fn put_without_a_password_updates_the_row_and_leaves_the_epoch() {
    // Arrange: a row whose epoch is already bumped.
    let (store, path) = store();
    insert_user(&store, "u1", "u1", false, T1);
    store
        .with_connection_sync(|conn| {
            conn.execute(
                &format!("UPDATE {USER_TABLE} SET token_epoch = 4 WHERE id = 'u1'"),
                [],
            )?;
            Ok(())
        })
        .unwrap();
    // Act: rename only.
    let outcome = on(&store, |conn| {
        UserRepo::new(conn).put(&values("u1", "u1", "Renamed", None, false), T0, true)
    })
    .unwrap();
    // Assert: no epoch was produced, so no in-flight token to refresh.
    assert!(!outcome.is_new_user);
    assert_eq!(outcome.token_epoch, None);
    assert!(!outcome.refresh_inflight_token);
    // Assert: name written, epoch and password untouched.
    let got = on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap();
    assert_eq!(got.name, "Renamed");
    assert_eq!(got.token_epoch, 4);
    assert_eq!(got.password, "CIPHER");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Put's INSERT fallback (`:140-147`): the UPDATE matched no row, so a fresh
/// row lands with the caller's `created_at` stamp. The password arm's
/// `RETURNING` produced nothing, which is Go's `ErrNotFound` → `isNewUser`
/// branch (`:135-137`).
#[test]
fn put_inserts_a_missing_row_with_the_fresh_created_at() {
    // Arrange:
    let (store, path) = store();
    // Act:
    let outcome = on(&store, |conn| {
        UserRepo::new(conn).put(
            &values("new-id", "newbie", "Newbie", Some("enc-one"), false),
            T2,
            true,
        )
    })
    .unwrap();
    // Assert:
    assert!(outcome.is_new_user);
    assert_eq!(outcome.token_epoch, None);
    assert!(!outcome.refresh_inflight_token, "a new user has no token");
    // Assert: the row exists with the INSERT's created_at, not the struct's.
    let got = on(&store, |conn| UserRepo::new(conn).get("new-id")).unwrap();
    assert_eq!(got.created_at, T2);
    assert_eq!(got.updated_at, T1);
    assert_eq!(
        got.token_epoch, 0,
        "the column DEFAULT starts the epoch at 0"
    );
    assert_eq!(got.password, "enc-one");
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Put's library auto-assigns (`:149-165`): an admin gets every library on
/// every Put (`INSERT OR IGNORE` keeps re-Puts idempotent); a new regular user
/// gets only `default_new_users`, and only on creation.
#[test]
fn put_assigns_all_libraries_to_admins_and_defaults_to_new_users() {
    // Arrange: one default library and one non-default.
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", true);
    let jazz = insert_library(&store, "Jazz", "/jazz", false);
    // Act: a brand-new admin.
    on(&store, |conn| {
        UserRepo::new(conn).put(&values("adm", "adm", "Adm", Some("p"), true), T0, true)
    })
    .unwrap();
    // Assert: both libraries.
    let admin = on(&store, |conn| UserRepo::new(conn).get("adm")).unwrap();
    assert_eq!(ids(&admin), vec![rock, jazz]);
    // Act: a brand-new regular user.
    on(&store, |conn| {
        UserRepo::new(conn).put(&values("reg", "reg", "Reg", Some("p"), false), T0, true)
    })
    .unwrap();
    // Assert: only the default library.
    let regular = on(&store, |conn| UserRepo::new(conn).get("reg")).unwrap();
    assert_eq!(ids(&regular), vec![rock]);
    // Act: re-Put the existing regular user without a password.
    on(&store, |conn| {
        UserRepo::new(conn).put(&values("reg", "reg", "Reg Renamed", None, false), T0, true)
    })
    .unwrap();
    // Assert: still only the default — the default assign is new-user only.
    let again = on(&store, |conn| UserRepo::new(conn).get("reg")).unwrap();
    assert_eq!(again.name, "Reg Renamed");
    assert_eq!(ids(&again), vec![rock]);
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// UpdateLastLoginAt/UpdateLastAccessAt (`:167-176`): Go discards the affected
/// row count, so an unknown id is a silent no-op rather than `ErrNotFound`.
#[test]
fn login_and_access_stamps_write_the_row_and_tolerate_a_miss() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "u1", false, T1);
    // Act:
    on(&store, |conn| {
        let repo = UserRepo::new(conn);
        repo.update_last_login_at("u1", T1)?;
        repo.update_last_access_at("u1", T2)?;
        repo.update_last_login_at("ghost", T2)?;
        repo.update_last_access_at("ghost", T2)
    })
    .unwrap();
    // Assert:
    let got = on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap();
    assert_eq!(got.last_login_at.as_deref(), Some(T1));
    assert_eq!(got.last_access_at.as_deref(), Some(T2));
    assert_no_rows(
        &store,
        &format!("SELECT count(*) FROM {USER_TABLE} WHERE id = 'ghost'"),
    );
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Delete (`:320-327`) through `deleteByID`
/// (`sql_base_repository.go:650-659`): a miss is `ErrNotFound`, and
/// `user_library`'s `ON DELETE CASCADE` (`schema_ddl.rs:69`) drops the
/// associations. Go turns that cascade into `EventUserUpdated` (`:321-326`),
/// which the events slice emits.
#[test]
fn delete_removes_the_row_and_cascades_the_associations() {
    // Arrange:
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", true);
    insert_user(&store, "u1", "u1", false, T1);
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("u1", &[rock])
    })
    .unwrap();
    // Act:
    on(&store, |conn| UserRepo::new(conn).delete("u1")).unwrap();
    // Assert: the association is gone with the row.
    assert_no_rows(
        &store,
        "SELECT count(*) FROM user_library WHERE user_id = 'u1'",
    );
    // Act / Assert: deleting again reports the miss.
    assert!(matches!(
        on(&store, |conn| UserRepo::new(conn).delete("u1")),
        Err(UserError::NotFound)
    ));
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// SetUserLibraries (`:497-512`): replace, shrink, and the empty-set skip
/// (`user_repository_test.go:373-410`).
#[test]
fn set_user_libraries_replaces_then_empties_the_set() {
    // Arrange:
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", true);
    let jazz = insert_library(&store, "Jazz", "/jazz", false);
    insert_user(&store, "u1", "u1", false, T1);
    // Act / Assert: two libraries.
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("u1", &[rock, jazz])
    })
    .unwrap();
    assert_eq!(
        ids(&on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap()),
        vec![rock, jazz]
    );
    // Act / Assert: replaced by one.
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("u1", &[jazz])
    })
    .unwrap();
    assert_eq!(
        ids(&on(&store, |conn| UserRepo::new(conn).get("u1")).unwrap()),
        vec![jazz]
    );
    // Act / Assert: the empty set deletes without inserting.
    on(&store, |conn| {
        UserRepo::new(conn).set_user_libraries("u1", &[])
    })
    .unwrap();
    assert!(on(&store, |conn| UserRepo::new(conn).get("u1"))
        .unwrap()
        .libraries
        .is_empty());
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// The async boundary: the repository runs inside
/// [`SQLStore::with_connection`], so checkout and every statement happen off
/// the executor (`persistence.go:171-185`).
#[tokio::test(flavor = "current_thread")]
async fn the_repository_runs_through_the_async_store_boundary() {
    // Arrange:
    let (store, path) = store();
    let rock = insert_library(&store, "Rock", "/rock", true);
    let put = store
        .with_connection(|conn| {
            Ok(UserRepo::new(conn).put(&values("u1", "u1", "U", Some("enc"), true), T0, true))
        })
        .await
        .unwrap()
        .unwrap();
    // Act:
    let got = store
        .with_connection(|conn| Ok(UserRepo::new(conn).find_by_username("U1")))
        .await
        .unwrap()
        .unwrap();
    // Assert:
    assert!(put.is_new_user);
    assert_eq!(got.id, "u1");
    assert_eq!(ids(&got), vec![rock]);
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// `PostScan`'s decode (`:36-43`) against the payload SQLite's `json_object`
/// really renders (verified with the sqlite3 CLI on the fresh DDL): booleans as
/// 0/1, NULL timestamps as JSON null. Go decodes that into `model.Library`,
/// whose json tags are camelCase (`model/library.go:14-35`), so only
/// `id`/`name`/`path` land and the rest stay zero — which is exactly what the
/// port's [`nd_db::user_repo::UserLibrary`] carries.
#[test]
fn libraries_json_decodes_only_the_keys_go_populates() {
    // Arrange: the rendered shape, including the number-for-bool and nulls.
    let payload = "[{\"id\":1,\"name\":\"Rock\",\"path\":\"/rock\",\"remote_path\":\"/remote\",\
                    \"last_scan_at\":\"2024-05-06T07:08:09Z\",\"last_scan_started_at\":null,\
                    \"full_scan_in_progress\":1,\"updated_at\":\"2024-01-01\",\"created_at\":\"2024-01-01\"},\
                   {\"id\":2,\"name\":\"Jazz\",\"path\":\"/jazz\",\"remote_path\":null,\
                    \"full_scan_in_progress\":0}]";
    // Act:
    let libraries = parse_libraries_json(payload).unwrap();
    // Assert:
    assert_eq!(libraries.len(), 2);
    assert_eq!(libraries[0].id, 1);
    assert_eq!(libraries[0].name, "Rock");
    assert_eq!(libraries[0].path, "/rock");
    assert_eq!(libraries[1].id, 2);
    // Assert: empty input is PostScan's skip branch (`:37`), not a parse error.
    assert!(parse_libraries_json("").unwrap().is_empty());
    // Assert: malformed JSON is an error, which Go returns out of ScanStruct.
    assert!(parse_libraries_json("{\"not\": an object}").is_err());
}
