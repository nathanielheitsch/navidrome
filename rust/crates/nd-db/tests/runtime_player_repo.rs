//! The player repository EXECUTED against a real SQLite DB: fresh schema via
//! `nd_dbschema`, one isolated temp file per test — the same harness shape as
//! `tests/runtime_user_repo.rs`.
//!
//! Go truth under test: `persistence/player_repository.go` Get/Read/FindMatch/
//! ReadAll/CountAll/CountByClient/Save/Put/Update/Delete/SetAPIKey/FindByAPIKey
//! plus the API-key hashing (`:195-198`) and the ownership rules
//! (`sql_base_repository.go:67-102,506-563`). Row mapping follows dbx
//! `ScanStruct` (`dbx@v1.12.0/rows.go:65-103`), established in `user_repo.rs`.
//!
//! The unique index that settles key races comes from the Go migration
//! `db/migrations/20260924010054_add_player_api_key_hash.sql:4`, which
//! `nd-dbschema`'s fresh DDL does not carry (`schema_ddl.rs:65` declares only
//! the column, and the crate emits no secondary indexes). [`apply_api_key_index`]
//! adds it so these tests mirror a migrated Go database, and the gap is reported
//! to the nd-dbschema lane.

use nd_db::player_repo::{
    hash_api_key, ApiKeyValidationError, LoggedUser, Options, PlayerError, PlayerRecord,
    PlayerRepo, PLAYER_TABLE,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const KEY_A: &str = "nds_0123456789abcdefghijkl";
const KEY_B: &str = "nds_ABCDEFGHIJKLMNOPQRSTUV";
const LAST_SEEN: &str = "2024-01-02 03:04:05";

/// One isolated on-disk DB per invocation: a unique temp path, freshly
/// migrated, pooled through `nd_db::pool::Pool` so `SEEDEDRAND`/`NATSORT` are
/// registered per connection. `_foreign_keys=on` is Go's `db/db.go:55` DSN
/// parameter, so `player.user_id`'s CASCADE really runs.
fn store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-player-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    {
        let conn = pool.checkout().unwrap();
        apply_fresh_schema(conn.conn()).unwrap();
        apply_api_key_index(conn.conn());
    }
    (SQLStore::new(pool), path)
}

/// The unique index Go's migration adds
/// (`20260924010054_add_player_api_key_hash.sql:4`).
fn apply_api_key_index(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS player_api_key_hash on player(api_key_hash);",
    )
    .unwrap();
}

/// A user row written straight to SQL — user creation belongs to `user_repo.rs`.
fn insert_user(store: &SQLStore, id: &str, name: &str, is_admin: bool) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "INSERT INTO user(id, user_name, name, email, password, is_admin, \
                 created_at, updated_at) VALUES(?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    id,
                    name,
                    name,
                    format!("{name}@example.com"),
                    "encrypted",
                    Value::from(is_admin),
                    "2024-01-01 00:00:00",
                    "2024-01-01 00:00:00",
                ],
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("user insert failed: {e}"));
}

/// A player row written straight to SQL, for paths `Put` does not own (a stored
/// hash, a `report_real_path` the record does not carry).
fn insert_player(
    store: &SQLStore,
    id: &str,
    user_id: &str,
    client: &str,
    name: &str,
    api_key_hash: Option<&str>,
) {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO {PLAYER_TABLE}(id, name, user_agent, user_id, client, ip, \
                     last_seen, max_bit_rate, transcoding_id, report_real_path, scrobble_enabled, \
                     api_key_hash) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)"
                ),
                rusqlite::params![
                    id,
                    name,
                    "Firefox/Linux",
                    user_id,
                    client,
                    "127.0.0.1",
                    LAST_SEEN,
                    128,
                    "",
                    Value::from(false),
                    Value::from(true),
                    api_key_hash.map(hash_api_key),
                ],
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("player insert failed: {e}"));
}

fn owner(id: &str) -> LoggedUser {
    LoggedUser {
        id: id.to_string(),
        is_admin: false,
    }
}

fn admin() -> LoggedUser {
    LoggedUser {
        id: "admin".to_string(),
        is_admin: true,
    }
}

/// Run one repository call on a lease; the store's own boundary error is
/// unwrapped, the repository's `PlayerError` handed back untouched.
fn on<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, PlayerError>,
) -> Result<T, PlayerError> {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease failed: {e}"))
}

/// Tear down the temp DB once the store released its pool.
fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

fn record(id: &str, user_id: &str) -> PlayerRecord {
    PlayerRecord {
        username: "jdoe".to_string(),
        id: id.to_string(),
        name: "NavidromeUI [Firefox/Linux]".to_string(),
        user_agent: "Firefox/Linux".to_string(),
        user_id: user_id.to_string(),
        client: "NavidromeUI".to_string(),
        ip: "127.0.0.1".to_string(),
        last_seen: Some(LAST_SEEN.to_string()),
        transcoding_id: String::new(),
        max_bit_rate: 128,
        report_real_path: true,
        scrobble_enabled: true,
        has_api_key: false,
        api_key: None,
    }
}

/// `Get` (`:54-59`) hydrates the joined row, and `has_api_key` is the select's
/// computed column — the hash itself is never exposed (`model/player.go:24-25`).
#[test]
fn get_hydrates_the_joined_row_and_computes_has_api_key() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_player(
        &store,
        "p1",
        "u1",
        "NavidromeUI",
        "NavidromeUI [Firefox/Linux]",
        Some(KEY_A),
    );
    insert_player(&store, "p2", "u1", "subsonic", "Subsonic", None);
    // Act:
    let got = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    let other = on(&store, |conn| PlayerRepo::new(conn).get("p2")).unwrap();
    // Assert:
    assert_eq!(got.id, "p1");
    assert_eq!(got.username, "jdoe", "the joined user_name");
    assert!(got.has_api_key);
    assert_eq!(got.api_key, None, "the plaintext is never read back");
    assert_eq!(got.last_seen.as_deref(), Some(LAST_SEEN));
    assert_eq!(got.max_bit_rate, 128);
    assert!(!got.report_real_path, "the fixture stores false");
    assert!(got.scrobble_enabled);
    assert!(!other.has_api_key);
    cleanup(store, path);
}

/// `Get` (`:54-59`) is `selectPlayer` + `Eq{"player.id": id}` — no ownership
/// restriction, so any caller can read any row by id
/// (`player_repository_test.go:71-87`). `Read` (`:106-111`) is the restricted
/// one and reports a foreign row as not-found (`:160-167`), never
/// permission-denied: `queryOne` simply sees no rows.
#[test]
fn get_is_unrestricted_and_read_is_owner_scoped() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "n", None);
    // Act: a stranger's Get succeeds (the id alone is enough).
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get("p1"))
            .unwrap()
            .id,
        "p1"
    );
    // Act: the restricted Read of the same row by that stranger misses.
    let missed = on(&store, |conn| {
        PlayerRepo::new(conn).read("p1", &owner("u2"))
    });
    // Assert: not-found, not permission-denied.
    assert!(matches!(missed, Err(PlayerError::NotFound)));
    // Assert: the owner and an admin both see it.
    assert!(on(&store, |conn| PlayerRepo::new(conn)
        .read("p1", &owner("u1")))
    .is_ok());
    assert!(on(&store, |conn| PlayerRepo::new(conn).read("p1", &admin())).is_ok());
    // Assert: a nonexistent id is not-found for both paths.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).get("ghost")),
        Err(PlayerError::NotFound)
    ));
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).read("ghost", &admin())),
        Err(PlayerError::NotFound)
    ));
    cleanup(store, path);
}

/// `FindMatch` (`:61-70`) is unrestricted and matches the exact triple; an
/// injection-shaped client binds as a value and matches nothing.
#[test]
fn find_match_uses_the_triple_and_is_unrestricted() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(
        &store,
        "p1",
        "u1",
        "NavidromeUI",
        "NavidromeUI [Firefox/Linux]",
        None,
    );
    // Act: another user's caller still resolves it (the API-key path needs this).
    let got = on(&store, |conn| {
        PlayerRepo::new(conn).find_match("u1", "NavidromeUI", "Firefox/Linux")
    })
    .unwrap();
    // Assert:
    assert_eq!(got.id, "p1");
    // Assert: the triple must match exactly — a different agent misses
    // (player_repository_test.go:66-69).
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).find_match(
            "u1",
            "NavidromeUI",
            "Chrome"
        )),
        Err(PlayerError::NotFound)
    ));
    // Assert: a quoted client is data, not SQL.
    assert!(matches!(
        on(&store, |conn| {
            PlayerRepo::new(conn).find_match("u1", "' or 1=1 --", "Firefox/Linux")
        }),
        Err(PlayerError::NotFound)
    ));
    cleanup(store, path);
}

/// `FindByAPIKey` (`:200-209`) resolves by hash and never by plaintext.
#[test]
fn find_by_api_key_resolves_by_hash() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "n", Some(KEY_A));
    // Act:
    let got = on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A)).unwrap();
    // Assert:
    assert_eq!(got.id, "p1");
    // Assert: the plaintext itself matches nothing, and an unknown/empty key is
    // not-found (player_repository_test.go:378-383).
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_B)),
        Err(PlayerError::NotFound)
    ));
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key("")),
        Err(PlayerError::NotFound)
    ));
    assert!(on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A)).is_ok());
    cleanup(store, path);
}

/// `Save` (`:133-163`) stamps the logged owner, mints an id, and writes the hash
/// in the same INSERT; the row is readable only by its hash.
#[test]
fn save_stamps_the_owner_mints_an_id_and_stores_only_the_hash() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    let new_player = PlayerRecord {
        id: String::new(),
        user_id: String::new(),
        api_key: Some(KEY_A.to_string()),
        ..record("", "u1")
    };
    // Act:
    let id = on(&store, |conn| PlayerRepo::new(conn).save(&new_player, "u1")).unwrap();
    // Assert: a generated id of the standard length, and the owner stamped.
    assert_eq!(id.len(), nd_id::ID_LENGTH);
    let got = on(&store, |conn| PlayerRepo::new(conn).get(&id)).unwrap();
    assert_eq!(got.user_id, "u1");
    assert!(got.has_api_key);
    // Assert: the stored column is the hash, never the key (:194-198).
    let stored = on(&store, |conn| {
        Ok(conn.query_row(
            &format!("SELECT api_key_hash FROM {PLAYER_TABLE} WHERE id = ?"),
            [id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )?)
    })
    .unwrap();
    assert_eq!(
        stored,
        Some(hash_api_key(KEY_A)),
        "stored column is the hash"
    );
    assert_ne!(stored, Some(KEY_A.to_string()));
    // Assert: a duplicate key is the unique validation error, not a raw driver
    // failure (:156-158).
    let clash = on(&store, |conn| PlayerRepo::new(conn).save(&new_player, "u1"));
    assert!(matches!(
        clash,
        Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
            message_key: "ra.validation.unique",
            ..
        }))
    ));
    cleanup(store, path);
}

/// `Save`'s guards: a foreign owner is permission-denied, a missing key is the
/// required validation error, a malformed key the format one, and a headless
/// caller cannot get an owner stamped (:135-147).
#[test]
fn save_guards_owner_key_presence_and_key_format() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    // Act/Assert: another user's owner is denied (:138-140).
    let foreign = record("", "u2");
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).save(&foreign, "u1")),
        Err(PlayerError::PermissionDenied)
    ));
    // Act/Assert: no key at all (:141-144).
    let no_key = PlayerRecord {
        api_key: None,
        ..record("", "u1")
    };
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).save(&no_key, "u1")),
        Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
            message_key: "ra.validation.required",
            ..
        }))
    ));
    // Act/Assert: an empty key is also required, not malformed.
    let empty_key = PlayerRecord {
        api_key: Some(String::new()),
        ..record("", "u1")
    };
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).save(&empty_key, "u1")),
        Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
            message_key: "ra.validation.required",
            ..
        }))
    ));
    // Act/Assert: a malformed key is the format message (:126-131).
    let bad_key = PlayerRecord {
        api_key: Some("nds_nope".to_string()),
        ..record("", "u1")
    };
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).save(&bad_key, "u1")),
        Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
            message_key: "resources.player.validation.apiKeyFormat",
            ..
        }))
    ));
    // Act/Assert: a headless caller's empty owner is NOT stamped with the
    // invalid id, so it is denied (:135-140).
    let headless = PlayerRecord {
        user_id: String::new(),
        api_key: Some(KEY_A.to_string()),
        ..record("", "")
    };
    assert!(matches!(
        on(&store, |conn| {
            PlayerRepo::new(conn).save(&headless, nd_db::sql_bookmarks::INVALID_USER_ID)
        }),
        Err(PlayerError::PermissionDenied)
    ));
    // Assert: nothing was written by any rejected call.
    let rows = on(&store, |conn| {
        Ok(
            conn.query_row(&format!("SELECT count(*) FROM {PLAYER_TABLE}"), [], |row| {
                row.get::<_, i64>(0)
            })?,
        )
    })
    .unwrap();
    assert_eq!(rows, 0);
    cleanup(store, path);
}

/// A Save that spoofed its id cannot overwrite an existing row: the id is always
/// freshly minted (`:153`), so the victim's row survives and the new row belongs
/// to the attacker (`player_repository_test.go:548-566`).
#[test]
fn save_with_a_spoofed_id_creates_a_new_row_and_leaves_the_victims_alone() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "victim", "v", false);
    insert_user(&store, "attacker", "a", false);
    insert_player(&store, "p1", "victim", "NavidromeUI", "original", None);
    let spoofed = PlayerRecord {
        id: "p1".to_string(),
        name: "HIJACKED".to_string(),
        api_key: Some(KEY_A.to_string()),
        ..record("p1", "attacker")
    };
    // Act:
    let id = on(&store, |conn| {
        PlayerRepo::new(conn).save(&spoofed, "attacker")
    })
    .unwrap();
    // Assert: a new row, not the victim's.
    assert_ne!(id, "p1");
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get(&id))
            .unwrap()
            .user_id,
        "attacker"
    );
    // Assert: the victim's row is untouched.
    let victim_row = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    assert_eq!(victim_row.name, "original");
    assert_eq!(victim_row.user_id, "victim");
    assert!(!victim_row.has_api_key);
    cleanup(store, path);
}

/// `Put` (`:42-45`) upserts: UPDATE first, INSERT when no row matched, and a
/// fresh id when the record came in without one. `api_key_hash` is untouched.
#[test]
fn put_updates_then_inserts_and_leaves_the_hash_alone() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "old name", Some(KEY_A));
    // Act: an existing id updates in place.
    let updated = PlayerRecord {
        name: "renamed".to_string(),
        ..record("p1", "u1")
    };
    let id = on(&store, |conn| PlayerRepo::new(conn).put(&updated)).unwrap();
    // Assert:
    assert_eq!(id, "p1");
    let got = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    assert_eq!(got.name, "renamed");
    assert!(got.has_api_key, "Put does not clear the stored hash");
    // Act: an unknown id inserts.
    let inserted = PlayerRecord {
        name: "brand new".to_string(),
        ..record("p2", "u1")
    };
    let id = on(&store, |conn| PlayerRepo::new(conn).put(&inserted)).unwrap();
    assert_eq!(id, "p2");
    // Act: an id-less record mints one.
    let fresh = PlayerRecord {
        id: String::new(),
        name: "minted".to_string(),
        ..record("", "u1")
    };
    let id = on(&store, |conn| PlayerRepo::new(conn).put(&fresh)).unwrap();
    assert_eq!(id.len(), nd_id::ID_LENGTH);
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get(&id))
            .unwrap()
            .name,
        "minted"
    );
    cleanup(store, path);
}

/// `Put` against an unknown owner fails on the foreign key, exactly as Go's
/// does (`player_repository_test.go:202-208`): ownership is enforced by the
/// schema, not by a Go-side check.
#[test]
fn put_to_an_unknown_owner_fails_on_the_foreign_key() {
    // Arrange:
    let (store, path) = store();
    // Act:
    let outcome = on(&store, |conn| {
        PlayerRepo::new(conn).put(&record("p1", "ghost"))
    });
    // Assert:
    assert!(matches!(outcome, Err(PlayerError::Sqlite(_))));
    cleanup(store, path);
}

/// `SetAPIKey` (`:211-223`): the owner can set and revoke; even an admin cannot
/// set a key for someone else, but can revoke it. Both are the point of the
/// access-level split (`:213` vs `:218`).
#[test]
fn set_api_key_is_owner_only_to_set_and_owner_or_admin_to_revoke() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "admin", "root", true);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "n", None);
    // Act: the owner sets a key.
    on(&store, |conn| {
        PlayerRepo::new(conn).set_api_key("p1", KEY_A, &owner("u1"))
    })
    .unwrap();
    // Assert: it resolves by hash and the flag flips.
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A))
            .unwrap()
            .id,
        "p1"
    );
    // Assert: an admin setting a key for another user is denied (:218), and the
    // owner's key survives.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).set_api_key(
            "p1",
            KEY_B,
            &admin()
        )),
        Err(PlayerError::PermissionDenied)
    ));
    assert!(on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A)).is_ok());
    // Assert: a stranger cannot set it either.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).set_api_key(
            "p1",
            KEY_B,
            &owner("u2")
        )),
        Err(PlayerError::PermissionDenied)
    ));
    // Act: an admin CAN revoke it (:213).
    on(&store, |conn| {
        PlayerRepo::new(conn).set_api_key("p1", "", &admin())
    })
    .unwrap();
    // Assert: NULL, so the hash lookup and the flag both go away.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A)),
        Err(PlayerError::NotFound)
    ));
    assert!(
        !on(&store, |conn| PlayerRepo::new(conn).get("p1"))
            .unwrap()
            .has_api_key
    );
    // Act/Assert: an unknown player is not-found (:554-563's other arm).
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).set_api_key(
            "ghost",
            "",
            &admin()
        )),
        Err(PlayerError::NotFound)
    ));
    // Act/Assert: a malformed key is rejected before any statement runs.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).set_api_key(
            "p1",
            "nds_bad",
            &owner("u1")
        )),
        Err(PlayerError::ApiKeyValidation(_))
    ));
    cleanup(store, path);
}

/// Two players cannot hold the same key: the unique index surfaces as the
/// `ra.validation.unique` validation error (`:219-221`) and the second player's
/// row is left alone (`player_repository_test.go:499-511`).
#[test]
fn a_key_already_in_use_is_the_unique_validation_error() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "holder", Some(KEY_A));
    insert_player(&store, "p2", "u1", "subsonic", "waiting", None);
    // Act:
    let clash = on(&store, |conn| {
        PlayerRepo::new(conn).set_api_key("p2", KEY_A, &owner("u1"))
    });
    // Assert:
    assert!(matches!(
        clash,
        Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
            message_key: "ra.validation.unique",
            ..
        }))
    ));
    let waiting = on(&store, |conn| PlayerRepo::new(conn).get("p2")).unwrap();
    assert!(!waiting.has_api_key);
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_A))
            .unwrap()
            .id,
        "p1",
        "the first holder keeps the key"
    );
    cleanup(store, path);
}

/// `updateOwned` (`sql_base_repository.go:506-515`): only the requested columns
/// change, and no caller — not even an admin — can reassign the owner, because
/// `user_id` is dropped from the SET list (`:512`).
#[test]
fn update_changes_requested_columns_and_never_the_owner() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "n", None);
    // Act: an admin asks for the owner column explicitly.
    let moved = record("p1", "u2");
    on(&store, |conn| {
        PlayerRepo::new(conn).update_owned("p1", &moved, &["ip", "userId"], &admin())
    })
    .unwrap();
    // Assert: ownership did not move.
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get("p1"))
            .unwrap()
            .user_id,
        "u1"
    );
    // Act: an empty column list writes every writable column, still no owner
    // (:578-590 in Go's suite).
    let all = PlayerRecord {
        name: "everything".to_string(),
        user_id: "u2".to_string(),
        ..record("p1", "u1")
    };
    on(&store, |conn| {
        PlayerRepo::new(conn).update_owned("p1", &all, &[], &owner("u1"))
    })
    .unwrap();
    // Assert:
    let got = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    assert_eq!(got.name, "everything");
    assert_eq!(got.user_id, "u1");
    // Assert: the key column is untouched by the column write.
    assert!(!got.has_api_key);
    cleanup(store, path);
}

/// A regular user cannot hijack a victim's row by spoofing `userId` in the body:
/// the ownership predicate in the WHERE clause matches nothing, so the write
/// targets nothing and reports permission-denied
/// (`player_repository_test.go:541-560`).
#[test]
fn update_by_a_non_owner_is_denied_and_leaves_the_row_untouched() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "victim", "v", false);
    insert_user(&store, "attacker", "a", false);
    insert_player(
        &store,
        "p1",
        "victim",
        "NavidromeUI",
        "original",
        Some(KEY_A),
    );
    let spoofed = PlayerRecord {
        name: "HIJACKED".to_string(),
        max_bit_rate: 1,
        ..record("p1", "attacker")
    };
    // Act:
    let outcome = on(&store, |conn| {
        PlayerRepo::new(conn).update_owned(
            "p1",
            &spoofed,
            &["name", "user_id", "max_bit_rate"],
            &owner("attacker"),
        )
    });
    // Assert:
    assert!(matches!(outcome, Err(PlayerError::PermissionDenied)));
    let victim_row = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    assert_eq!(victim_row.name, "original");
    assert_eq!(victim_row.user_id, "victim");
    assert!(victim_row.has_api_key, "the victim's key survived");
    // Assert: a nonexistent id is not-found (:616-620 in Go's suite).
    assert!(matches!(
        on(&store, |conn| {
            PlayerRepo::new(conn).update_owned(
                "ghost",
                &record("ghost", "attacker"),
                &["name"],
                &owner("attacker"),
            )
        }),
        Err(PlayerError::NotFound)
    ));
    cleanup(store, path);
}

/// `Update` (`:165-178`) through [`nd_db::player_repo::update`]: with a key both
/// legs run inside one transaction, and a failure in the first leg leaves the
/// second unwritten (`:171-177` plus `inTx`, `:180-188`).
#[test]
fn update_with_a_key_writes_both_legs_or_neither() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "original", Some(KEY_A));
    insert_player(&store, "p2", "u1", "subsonic", "holder", None);
    // Act: p2 tries to take the key p1 already holds.
    let with_key = PlayerRecord {
        name: "renamed".to_string(),
        api_key: Some(KEY_A.to_string()),
        ..record("p2", "u1")
    };
    let outcome = nd_db::player_repo::update(&store, "p2", &with_key, &[], &owner("u1"));
    // Assert: the collision surfaces as the unique validation error…
    assert!(matches!(outcome, Err(PlayerError::ApiKeyValidation(_))));
    // Assert: …and the column write never landed (one transaction).
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get("p2"))
            .unwrap()
            .name,
        "holder"
    );
    // Act: an admin editing another user's player WITHOUT a key changes the
    // columns and leaves the stored hash alone (:481-487 in Go's suite).
    let admin_edit = PlayerRecord {
        name: "admin-renamed".to_string(),
        ..record("p1", "u1")
    };
    nd_db::player_repo::update(&store, "p1", &admin_edit, &["name"], &admin()).unwrap();
    // Assert:
    let p1 = on(&store, |conn| PlayerRepo::new(conn).get("p1")).unwrap();
    assert_eq!(p1.name, "admin-renamed");
    assert!(p1.has_api_key, "the key survived the column write");
    // Act: the success path writes both legs.
    let ok = PlayerRecord {
        name: "renamed".to_string(),
        api_key: Some(KEY_B.to_string()),
        ..record("p2", "u1")
    };
    nd_db::player_repo::update(&store, "p2", &ok, &[], &owner("u1")).unwrap();
    // Assert:
    let got = on(&store, |conn| PlayerRepo::new(conn).get("p2")).unwrap();
    assert_eq!(got.name, "renamed");
    assert!(got.has_api_key);
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).find_by_api_key(KEY_B))
            .unwrap()
            .id,
        "p2"
    );
    // Assert: an admin cannot mint a key for another user through Update either
    // (:492-502): permission-denied, and no column changed.
    let hijack = PlayerRecord {
        name: "Hijacked".to_string(),
        api_key: Some(KEY_B.to_string()),
        ..record("p2", "u1")
    };
    assert!(matches!(
        nd_db::player_repo::update(&store, "p2", &hijack, &["name", "apiKey"], &admin()),
        Err(PlayerError::PermissionDenied)
    ));
    assert_eq!(
        on(&store, |conn| PlayerRepo::new(conn).get("p2"))
            .unwrap()
            .name,
        "renamed"
    );
    cleanup(store, path);
}

/// `Update` with no key takes the single-statement path (`:168-170`) and reports
/// a foreign row as permission-denied (`sql_base_repository.go:554-563`).
#[test]
fn update_without_a_key_denies_a_foreign_row() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u2", "NavidromeUI", "n", None);
    // Act:
    let outcome =
        nd_db::player_repo::update(&store, "p1", &record("p1", "u2"), &["name"], &owner("u1"));
    // Assert:
    assert!(matches!(outcome, Err(PlayerError::PermissionDenied)));
    cleanup(store, path);
}

/// `Delete` (`:190-192`) removes only the caller's own rows, one per statement,
/// stops at the first failure, and classifies a miss by whether the row exists
/// (`player_repository_test.go:128-148`).
#[test]
fn delete_removes_own_rows_only_and_classifies_a_miss() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "mine", None);
    insert_player(&store, "p2", "u2", "subsonic", "theirs", None);
    // Act: u2 tries its own row plus a stranger's; the stranger's id is first,
    // so deleteOwnedAll returns there and neither row is touched (:543-547).
    let denied = on(&store, |conn| {
        PlayerRepo::new(conn).delete(&["p1", "p2"], &owner("u2"))
    });
    // Assert:
    assert!(matches!(denied, Err(PlayerError::PermissionDenied)));
    assert!(on(&store, |conn| PlayerRepo::new(conn).get("p1")).is_ok());
    assert!(on(&store, |conn| PlayerRepo::new(conn).get("p2")).is_ok());
    // Act: u2 deletes its own row.
    on(&store, |conn| {
        PlayerRepo::new(conn).delete(&["p2"], &owner("u2"))
    })
    .unwrap();
    // Assert:
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).get("p2")),
        Err(PlayerError::NotFound)
    ));
    // Act: an admin deletes any row.
    on(&store, |conn| {
        PlayerRepo::new(conn).delete(&["p1"], &admin())
    })
    .unwrap();
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn).get("p1")),
        Err(PlayerError::NotFound)
    ));
    // Act/Assert: an unknown id is not-found.
    assert!(matches!(
        on(&store, |conn| PlayerRepo::new(conn)
            .delete(&["ghost"], &admin())),
        Err(PlayerError::NotFound)
    ));
    cleanup(store, path);
}

/// `ReadAll` (`:113-118`) honours the mapped sort, the page bound, and the
/// `hasapikey` filter; `CountAll` (`:98-100`) counts under the same restriction.
#[test]
fn read_all_pages_sorts_and_filters_and_count_all_agrees() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "a", Some(KEY_A));
    insert_player(&store, "p2", "u1", "subsonic", "b", None);
    insert_player(&store, "p3", "u2", "dsub", "c", Some(KEY_B));
    let options = Options {
        sort: "user_name".to_string(),
        order: "asc".to_string(),
        max: 1,
        offset: 0,
        filters: Some("player.api_key_hash IS NOT NULL".to_string()),
        filter_args: Vec::new(),
    };
    // Act: u1 sees only its own keyed player.
    let page = on(&store, |conn| {
        PlayerRepo::new(conn).read_all(Some(&options), &owner("u1"))
    })
    .unwrap();
    // Assert:
    assert_eq!(
        page.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        vec!["p1"]
    );
    // Act: the count uses the same restriction and filter.
    let count = on(&store, |conn| {
        PlayerRepo::new(conn).count_all(Some(&options), &owner("u1"))
    })
    .unwrap();
    // Assert:
    assert_eq!(count, 1);
    // Act: an admin sees every keyed player.
    let admin_count = on(&store, |conn| {
        PlayerRepo::new(conn).count_all(Some(&options), &admin())
    })
    .unwrap();
    assert_eq!(admin_count, 2);
    // Act: the page bound applies to the admin read too.
    let admin_page = on(&store, |conn| {
        PlayerRepo::new(conn).read_all(Some(&options), &admin())
    })
    .unwrap();
    assert_eq!(admin_page.len(), 1);
    // Assert: an empty result is an empty Vec, not not-found (Go's queryAll,
    // sql_base_repository.go:441-457).
    let none = on(&store, |conn| {
        PlayerRepo::new(conn).read_all(
            Some(&Options {
                filters: Some("player.api_key_hash IS NULL".to_string()),
                ..Options::default()
            }),
            &owner("u2"),
        )
    })
    .unwrap();
    assert!(none.is_empty());
    // Assert: sorting by the mapped username is stable and uses the joined
    // column, so an admin's full page comes back in name order.
    let sorted = on(&store, |conn| {
        PlayerRepo::new(conn).read_all(
            Some(&Options {
                sort: "user_name".to_string(),
                order: "asc".to_string(),
                ..Options::default()
            }),
            &admin(),
        )
    })
    .unwrap();
    assert_eq!(sorted.len(), 3);
    assert!(sorted.iter().all(|p| !p.username.is_empty()));
    cleanup(store, path);
}

/// `CountByClient` (`:77-96`) is unrestricted, groups by client, and the CASE
/// replaces `NavidromeUI` with the player's name — Go's query has no ORDER BY,
/// so the name it picks per group is arbitrary and must not be read as stable.
#[test]
fn count_by_client_groups_unrestricted_and_substitutes_the_ui_name() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    insert_user(&store, "u2", "other", false);
    insert_player(&store, "p1", "u1", "NavidromeUI", "Firefox/Linux", None);
    insert_player(&store, "p2", "u2", "NavidromeUI", "Chrome/Mac", None);
    insert_player(&store, "p3", "u2", "subsonic", "Subsonic", None);
    // Act:
    let counts = on(&store, |conn| PlayerRepo::new(conn).count_by_client(None)).unwrap();
    // Assert: two NavidromeUI rows collapse into one group keyed by a name, and
    // the other client keeps its literal.
    assert_eq!(counts.get("subsonic"), Some(&1));
    assert_eq!(counts.values().sum::<i64>(), 3);
    assert_eq!(
        counts.len(),
        2,
        "the UI group is keyed by a name, not the client"
    );
    assert!(!counts.contains_key("NavidromeUI"));
    // Act: the insights census filters on last_seen with a bound value
    // (core/metrics/insights.go:305-307).
    let recent = on(&store, |conn| {
        PlayerRepo::new(conn).count_by_client(Some(&Options {
            filters: Some("last_seen > ?".to_string()),
            filter_args: vec![Value::from("2023-01-01 00:00:00")],
            ..Options::default()
        }))
    })
    .unwrap();
    // Assert: the same rows (all fixtures are newer than the bound).
    assert_eq!(recent.values().sum::<i64>(), 3);
    // Assert: a bound value in the future excludes everything.
    let stale = on(&store, |conn| {
        PlayerRepo::new(conn).count_by_client(Some(&Options {
            filters: Some("last_seen > ?".to_string()),
            filter_args: vec![Value::from("2099-01-01 00:00:00")],
            ..Options::default()
        }))
    })
    .unwrap();
    assert!(stale.is_empty());
    cleanup(store, path);
}

/// The async boundary: the repository runs through
/// [`SQLStore::with_connection`], so checkout and every statement happen off the
/// executor (`persistence.go:171-185`).
#[tokio::test(flavor = "current_thread")]
async fn the_repository_runs_through_the_async_store_boundary() {
    // Arrange:
    let (store, path) = store();
    insert_user(&store, "u1", "jdoe", false);
    let new_player = PlayerRecord {
        id: String::new(),
        user_id: String::new(),
        api_key: Some(KEY_A.to_string()),
        ..record("", "u1")
    };
    // Act:
    let id = store
        .with_connection(move |conn| Ok(PlayerRepo::new(conn).save(&new_player, "u1")))
        .await
        .unwrap()
        .unwrap();
    let got = store
        .with_connection(move |conn| Ok(PlayerRepo::new(conn).find_by_api_key(KEY_A)))
        .await
        .unwrap()
        .unwrap();
    // Assert:
    assert_eq!(got.id, id);
    assert!(got.has_api_key);
    cleanup(store, path);
}
