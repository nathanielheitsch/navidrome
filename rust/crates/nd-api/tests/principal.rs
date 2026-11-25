//! API-01a integration: the non-HTTP native API-key principal and current
//! library access, executed against a REAL migrated SQLite database.
//!
//! Truth under test (docs/rust-port/subplans/boot-api.md §API-01,
//! post-api-00-queue.md dispatch 1): a key authenticates through the REAL indexed
//! player lookup (`PlayerRepo::find_by_api_key`) and the CURRENT owner row
//! (`UserRepo::get`), and library grants are re-read per operation from the
//! `user_library` table.
//!
//! Keys are generated (`nds_` + `nd_id::new_random()`) and issued through the
//! production `PlayerRepo::save`; tables this lane does not own (`library`,
//! `user`, `media_file`) use raw fixture inserts — the same harness shape
//! `nd-db/tests/runtime_*` uses. No hand-fabricated success data.

use nd_api::principal::{
    current_library_access, resolve_api_principal, ApiKeyCredential, ApiPrincipal, CredentialKind,
    LibraryAccess, PrincipalError,
};
use nd_db::player_repo::{
    hash_api_key, ApiKeyValidationError, LoggedUser, PlayerError, PlayerRecord, PlayerRepo,
};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::scanner_queries::{get_visible_media, VisibleMediaError};
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

const STAMP: &str = "2024-01-01 00:00:00+00:00";

/// A generated, canonical API key (`nds_` + `nd_id::new_random()`'s 22 base62).
fn generated_key() -> String {
    format!("nds_{}", nd_id::new_random())
}

/// One isolated on-disk DB per invocation, freshly migrated and pooled — the
/// `nd-db/tests/runtime_player_repo.rs` harness. `_foreign_keys=on` is Go's
/// `db/db.go:55` DSN parameter, so `player.user_id`'s CASCADE really runs (used
/// by the owner-deletion case).
fn store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-api-principal-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear stale {path:?}: {e}"));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(&first).unwrap_or_else(|cause| panic!("fresh schema: {cause}"));
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Run a fixture statement on a leased connection.
fn run(store: &SQLStore, sql: &str, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<()>) {
    store
        .with_connection_sync(|conn| {
            f(conn)?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("fixture on {sql}: {e}"));
}

/// Read one scalar from a fixture query. `T: FromSql` covers the `String`/`i64`
/// scalars the assertions read.
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    store
        .with_connection_sync(|conn| conn.query_row(sql, [], |row| row.get::<_, T>(0)))
        .unwrap_or_else(|e| panic!("scalar on {sql}: {e}"))
}

/// A user row written straight to SQL (user creation belongs to `user_repo.rs`,
/// outside this lane). `password` is a placeholder, never a real hash.
fn insert_user(store: &SQLStore, id: &str, is_admin: bool) {
    let id = id.to_string();
    run(store, "INSERT INTO user", move |conn| {
        conn.execute(
            "INSERT INTO user(id, user_name, name, email, password, is_admin, created_at, updated_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                id,
                id,
                id,
                format!("{id}@example.com"),
                "CIPHERTEXT-not-a-real-hash",
                is_admin,
                STAMP,
                STAMP,
            ],
        )?;
        Ok(())
    });
}

/// A `library` row, optionally granted to `user_id` via `user_library`.
fn insert_library(store: &SQLStore, id: i64, name: &str, granted_to: Option<&str>) {
    let name = name.to_string();
    let grant = granted_to.map(str::to_string);
    run(store, "INSERT INTO library", move |conn| {
        conn.execute(
            "INSERT INTO library(id, name, path) VALUES(?1,?2,?3)",
            rusqlite::params![id, name, format!("/music/{name}")],
        )?;
        if let Some(user_id) = grant {
            conn.execute(
                "INSERT INTO user_library(user_id, library_id) VALUES(?1,?2)",
                rusqlite::params![user_id, id],
            )?;
        }
        Ok(())
    });
}

/// A `media_file` row in `library_id`, hand-INSERTed on the columns the scoped
/// read needs (media creation belongs to the scanner lane, not this one).
fn insert_media(store: &SQLStore, id: &str, library_id: i64) {
    let id = id.to_string();
    run(store, "INSERT INTO media_file", move |conn| {
        conn.execute(
            "INSERT INTO media_file(id, library_id, path, title, created_at, updated_at, birth_time, tags) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,'{}')",
            rusqlite::params![
                id,
                library_id,
                format!("/lib{library_id}/{id}"),
                id,
                STAMP,
                STAMP,
                STAMP,
            ],
        )?;
        Ok(())
    });
}

/// Create a keyed player through the PRODUCTION `PlayerRepo::save`, so the key
/// hash and the generated id land exactly as the issuance path writes them. The
/// owner is Go's `loggedUser(ctx).ID`, stamped by `save` itself.
fn save_player(store: &SQLStore, owner: &str, key: &str, name: &str) -> String {
    let owner = owner.to_string();
    let name = name.to_string();
    store
        .with_connection_sync(move |conn| {
            let record = PlayerRecord {
                name,
                client: "test-client".to_string(),
                api_key: Some(key.to_string()),
                ..PlayerRecord::default()
            };
            PlayerRepo::new(conn)
                .save(&record, &owner)
                .map_err(|e| denial_as_sqlite(e.to_string()))
        })
        .unwrap_or_else(|e| panic!("PlayerRepo::save: {e}"))
}

/// Revoke a key through the PRODUCTION `PlayerRepo::set_api_key` (empty key ⇒
/// hash NULL), as the owner.
fn revoke_key(store: &SQLStore, player_id: &str, owner: &str) {
    let player_id = player_id.to_string();
    let owner = owner.to_string();
    store
        .with_connection_sync(move |conn| {
            let logged = LoggedUser {
                id: owner,
                is_admin: false,
            };
            PlayerRepo::new(conn)
                .set_api_key(&player_id, "", &logged)
                .map_err(|e| denial_as_sqlite(e.to_string()))
        })
        .unwrap_or_else(|e| panic!("revoke set_api_key: {e}"));
}

/// Fold a repository error into the SQLite type the store callback requires, so
/// a fixture failure surfaces as a panic rather than being silently retried.
fn denial_as_sqlite(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(message))
}

/// The scoped read the API lane will run, on a leased connection. `Ok(true)`
/// means the media is visible to `user_id`, `Ok(false)` means denied.
fn visible(store: &SQLStore, id: &str, user_id: &str) -> bool {
    let id = id.to_string();
    let user_id = user_id.to_string();
    store
        .with_connection_sync(move |conn| {
            Ok(match get_visible_media(conn, &id, &user_id, false) {
                Ok(_) => true,
                Err(VisibleMediaError::NotFound) => false,
                Err(err) => return Err(denial_as_sqlite(format!("scoped read failed: {err}"))),
            })
        })
        .unwrap_or_else(|e| panic!("scoped read: {e}"))
}

/// Resolve over the async entrypoint. A current-thread runtime is enough: the
/// point of the test is that the resolver moves its SQLite work off the
/// executor, and the assertion needs the result.
async fn resolve(store: &SQLStore, key: &str) -> Result<ApiPrincipal, PrincipalError> {
    resolve_api_principal(store, &ApiKeyCredential::new(key)).await
}

async fn access(
    store: &SQLStore,
    principal: &ApiPrincipal,
) -> Result<LibraryAccess, PrincipalError> {
    current_library_access(store, principal).await
}

#[tokio::test]
async fn generated_key_resolves_to_owner_and_player() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    insert_library(&store, 1, "Main", Some("alice"));
    let key = generated_key();
    let player_id = save_player(&store, "alice", &key, "player-a");

    let principal = resolve(&store, &key).await.expect("generated key resolves");
    assert_eq!(
        principal.owner_id(),
        "alice",
        "principal names the key's owner"
    );
    assert_eq!(principal.owner_name(), "alice");
    assert_eq!(
        principal.player_id(),
        player_id,
        "principal carries the player id"
    );
    assert_eq!(principal.kind(), CredentialKind::ApiKey);
    assert!(!principal.is_admin(), "a regular owner is not admin");

    // The DB really stored the indexed hash the lookup binds.
    let stored_hash: String = scalar(
        &store,
        &format!("SELECT api_key_hash FROM player WHERE id = '{player_id}'"),
    );
    assert_eq!(
        stored_hash,
        hash_api_key(&key),
        "the lookup column is the SHA-256 hash the resolver binds"
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn revoked_key_is_denied_as_credential_not_found() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    let key = generated_key();
    let player_id = save_player(&store, "alice", &key, "player-a");

    assert!(
        resolve(&store, &key).await.is_ok(),
        "key authenticates while issued"
    );

    revoke_key(&store, &player_id, "alice");

    let err = resolve(&store, &key)
        .await
        .expect_err("revoked key must be denied");
    assert_eq!(
        err,
        PrincipalError::CredentialNotFound,
        "revocation ⇒ lookup miss"
    );
    assert!(
        err.is_denial(),
        "revocation is a denial, not an operational failure"
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn malformed_keys_are_denied_without_touching_the_database() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    // A well-formed key that exists in the DB, so a shape bug cannot be
    // mistaken for a lookup miss.
    save_player(&store, "alice", &generated_key(), "player-a");

    let truncated = format!("nds_{}", &nd_id::new_random()[..21]);
    let cases = [
        String::new(),
        format!("ndt_{}", nd_id::new_random()),
        truncated,
        format!("nds_{}x", nd_id::new_random()),
        "nds_!!!!!!!!!!!!!!!!!!!!!!".to_string(),
    ];
    for bad in cases {
        assert!(
            !ApiKeyCredential::new(&bad).is_well_formed(),
            "{bad:?} is not canonical"
        );
        let err = resolve(&store, &bad)
            .await
            .expect_err("malformed key must be denied");
        assert_eq!(err, PrincipalError::MalformedKey, "{bad:?} ⇒ MalformedKey");
        // The denial never echoes the presented bytes. (An empty key is vacuous
        // — `contains("")` is always true — so only non-empty keys are checked.)
        if !bad.is_empty() {
            assert!(
                !err.to_string().contains(&bad) && !format!("{err:?}").contains(&bad),
                "the denial never echoes the key"
            );
        }
    }

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn deleted_owner_denies_the_key() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    insert_user(&store, "bob", false);
    let key = generated_key();
    save_player(&store, "alice", &key, "player-a");
    assert!(resolve(&store, &key).await.is_ok());

    // Deleting the owner cascades to the player row (foreign_keys=on), so a key
    // cannot survive owner deletion.
    run(&store, "DELETE FROM user", |conn| {
        conn.execute("DELETE FROM user WHERE id = 'alice'", [])?;
        Ok(())
    });

    let err = resolve(&store, &key)
        .await
        .expect_err("a key cannot outlive its owner");
    assert!(
        err.is_denial(),
        "owner loss is a denial, not an operational failure"
    );

    // Non-vacuity: the unrelated user survives and its own absent key is a
    // lookup miss — the table still works, so the denial is specific.
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM user"),
        1,
        "bob remains, so the denial is specific to the deleted owner"
    );
    assert_eq!(
        resolve(&store, &generated_key())
            .await
            .expect_err("no key exists for bob"),
        PrincipalError::CredentialNotFound
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn two_keys_distinct_owners_resolve_to_their_own_principal() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    insert_user(&store, "bob", true);
    let key_a = generated_key();
    let key_b = generated_key();
    let player_a = save_player(&store, "alice", &key_a, "pa");
    let player_b = save_player(&store, "bob", &key_b, "pb");

    let pa = resolve(&store, &key_a).await.expect("key a");
    let pb = resolve(&store, &key_b).await.expect("key b");
    assert_eq!(pa.owner_id(), "alice");
    assert_eq!(pb.owner_id(), "bob");
    assert!(pb.is_admin(), "admin flag follows each key's own owner");
    assert!(!pa.is_admin());
    assert_eq!(pa.player_id(), player_a);
    assert_eq!(pb.player_id(), player_b);
    assert_ne!(
        pa.owner_id(),
        pb.owner_id(),
        "a key never selects another user"
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn zero_grants_is_not_all_grants_and_denies_next_lookup() {
    let (store, path) = store();
    insert_user(&store, "carol", false);
    // Two libraries exist, but carol is granted NEITHER.
    insert_library(&store, 1, "Main", None);
    insert_library(&store, 2, "Other", None);
    let key = generated_key();
    save_player(&store, "carol", &key, "pc");

    let principal = resolve(&store, &key).await.expect("carol principal");
    let grants = access(&store, &principal).await.expect("carol access");
    assert_eq!(
        grants.granted_library_ids,
        Vec::<i64>::new(),
        "zero grants, not all libraries"
    );
    assert!(
        grants.denies_all(),
        "zero grants means deny, never unrestricted"
    );

    insert_media(&store, "m1", 1);
    insert_media(&store, "m2", 2);

    assert!(
        !visible(&store, "m1", "carol") && !visible(&store, "m2", "carol"),
        "a zero-grant user sees nothing (deny, never all)"
    );
    // Non-vacuity: the rows exist and a scoped reader CAN see them (an admin's
    // scope omits the clause), so carol's denial above is the scope, not an
    // absent table or a broken read.
    assert_eq!(scalar::<i64>(&store, "SELECT count(*) FROM media_file"), 2);
    insert_user(&store, "root", true);
    assert!(visible(&store, "m1", "root"), "m1 exists and is readable");
    assert!(visible(&store, "m2", "root"), "m2 exists and is readable");

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn admin_owner_sees_all_libraries_without_an_explicit_grant() {
    let (store, path) = store();
    insert_user(&store, "root", true);
    insert_library(&store, 1, "Main", None);
    insert_library(&store, 2, "Other", None);
    let key = generated_key();
    save_player(&store, "root", &key, "padmin");

    let principal = resolve(&store, &key).await.expect("admin principal");
    assert!(
        principal.is_admin(),
        "admin flag from the owner's current row"
    );

    insert_media(&store, "m9", 2);
    // Admin scope omits the clause: media in a library never explicitly granted
    // to the admin is visible.
    assert!(visible(&store, "m9", "root"), "admin sees all libraries");

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn removing_one_grant_denies_only_that_library_on_the_next_operation() {
    let (store, path) = store();
    insert_user(&store, "carol", false);
    insert_library(&store, 1, "Main", Some("carol"));
    insert_library(&store, 2, "Other", Some("carol"));
    let key = generated_key();
    save_player(&store, "carol", &key, "pc");
    insert_media(&store, "m1", 1);
    insert_media(&store, "m2", 2);

    let principal = resolve(&store, &key).await.expect("carol");
    let both = access(&store, &principal).await.expect("initial grants");
    assert_eq!(
        both.granted_library_ids,
        vec![1, 2],
        "grants read from the current DB"
    );
    assert!(visible(&store, "m1", "carol") && visible(&store, "m2", "carol"));

    // Revoke the library-2 grant.
    run(&store, "DELETE FROM user_library", |conn| {
        conn.execute(
            "DELETE FROM user_library WHERE user_id = 'carol' AND library_id = 2",
            [],
        )?;
        Ok(())
    });

    // Grants are NOT cached in the principal: the next read reflects the removal.
    let after = access(&store, &principal)
        .await
        .expect("post-revocation grants");
    assert_eq!(
        after.granted_library_ids,
        vec![1],
        "only the remaining grant"
    );

    // The removed library is denied on the next scoped lookup; the authorized
    // one is still visible.
    assert!(
        !visible(&store, "m2", "carol"),
        "library-2 media denied after removal"
    );
    assert!(
        visible(&store, "m1", "carol"),
        "the remaining grant still reads"
    );

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn principal_and_errors_carry_no_credential_material() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    let key = generated_key();
    save_player(&store, "alice", &key, "player-a");

    let credential = ApiKeyCredential::new(&key);
    assert!(
        !format!("{credential:?}").contains(&key),
        "Debug never leaks the key"
    );
    assert!(
        !credential.to_string().contains(&key),
        "Display never leaks the key"
    );

    let principal = resolve(&store, &key).await.expect("principal");
    let rendered = format!("{principal:?}");
    assert!(!rendered.contains(&key), "principal Debug has no key");
    assert!(
        !rendered.contains("api_key"),
        "no key field on the principal"
    );
    assert!(
        !rendered.contains("password"),
        "no password on the principal"
    );
    assert!(!rendered.contains("hash"), "no hash on the principal");

    // A malformed denial never echoes the presented key.
    let malformed = format!("nds_{}!!", nd_id::new_random());
    let err = resolve(&store, &malformed).await.expect_err("malformed");
    assert!(
        !format!("{err:?}").contains(&malformed) && !err.to_string().contains(&malformed),
        "error text is credential-free"
    );
    assert_eq!(err, PrincipalError::MalformedKey);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn operational_failure_is_typed_not_a_denial() {
    // A store whose pool has NO live connection: the only Initializer is dropped
    // without returning it, so checkout reports depletion — an operational
    // failure that must NOT be flattened to a credential denial (boot-api.md:
    // key db failure is 500, not 404/401).
    let (pool, first) = Pool::new(&PoolConfig::with_size(":memory:", 1)).unwrap();
    drop(first);
    let store = SQLStore::new(pool);

    let err = resolve(&store, &generated_key())
        .await
        .expect_err("no connection ⇒ operational");
    assert_eq!(err, PrincipalError::Operational);
    assert!(
        !err.is_denial(),
        "operational failure is never treated as a credential denial"
    );

    // Ordering non-vacuity on the SAME broken store: a malformed key is denied
    // as `MalformedKey` (the format check runs before any DB access), NOT as
    // `Operational`. A resolver that reached the DB first would report
    // Operational here.
    let malformed = resolve(&store, "nds_short")
        .await
        .expect_err("malformed is denied first");
    assert_eq!(malformed, PrincipalError::MalformedKey);
    assert!(malformed.is_denial());
}

#[tokio::test]
async fn duplicate_key_is_rejected_by_the_unique_index_not_silently_shared() {
    let (store, path) = store();
    insert_user(&store, "alice", false);
    insert_user(&store, "bob", false);
    let key = generated_key();
    save_player(&store, "alice", &key, "pa");

    // Issuing the same key for another player collides on the unique hash index.
    let key_for_bob = key.clone();
    let collision = store
        .with_connection_sync(|conn| {
            let record = PlayerRecord {
                name: "pb".to_string(),
                client: "test-client".to_string(),
                api_key: Some(key_for_bob.clone()),
                ..PlayerRecord::default()
            };
            Ok::<_, rusqlite::Error>(PlayerRepo::new(conn).save(&record, "bob").err().map(|err| {
                matches!(
                    err,
                    PlayerError::ApiKeyValidation(ApiKeyValidationError {
                        message_key: "ra.validation.unique",
                        ..
                    })
                )
            }))
        })
        .expect("save runs");
    assert_eq!(
        collision,
        Some(true),
        "the unique index rejects a shared key: {key_for_bob}"
    );

    // The original key still resolves to its real owner only.
    assert_eq!(
        resolve(&store, &key).await.expect("still valid").owner_id(),
        "alice"
    );

    std::fs::remove_file(&path).ok();
}
