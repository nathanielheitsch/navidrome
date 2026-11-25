//! Executed login and first-admin HTTP tests against a fresh schema.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;
use nd_auth::credentials::{Authenticator, SigningSecrets};
use nd_auth::login::{create_admin, login, login_from_headers, HeaderLoginState, LoginState};
use nd_auth::middleware::{authenticate, AuthGate, AuthenticatedUser, MiddlewareState, ProxyPeer};
use nd_auth::refresh::refresh;
use nd_auth::setup_store::{run_initial_setup, setup_in_tx, DevAdmin, SetupStoreError};
use nd_auth::verifier::ResolutionConfig;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::property_repo::{get_sql, put_sql};
use nd_db::store::SQLStore;
use nd_db::user_repo::UserRepo;
use nd_dbschema::schema::apply_fresh_schema;
use serde::Deserialize;
use tower::ServiceExt;

#[derive(Deserialize)]
struct TokenPayload {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeaderPayload {
    id: String,
    username: String,
    is_admin: bool,
}

fn setup() -> (SQLStore, std::path::PathBuf, Authenticator) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-login-", ".db"));
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 4)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    let auth = Authenticator::new(
        SigningSecrets {
            current: b"test signing secret".to_vec(),
            previous: None,
            public_current: b"other secret".to_vec(),
        },
        3600,
    );
    (SQLStore::new(pool), path, auth)
}

fn request(path: &str, json: &'static str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .body(Body::from(json))
        .unwrap()
}

fn routes(state: LoginState) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/create", post(create_admin))
        .with_state(state)
}

#[tokio::test]
async fn first_admin_login_and_wrong_password_use_stored_hash() {
    let (store, path, auth) = setup();
    let state = LoginState::new(
        store.clone(),
        auth.clone(),
        20,
        Duration::from_secs(60),
        false,
    )
    .unwrap();
    let app = routes(state);
    let response = app
        .clone()
        .oneshot(request(
            "/create",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(!text.contains("subsonicSalt"));
    assert!(!text.contains("subsonicToken"));
    let axum::Json(payload) = axum::Json::<TokenPayload>::from_bytes(&body).unwrap();
    let user = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
        .unwrap()
        .unwrap();
    assert!(nd_auth::credentials::password::verify_password("secret", &user.password).unwrap());
    let protected = Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(from_fn_with_state(
            MiddlewareState {
                store: store.clone(),
                auth,
                gate: AuthGate::Session,
                resolution: ResolutionConfig::default(),
            },
            authenticate,
        ));
    let protected_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {}", payload.token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        protected
            .clone()
            .oneshot(protected_request)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let wrong = app
        .clone()
        .oneshot(request(
            "/login",
            r#"{"username":"alice","password":"wrong"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(!wrong.headers().contains_key("x-nd-authorization"));
    let wrong_body = to_bytes(wrong.into_body(), 8192).await.unwrap();
    assert!(!std::str::from_utf8(&wrong_body).unwrap().contains("token"));
    let login_response = app
        .clone()
        .oneshot(request(
            "/login",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(login_response.status(), StatusCode::OK);
    let login_body = to_bytes(login_response.into_body(), 8192).await.unwrap();
    let axum::Json(login_payload) = axum::Json::<TokenPayload>::from_bytes(&login_body).unwrap();
    let login_request = Request::builder()
        .uri("/")
        .header(
            "x-nd-authorization",
            format!("Bearer {}", login_payload.token),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        protected.oneshot(login_request).await.unwrap().status(),
        StatusCode::OK
    );
    let another = app
        .oneshot(request("/create", r#"{"username":"bob","password":"pw"}"#))
        .await
        .unwrap();
    assert_eq!(another.status(), StatusCode::FORBIDDEN);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn body_cap_and_invalid_limiter_fail_closed() {
    let (store, path, auth) = setup();
    assert!(LoginState::new(
        store.clone(),
        auth.clone(),
        0,
        Duration::from_secs(60),
        false
    )
    .is_err());
    assert!(LoginState::new(
        store.clone(),
        auth.clone(),
        -1,
        Duration::from_secs(60),
        false
    )
    .is_err());
    let app = routes(
        LoginState::new(
            store.clone(),
            auth.clone(),
            1,
            Duration::from_secs(60),
            false,
        )
        .unwrap(),
    );
    let oversized = Request::builder()
        .method("POST")
        .uri("/login")
        .body(Body::from(vec![b'x'; 8193]))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let app =
        routes(LoginState::new(store.clone(), auth, 1, Duration::from_secs(60), false).unwrap());
    let wrong = || request("/login", r#"{"username":"nobody","password":"wrong"}"#);
    assert_eq!(
        app.clone().oneshot(wrong()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let blocked = app
        .oneshot(request(
            "/login",
            r#"{"username":"nobody","password":"wrong"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    let count = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
        .unwrap();
    assert_eq!(count, 0);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn concurrent_admin_requests_use_production_retry_schedule() {
    // Contention may invoke the production 5s, 10s, and 15s retry delays.
    let (store, path, auth) = setup();
    let app =
        routes(LoginState::new(store.clone(), auth, 20, Duration::from_secs(60), false).unwrap());
    let one = app.clone().oneshot(request(
        "/create",
        r#"{"username":"alice","password":"first"}"#,
    ));
    let two = app.oneshot(request(
        "/create",
        r#"{"username":"bob","password":"second"}"#,
    ));
    let (one, two) = tokio::join!(one, two);
    let mut statuses = [one.unwrap().status(), two.unwrap().status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::FORBIDDEN]);
    let (count, admin) = store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            Ok((repo.count_all().unwrap(), repo.find_first_admin().unwrap()))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(admin.is_admin);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn trusted_header_provisions_first_admin_then_regular_user() {
    let (store, path, auth) = setup();
    let state = HeaderLoginState {
        login: LoginState::new(store.clone(), auth, 20, Duration::from_secs(60), false).unwrap(),
        resolution: ResolutionConfig {
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8".into(),
            ..ResolutionConfig::default()
        },
    };
    let app = Router::new()
        .route("/", get(login_from_headers))
        .with_state(state);
    for (name, peer, expected) in [
        ("ignored", "11.0.0.1", StatusCode::UNAUTHORIZED),
        ("alice", "10.0.0.1", StatusCode::OK),
        ("bob", "10.0.0.1", StatusCode::OK),
    ] {
        let mut req = Request::builder()
            .uri("/")
            .header("x-user", name)
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ProxyPeer(peer.into()));
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), expected);
    }
    let (alice, bob, count) = store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            Ok((
                repo.find_by_username("alice").unwrap(),
                repo.find_by_username("bob").unwrap(),
                repo.count_all().unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(count, 2);
    assert!(alice.is_admin);
    assert!(!bob.is_admin);
    assert!(alice.password.starts_with("$argon2id$"));
    assert!(alice.last_login_at.is_some());
    drop(app);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn concurrent_trusted_header_duplicate_username_returns_same_user() {
    let (store, path, auth) = setup();
    let state = HeaderLoginState {
        login: LoginState::new(store.clone(), auth, 20, Duration::from_secs(60), false).unwrap(),
        resolution: ResolutionConfig {
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8".into(),
            ..ResolutionConfig::default()
        },
    };
    let app = Router::new()
        .route("/", get(login_from_headers))
        .with_state(state);
    let req = || {
        let mut req = Request::builder()
            .uri("/")
            .header("x-user", "alice")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ProxyPeer("10.0.0.1".into()));
        req
    };
    let (one, two) = tokio::join!(app.clone().oneshot(req()), app.oneshot(req()));
    let mut ids = Vec::new();
    for response in [one.unwrap(), two.unwrap()] {
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let axum::Json(body) = axum::Json::<HeaderPayload>::from_bytes(&body).unwrap();
        assert_eq!(body.username, "alice");
        assert!(body.is_admin);
        ids.push(body.id);
    }
    assert_eq!(ids[0], ids[1]);
    assert!(!ids[0].is_empty());
    assert_eq!(
        store
            .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
            .unwrap(),
        1
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn initial_setup_persists_hashed_dev_admin_once() {
    let (store, path, _) = setup();
    assert!(
        run_initial_setup(&store, "dev-password", "2024-01-01T00:00:00Z")
            .await
            .unwrap()
    );
    assert!(
        !run_initial_setup(&store, "changed", "2024-01-02T00:00:00Z")
            .await
            .unwrap()
    );
    let (count, admin) = store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            Ok((repo.count_all().unwrap(), repo.find_first_admin().unwrap()))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(admin.user_name, "admin");
    assert!(
        nd_auth::credentials::password::verify_password("dev-password", &admin.password).unwrap()
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn initial_setup_creates_dev_admin_with_existing_regular_user() {
    let (store, path, _) = setup();
    store
        .with_connection_sync(|conn| {
            let stamp = "2024-01-01T00:00:00Z";
            let values = nd_db::user_repo::UserPutValues {
                id: "regular",
                user_name: "regular",
                name: "Regular",
                email: "",
                password: None,
                is_admin: false,
                last_login_at: None,
                last_access_at: None,
                created_at: stamp,
                updated_at: stamp,
                scrobble_filter: "",
            };
            UserRepo::new(conn).put(&values, stamp, false).unwrap();
            Ok(())
        })
        .unwrap();
    assert!(
        run_initial_setup(&store, "dev-password", "2024-01-01T00:00:00Z")
            .await
            .unwrap()
    );
    assert!(
        !run_initial_setup(&store, "dev-password", "2024-01-01T00:00:00Z")
            .await
            .unwrap()
    );
    store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            assert_eq!(repo.count_all().unwrap(), 2);
            assert!(repo.find_by_username("admin").unwrap().is_admin);
            Ok(())
        })
        .unwrap();
    drop(store);
    let _ = std::fs::remove_file(path);
}
#[tokio::test]
async fn same_username_admin_race_is_settled_by_the_unique_constraint() {
    // What actually settles two concurrent creations of the SAME username is the
    // `user.user_name` UNIQUE constraint (`nd-db/tests/fixtures/go_schema_dump.txt:288-290`),
    // which is CASE-SENSITIVE: the NOCASE collation belongs only to the separate
    // `user_username_password` index (:302-303). Go has neither a transaction nor a
    // constraint-backed check (`server/auth.go:122-160` counts, then Puts), and its createAdmin
    // failure path returns 500 (`server/auth.go:137-140`).
    let (store, path, auth) = setup();
    let app =
        routes(LoginState::new(store.clone(), auth, 20, Duration::from_secs(60), false).unwrap());
    let one = app.clone().oneshot(request(
        "/create",
        r#"{"username":"same","password":"first"}"#,
    ));
    let two = app.oneshot(request(
        "/create",
        r#"{"username":"same","password":"second"}"#,
    ));
    let (one, two) = tokio::join!(one, two);
    let mut statuses: Vec<StatusCode> = vec![one.unwrap().status(), two.unwrap().status()];
    statuses.sort_by_key(|status| status.as_u16());
    // Exactly one succeeds. The loser is refused by the count guard (403) or loses the race and
    // surfaces the constraint error as 500 — matching Go. Never two admins, never a panic.
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}: exactly one creation may succeed"
    );
    assert!(
        statuses
            .iter()
            .any(|s| *s == StatusCode::FORBIDDEN || *s == StatusCode::INTERNAL_SERVER_ERROR),
        "{statuses:?}: the loser must be refused or reported, not silently accepted"
    );
    let count = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
        .unwrap();
    assert_eq!(count, 1, "the race must settle on exactly one user row");
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn setup_transaction_rolls_back_user_and_marker_together() {
    // The atomicity layer: the marker write and the dev-admin insert (`server/initial_setup.go:21-42`)
    // ride ONE retryable transaction, so a failure after the insert commits neither half. This
    // replays the same two writes through the same repos inside `with_tx` and forces a failure
    // afterwards. It proves the store rolls both back atomically; that `run_initial_setup` wraps
    // them in one `with_tx_retry` closure is visible at `nd-auth/src/setup_store.rs:55-97`.
    let (store, path, _) = setup();
    let error = store
        .with_tx(|tx| -> Result<(), rusqlite::Error> {
            let stamp = "2024-01-01T00:00:00Z";
            UserRepo::new(tx)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "",
                        user_name: "admin",
                        name: nd_auth::initial_setup::DEV_INITIAL_NAME,
                        email: "",
                        password: Some("not-a-real-hash"),
                        is_admin: true,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            let statement = nd_db::property_repo::put_sql(
                nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY,
                stamp,
            );
            if nd_db::execution::execute(tx, &statement.update).unwrap() == 0 {
                nd_db::execution::execute(tx, &statement.insert).unwrap();
            }
            // Forced failure AFTER both writes.
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
                Some("forced mid-transaction failure".to_owned()),
            ))
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("forced mid-transaction failure"));
    let (users, marker) = store
        .with_connection_sync(|conn| {
            Ok((
                UserRepo::new(conn).count_all().unwrap(),
                nd_db::execution::query_optional(
                    conn,
                    &nd_db::property_repo::get_sql(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(users, 0, "rolled-back insert must leave no user row");
    assert_eq!(
        marker, None,
        "rolled-back marker write must leave no setup flag"
    );
    assert!(
        run_initial_setup(&store, "dev-password", "2024-01-02T00:00:00Z")
            .await
            .unwrap(),
        "a rolled-back attempt must not have consumed the one-shot setup flag"
    );
    drop(store);
    let _ = std::fs::remove_file(path);

    // Positive control, proving the transaction is what supplies the rollback: the SAME two writes
    // issued as bare statements (no transaction) survive a later failure. If the atomic assertion
    // above were weakened to accept partial state, this control would still pass and the test would
    // stop distinguishing the two paths — so the contrast is the load-bearing part.
    let (split, split_path, _) = setup();
    let stamp = "2024-01-01T00:00:00Z";
    let failure = split
        .with_connection_sync(|conn| -> Result<(), rusqlite::Error> {
            UserRepo::new(conn)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "",
                        user_name: "admin",
                        name: nd_auth::initial_setup::DEV_INITIAL_NAME,
                        email: "",
                        password: Some("not-a-real-hash"),
                        is_admin: true,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            let statement = nd_db::property_repo::put_sql(
                nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY,
                stamp,
            );
            if nd_db::execution::execute(conn, &statement.update).unwrap() == 0 {
                nd_db::execution::execute(conn, &statement.insert).unwrap();
            }
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
                Some("forced failure after untransacted writes".to_owned()),
            ))
        })
        .unwrap_err();
    assert!(failure
        .to_string()
        .contains("forced failure after untransacted writes"));
    let (split_users, split_marker) = split
        .with_connection_sync(|conn| {
            Ok((
                UserRepo::new(conn).count_all().unwrap(),
                nd_db::execution::query_optional(
                    conn,
                    &nd_db::property_repo::get_sql(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(split_users, 1, "without a transaction the insert persists");
    assert_eq!(
        split_marker.as_deref(),
        Some(stamp),
        "without a transaction the marker persists — this is the partial state the transaction prevents"
    );
    drop(split);
    let _ = std::fs::remove_file(split_path);
}

#[tokio::test]
async fn dev_admin_coexists_with_a_differently_cased_username() {
    // Inherited upstream property, identical in Go: uniqueness on `user.user_name` is
    // CASE-SENSITIVE (`go_schema_dump.txt:288-290`) while lookups go through the NOCASE index
    // (:302-303), so an existing `Admin` does not satisfy `count_by_username("admin")` and the dev
    // admin is still created. Pinned so a future COLLATE change is a decision, not silent drift.
    let (store, path, _) = setup();
    store
        .with_connection_sync(|conn| {
            let stamp = "2024-01-01T00:00:00Z";
            UserRepo::new(conn)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "existing",
                        user_name: "Admin",
                        name: "Existing",
                        email: "",
                        password: None,
                        is_admin: false,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    assert!(
        run_initial_setup(&store, "dev-password", "2024-01-02T00:00:00Z")
            .await
            .unwrap()
    );
    let count = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
        .unwrap();
    assert_eq!(count, 2, "case-sensitive UNIQUE lets both rows exist");
    drop(store);
    let _ = std::fs::remove_file(path);
}

// ---- BOOT-02: the token login actually returns must be native + expiring ----
//
// These run the real `POST /login` / `POST /create` handlers against a
// fresh-schema SQLite store and then send the UNMODIFIED returned token to a
// strict `AuthGate::Api` router (`ResolutionConfig::default()`: no dev
// username, no trusted header), so a 200 can only come from the mint itself.

/// Strict API fixture for a login-issued token.
fn api_routes(store: SQLStore, auth: Authenticator) -> Router {
    Router::new()
        .route(
            "/",
            get(
                |axum::Extension(user): axum::Extension<AuthenticatedUser>| async move {
                    assert_eq!(user.username, "alice");
                    assert!(user.is_admin);
                    "ok"
                },
            ),
        )
        .layer(from_fn_with_state(
            MiddlewareState {
                store,
                auth,
                gate: AuthGate::Api,
                resolution: ResolutionConfig::default(),
            },
            authenticate,
        ))
}

/// Assert the decoded claims of a login-issued token against the DB row, and
/// return them. `adm=false`/`ep=0` are legitimately omitted by serde, so this
/// compares decoded semantics, not byte presence.
fn assert_native_session_claims(
    auth: &Authenticator,
    token: &str,
    user: &nd_db::user_repo::UserRecord,
    mint_window: (i64, i64),
) -> nd_auth::claims::Claims {
    let claims = auth.validate(token).unwrap_or_else(|error| {
        panic!("the login token must verify under its own signing secret: {error:?}")
    });
    assert_eq!(
        claims.aud,
        vec![nd_auth::claims::AUDIENCE_NATIVE.to_string()],
        "a login token must carry aud=[native]"
    );
    assert_eq!(claims.iss, nd_auth::claims::JWT_ISSUER);
    assert_eq!(claims.sub, user.user_name);
    assert_eq!(claims.user_id, user.id, "uid must name the executing row");
    assert_eq!(claims.is_admin, user.is_admin);
    assert_eq!(
        claims.epoch, user.token_epoch,
        "ep must be the stored epoch"
    );
    assert!(
        claims.iat >= mint_window.0 && claims.iat <= mint_window.1,
        "iat must sit inside the request clock window"
    );
    assert!(claims.exp > 0, "a session token must never be non-expiring");
    assert_eq!(
        claims.exp,
        claims.iat + auth.session_timeout_secs(),
        "the lifetime must be the configured session timeout"
    );
    claims
}

#[tokio::test]
async fn login_token_is_native_expiring_and_roundtrips_strict_api() {
    let (store, path, auth) = setup();
    let state = LoginState::new(
        store.clone(),
        auth.clone(),
        20,
        Duration::from_secs(60),
        false,
    )
    .unwrap();
    let app = routes(state);

    // First admin through the real handler, then an ordinary /login.
    let created = app
        .clone()
        .oneshot(request(
            "/create",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let before = now_secs();
    let response = app
        .clone()
        .oneshot(request(
            "/login",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let after = now_secs();
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let axum::Json(payload) = axum::Json::<TokenPayload>::from_bytes(&body).unwrap();

    let user = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
        .unwrap()
        .unwrap();
    assert_native_session_claims(&auth, &payload.token, &user, (before, after));

    // The exact token the UI received authenticates the strict API gate.
    let api = api_routes(store.clone(), auth.clone());
    let api_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {}", payload.token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.clone().oneshot(api_request).await.unwrap().status(),
        StatusCode::OK,
        "the login token must be accepted by AuthGate::Api unchanged"
    );

    // Non-admin sibling: adm decodes false, and the token still works for its
    // own user at the API gate.
    store
        .with_connection_sync(|conn| {
            let stamp = "2024-01-01T00:00:00Z";
            UserRepo::new(conn)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "bob-id",
                        user_name: "bob",
                        name: "Bob",
                        email: "",
                        password: Some(
                            &nd_auth::credentials::password::hash_password("bobpw").unwrap(),
                        ),
                        is_admin: false,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    let before = now_secs();
    let response = app
        .clone()
        .oneshot(request(
            "/login",
            r#"{"username":"bob","password":"bobpw"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let axum::Json(payload) = axum::Json::<TokenPayload>::from_bytes(&body).unwrap();
    let bob = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("bob")))
        .unwrap()
        .unwrap();
    let claims = assert_native_session_claims(&auth, &payload.token, &bob, (before, now_secs()));
    assert!(!claims.is_admin);

    let bob_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {}", payload.token))
        .body(Body::empty())
        .unwrap();
    let bob_app = Router::new()
        .route(
            "/",
            get(
                |axum::Extension(user): axum::Extension<AuthenticatedUser>| async move {
                    assert_eq!(user.username, "bob");
                    assert!(!user.is_admin);
                    "ok"
                },
            ),
        )
        .layer(from_fn_with_state(
            MiddlewareState {
                store: store.clone(),
                auth: auth.clone(),
                gate: AuthGate::Api,
                resolution: ResolutionConfig::default(),
            },
            authenticate,
        ));
    assert_eq!(
        bob_app.oneshot(bob_request).await.unwrap().status(),
        StatusCode::OK,
        "a non-admin login token authenticates its own user at the API gate"
    );

    // A wrong password still yields 401 with no token anywhere.
    let wrong = app
        .oneshot(request(
            "/login",
            r#"{"username":"alice","password":"wrong"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(!wrong.headers().contains_key("x-nd-authorization"));
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn create_admin_token_roundtrips_strict_api() {
    // The first-setup path shares `payload`, so its token must be native and
    // expiring too.
    let (store, path, auth) = setup();
    let app = routes(
        LoginState::new(
            store.clone(),
            auth.clone(),
            20,
            Duration::from_secs(60),
            false,
        )
        .unwrap(),
    );
    let before = now_secs();
    let response = app
        .clone()
        .oneshot(request(
            "/create",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let after = now_secs();
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let axum::Json(payload) = axum::Json::<TokenPayload>::from_bytes(&body).unwrap();

    let (count, user) = store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            Ok((repo.count_all().unwrap(), repo.find_first_admin().unwrap()))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(user.is_admin);
    assert!(
        nd_auth::credentials::password::verify_password("secret", &user.password).unwrap(),
        "the stored credential is a real argon2id hash"
    );
    assert_native_session_claims(&auth, &payload.token, &user, (before, after));

    let api = api_routes(store.clone(), auth.clone());
    let api_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {}", payload.token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.clone().oneshot(api_request).await.unwrap().status(),
        StatusCode::OK,
        "the first-admin token must authenticate the strict API gate"
    );

    // Second first-admin attempt is still refused and mints nothing.
    let another = app
        .clone()
        .oneshot(request("/create", r#"{"username":"bob","password":"pw"}"#))
        .await
        .unwrap();
    assert_eq!(another.status(), StatusCode::FORBIDDEN);
    assert!(!another.headers().contains_key("x-nd-authorization"));
    assert_eq!(
        store
            .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
            .unwrap(),
        1
    );

    // A login for the same admin yields the same shape (shared payload path).
    let login = app
        .oneshot(request(
            "/login",
            r#"{"username":"alice","password":"secret"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let body = to_bytes(login.into_body(), 8192).await.unwrap();
    let axum::Json(relogin) = axum::Json::<TokenPayload>::from_bytes(&body).unwrap();
    assert_native_session_claims(&auth, &relogin.token, &user, (before, now_secs() + 1));
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn invalid_session_timeout_fails_login_closed_without_a_token() {
    // Same signing keys, misconfigured session timeout: the stored credential
    // is still valid, but the mint is refused. Both paths that share `payload`
    // — first-admin `/create` and ORDINARY `/login` against the committed admin
    // — must return the existing generic sanitized 500 with no token anywhere.
    // Asserts the exact existing body text, so an internal error string cannot
    // slip through; production body policy is not changed by this test.
    for timeout in [0_i64, -1, i64::MAX] {
        let (store, path, _) = setup();
        let auth = Authenticator::new(
            SigningSecrets {
                current: b"test signing secret".to_vec(),
                previous: None,
                public_current: b"other secret".to_vec(),
            },
            timeout,
        );
        let state = LoginState::new(
            store.clone(),
            auth.clone(),
            20,
            Duration::from_secs(60),
            false,
        )
        .unwrap();
        let app = routes(state);

        for (route, body) in [
            ("/create", r#"{"username":"alice","password":"secret"}"#),
            ("/login", r#"{"username":"alice","password":"secret"}"#),
        ] {
            let response = app.clone().oneshot(request(route, body)).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "timeout {timeout} must fail the {route} mint, not mint a token"
            );
            assert!(
                !response.headers().contains_key("x-nd-authorization"),
                "{route} must advertise no replacement token"
            );
            let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert_eq!(
                text, GENERIC_LOGIN_FAILURE,
                "{route} must return the existing generic sanitized body verbatim"
            );
            assert_no_jwt_payload(text, route);
        }

        // The credential itself is fine, so the failure really is the mint: the
        // admin row exists and the password verifies against the stored hash.
        let user = store
            .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .with_connection_sync(|conn| Ok(UserRepo::new(conn).count_all().unwrap()))
                .unwrap(),
            1,
            "the admin row committed before the mint failed (config atomicity is BOOT's job, an accepted known boundary)"
        );
        assert!(
            nd_auth::credentials::password::verify_password("secret", &user.password).unwrap(),
            "the stored credential is valid — only the mint failed"
        );
        drop(app);
        drop(store);
        let _ = std::fs::remove_file(path);
    }
}

/// The exact generic body `login.rs` returns on an unauthenticated internal
/// failure — pinned verbatim, not merely "does not contain `token`".
const GENERIC_LOGIN_FAILURE: &str = "Unknown error authenticating user";

/// Assert a failure body carries no minted token in any shape: no `token`
/// field, no `Bearer` value, no base64url JWT segment, and at most one `.`
/// (a three-part JWT would have two).
fn assert_no_jwt_payload(text: &str, route: &str) {
    assert!(
        !text.contains("token"),
        "{route} body must carry no token: {text}"
    );
    assert!(
        !text.contains("Bearer"),
        "{route} body must carry no bearer: {text}"
    );
    assert!(
        !text.contains("eyJ"),
        "{route} body must carry no JWT: {text}"
    );
    let dots = text.matches('.').count();
    assert!(
        dots <= 1,
        "{route} body must not contain a JWT structure: {text}"
    );
}

#[tokio::test]
async fn refresh_emits_no_token_when_session_timeout_is_invalid() {
    // A token minted by a correctly configured Authenticator is still accepted
    // by a same-key Authenticator with an invalid timeout: the request
    // authenticates and the body is preserved, but the refresh cannot re-stamp
    // a valid expiry, so no new token is advertised.
    let (store, path, auth) = setup();
    store
        .with_connection_sync(|conn| {
            let stamp = "2024-01-01T00:00:00Z";
            let hash = nd_auth::credentials::password::hash_password("secret").unwrap();
            UserRepo::new(conn)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "alice-id",
                        user_name: "alice",
                        name: "Alice",
                        email: "",
                        password: Some(&hash),
                        is_admin: true,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    let token = auth
        .create_native_session_token(
            &nd_auth::claims::TokenSubject {
                id: "alice-id".into(),
                token_epoch: 0,
            },
            "alice",
            true,
            now_secs(),
        )
        .unwrap();

    let broken = Authenticator::new(
        SigningSecrets {
            current: b"test signing secret".to_vec(),
            previous: None,
            public_current: b"other secret".to_vec(),
        },
        0,
    );
    let app = Router::new()
        .route("/", get(|| async { "body survives" }))
        .layer(from_fn_with_state(broken.clone(), refresh))
        .layer(from_fn_with_state(
            MiddlewareState {
                store: store.clone(),
                auth: broken,
                gate: AuthGate::Api,
                resolution: ResolutionConfig::default(),
            },
            authenticate,
        ));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("x-nd-authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !response.headers().contains_key("x-nd-authorization"),
        "an invalid expiry must emit no replacement token"
    );
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    assert_eq!(std::str::from_utf8(&body).unwrap(), "body survives");
    drop(store);
    let _ = std::fs::remove_file(path);
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// #13 setup-store refactor: the narrow synchronous helper (`setup_in_tx`).
//
// Everything below uses a synthetic temp DB, a synthetic password hash and a
// synthetic library row. The helper is executed *inside a caller-owned
// transaction* to prove it performs the real writes — including
// `UserRepo::put`'s admin grant leg — while contributing no transaction control
// of its own. Hashing placement is NOT asserted here by timing; it is proved
// deterministically by the counting-hash tests in `setup_store`'s own module.
// ---------------------------------------------------------------------------

/// A temp store whose pool has exactly one connection, so any nested checkout
/// by `setup_in_tx` would block forever rather than silently succeed.
fn setup_single_connection() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-setup-tx-", ".db"));
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 1)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

const SYNTHETIC_HASH: &str = "$argon2id$v=19$m=19,t=2,p=1$c29tZS1zeW50aGV0aWM$fake-hash-for-tests";

fn marker_value(store: &SQLStore) -> Option<String> {
    store
        .with_connection_sync(|conn| {
            nd_db::execution::query_optional(
                conn,
                &get_sql(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
                |row| row.get::<_, String>(0),
            )
        })
        .unwrap()
}

/// Seed a synthetic library row, committed outside any setup transaction. It
/// exists so `UserRepo::put`'s admin auto-assign
/// (`user_repository.go:150-156`, run by `setup_in_tx`) has something to grant:
/// with no library rows the grant leg is a no-op and the assertion would be
/// vacuous.
fn seed_library(store: &SQLStore, name: &str, path: &str) -> i64 {
    store
        .with_connection_sync(|conn| {
            conn.execute(
                "INSERT INTO library (name, path) VALUES (?, ?)",
                rusqlite::params![name, path],
            )?;
            conn.query_row("SELECT id FROM library WHERE name = ?", [name], |row| {
                row.get::<_, i64>(0)
            })
        })
        .expect("seed a synthetic library")
}

/// The actual `(user_id, library_id)` grant rows, read directly from the table.
fn grant_rows(store: &SQLStore) -> Vec<(String, i64)> {
    store
        .with_connection_sync(|conn| {
            let mut statement = conn.prepare(
                "SELECT user_id, library_id FROM user_library ORDER BY user_id, library_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .expect("read grant rows")
}

/// The same read inside a caller-owned transaction, so a grant written by the
/// helper can be observed before the transaction is rolled back.
fn grant_rows_in_tx(tx: &rusqlite::Transaction<'_>, user_id: &str) -> Vec<i64> {
    let mut statement = tx
        .prepare("SELECT library_id FROM user_library WHERE user_id = ?")
        .expect("prepare grant read");
    statement
        .query_map([user_id], |row| row.get::<_, i64>(0))
        .expect("query grants")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read grants in tx")
}

fn user_rows(store: &SQLStore) -> (i64, Option<String>) {
    store
        .with_connection_sync(|conn| {
            let repo = UserRepo::new(conn);
            Ok((
                repo.count_all().unwrap(),
                repo.find_by_username(nd_auth::initial_setup::DEV_INITIAL_USER_NAME)
                    .ok()
                    .map(|user| user.password),
            ))
        })
        .unwrap()
}

#[tokio::test]
async fn helper_writes_then_forced_rollback_leaves_nothing_behind() {
    // Pool size 1: the outer transaction holds the only connection, so a
    // checkout inside `setup_in_tx` could not even be admitted. The outer tx
    // inserts a synthetic sentinel, runs the REAL helper, then forces a
    // rollback; nothing from either the sentinel or the helper may survive.
    let (store, path) = setup_single_connection();
    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "pool is exactly one connection"
    );
    // A seeded library makes the admin grant leg real rather than vacuous.
    let library_id = seed_library(&store, "synthetic-library", "/tmp/synthetic-library");

    let stamp: &str = "2024-01-01T00:00:00Z";
    let sentinel_key: &str = "synthetic-sentinel";
    let wrote = store
        .with_tx(move |tx| -> Result<bool, rusqlite::Error> {
            let sentinel = put_sql(sentinel_key, "sentinel-value");
            if nd_db::execution::execute(tx, &sentinel.update).unwrap() == 0 {
                nd_db::execution::execute(tx, &sentinel.insert).unwrap();
            }
            // The real helper, on the caller's borrowed transaction.
            let wrote = setup_in_tx(
                tx,
                Some(&DevAdmin {
                    password_hash: SYNTHETIC_HASH,
                }),
                stamp,
            )
            .unwrap();
            // Proving the helper DID write, inside this transaction, before
            // anything is rolled back.
            assert!(
                wrote,
                "setup was not completed, so the helper must report true"
            );
            let repo = UserRepo::new(tx);
            assert_eq!(
                repo.count_all().unwrap(),
                1,
                "the helper must have inserted the dev admin inside the tx"
            );
            assert_eq!(
                repo.find_first_admin().unwrap().password,
                SYNTHETIC_HASH,
                "the helper must store the caller's prepared hash verbatim"
            );
            assert_eq!(
                nd_db::execution::query_optional(
                    tx,
                    &get_sql(nd_auth::initial_setup::INITIAL_SETUP_FLAG_KEY),
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                .as_deref(),
                Some(stamp),
                "the helper must have written the marker inside the tx"
            );
            assert_eq!(
                nd_db::execution::query_optional(tx, &get_sql(sentinel_key), |row| row
                    .get::<_, String>(0),)
                .unwrap()
                .as_deref(),
                Some("sentinel-value"),
                "the caller's sentinel must be visible in the same tx"
            );
            // `UserRepo::put` assigns every library to an admin
            // (`user_repository.go:150-156`), so the helper's insert also
            // produced a concrete grant row — observed inside this tx.
            let admin_id = repo.find_first_admin().unwrap().id;
            assert_eq!(
                grant_rows_in_tx(tx, &admin_id),
                vec![library_id],
                "the helper's admin must hold the seeded grant inside the caller's tx"
            );
            // Forced failure AFTER the helper wrote: rusqlite rolls the tx back.
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE),
                Some("forced rollback after helper wrote".to_owned()),
            ))
        })
        .await
        .unwrap_err();
    assert!(wrote
        .to_string()
        .contains("forced rollback after helper wrote"));

    assert_eq!(
        store.idle_count().unwrap(),
        1,
        "the lease came back to the size-1 pool"
    );
    let (users, password) = user_rows(&store);
    assert_eq!(users, 0, "rolled-back helper insert must leave no user row");
    assert_eq!(password, None, "no dev admin may survive the rollback");
    assert_eq!(
        marker_value(&store),
        None,
        "rolled-back marker write must leave no setup flag"
    );
    assert_eq!(
        store
            .with_connection_sync(|conn| nd_db::execution::query_optional(
                conn,
                &get_sql(sentinel_key),
                |row| row.get::<_, String>(0),
            ))
            .unwrap(),
        None,
        "the caller's sentinel must be rolled back too"
    );
    // The grant written by `UserRepo::put`'s admin leg rolls back with
    // everything else, while the pre-existing seeded library row is untouched.
    assert_eq!(
        grant_rows(&store),
        Vec::new(),
        "a rolled-back helper must leave no grant row"
    );
    assert_eq!(
        store
            .with_connection_sync(|conn| {
                Ok(conn.query_row("SELECT count(*) FROM library", [], |row| {
                    row.get::<_, i64>(0)
                }))
            })
            .unwrap()
            .unwrap(),
        1,
        "the seeded library row must survive the rollback"
    );

    // A later real setup still succeeds: the aborted attempt consumed nothing.
    assert!(
        run_initial_setup(&store, "dev-password", "2024-01-02T00:00:00Z")
            .await
            .unwrap(),
        "a rolled-back attempt must not have consumed the one-shot setup flag"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn helper_committed_by_caller_persists_marker_user_and_grant() {
    // The successful path of the same shape: the caller commits, so the helper's
    // writes survive — proving the helper performs real writes and that its
    // rollback behavior above came from the caller's transaction, not itself.
    let (store, path) = setup_single_connection();
    let stamp: &str = "2024-01-01T00:00:00Z";
    let library_id = seed_library(&store, "committed-library", "/tmp/committed-library");
    let wrote = store
        .with_tx(move |tx| {
            setup_in_tx(
                tx,
                Some(&DevAdmin {
                    password_hash: SYNTHETIC_HASH,
                }),
                stamp,
            )
        })
        .await
        .unwrap();
    assert!(wrote);
    let (users, password) = user_rows(&store);
    assert_eq!(users, 1);
    assert_eq!(password.as_deref(), Some(SYNTHETIC_HASH));
    assert_eq!(marker_value(&store).as_deref(), Some(stamp));
    // Committing the caller's tx makes the helper's grant durable, against the
    // same seeded library id.
    let admin_id = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_first_admin().unwrap().id))
        .unwrap();
    assert_eq!(
        grant_rows(&store),
        vec![(admin_id.clone(), library_id)],
        "the committed helper write must persist the exact (admin_id, library_id) grant"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn helper_rechecks_marker_in_tx_and_never_replaces_an_existing_admin() {
    let (store, path) = setup_single_connection();
    let stamp: &str = "2024-01-01T00:00:00Z";
    // Seed a library first, so the pre-existing admin acquires a real grant
    // through `UserRepo::put`'s admin leg and the preservation assertions below
    // have something to preserve.
    let library_id = seed_library(&store, "preexisting-library", "/tmp/preexisting-library");
    // A pre-existing regular user, and separately a pre-existing admin, both
    // committed before the helper runs.
    store
        .with_tx(move |tx| {
            UserRepo::new(tx)
                .put(
                    &nd_db::user_repo::UserPutValues {
                        id: "existing-admin",
                        user_name: nd_auth::initial_setup::DEV_INITIAL_USER_NAME,
                        name: "Existing",
                        email: "",
                        password: Some("existing-hash"),
                        is_admin: true,
                        last_login_at: None,
                        last_access_at: None,
                        created_at: stamp,
                        updated_at: stamp,
                        scrobble_filter: "",
                    },
                    stamp,
                    false,
                )
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    let wrote = store
        .with_tx(move |tx| {
            setup_in_tx(
                tx,
                Some(&DevAdmin {
                    password_hash: SYNTHETIC_HASH,
                }),
                stamp,
            )
        })
        .await
        .unwrap();
    assert!(wrote, "a missing marker still completes setup");
    let (users, password) = user_rows(&store);
    assert_eq!(users, 1, "the existing admin must not be duplicated");
    assert_eq!(
        password.as_deref(),
        Some("existing-hash"),
        "the existing admin's credential must never be replaced"
    );
    assert_eq!(marker_value(&store).as_deref(), Some(stamp));
    // The helper skipped the insert, so it also skipped the grant leg: the
    // pre-existing admin keeps exactly its own grant.
    assert_eq!(
        grant_rows(&store),
        vec![("existing-admin".to_owned(), library_id)],
        "the skipped-insert path must leave the pre-existing grant untouched"
    );

    // With the marker present, the in-tx recheck short-circuits: `false` and no
    // writes at all.
    let again = store
        .with_tx(move |tx| {
            setup_in_tx(
                tx,
                Some(&DevAdmin {
                    password_hash: "a-different-hash",
                }),
                "2024-02-01T00:00:00Z",
            )
        })
        .await
        .unwrap();
    assert!(
        !again,
        "the marker check inside the transaction must short-circuit"
    );
    let (users, password) = user_rows(&store);
    assert_eq!(users, 1);
    assert_eq!(password.as_deref(), Some("existing-hash"));
    assert_eq!(
        marker_value(&store).as_deref(),
        Some(stamp),
        "marker value is not overwritten"
    );

    // The marker-present short-circuit must not disturb existing grants either:
    // no re-grant, no re-insert, no credential change.
    assert_eq!(
        grant_rows(&store),
        vec![("existing-admin".to_owned(), library_id)],
        "the marker-present path must preserve pre-existing grants"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn helper_without_dev_admin_writes_only_the_marker() {
    // The no-password arm (`server/initial_setup.go:26-27`): marker only, no user.
    let (store, path) = setup_single_connection();
    let stamp: &str = "2024-01-01T00:00:00Z";
    let wrote = store
        .with_tx(move |tx| setup_in_tx(tx, None, stamp))
        .await
        .unwrap();
    assert!(wrote);
    let (users, password) = user_rows(&store);
    assert_eq!(users, 0, "no password means no user row");
    assert_eq!(password, None);
    assert_eq!(marker_value(&store).as_deref(), Some(stamp));
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn busy_classification_and_error_surface_match_the_store_policy() {
    // The API adds no retry policy of its own: retryability is the store's
    // classification, and extended busy codes are included (`persistence.go:207-228`,
    // `db/db.go:128-140`). Only the exhausted outcome is the retryable signal.
    assert!(nd_db::store::is_busy(&rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        None,
    )));
    assert!(nd_db::store::is_busy(&rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
        None,
    )));
    assert!(!nd_db::store::is_busy(&rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
        None,
    )));

    let exhausted: SetupStoreError = nd_db::store::StoreError::BusyRetriesExhausted {
        attempts: 4,
        cause: "database is locked".to_owned(),
    }
    .into();
    assert!(matches!(
        exhausted,
        SetupStoreError::Store(nd_db::store::StoreError::BusyRetriesExhausted { .. })
    ));

    // Scope of this claim: `SetupStoreError::Store` is `#[error(transparent)]`
    // over the store failure and the module adds no payload of its own, so
    // *nothing this module contributes* can name the hash, the password or the
    // field. SQLite's own diagnostic text (constraint names, SQL) is passed
    // through verbatim and is SQLite's business, not a guarantee made here.
    let text = format!("{exhausted} | {exhausted:?}");
    assert!(text.contains("database is locked"));
    assert!(!text.contains(SYNTHETIC_HASH));
    assert!(!text.contains("dev-password"));
    assert!(!text.contains("password_hash"));
}

#[tokio::test]
async fn a_not_busy_failure_through_the_helper_surfaces_immediately_and_writes_nothing() {
    // A not-busy SQL failure (`persistence.go:218`) is not retried, so one
    // attempt runs and nothing commits. Busy exhaustion is covered deterministically
    // in `setup_store`'s own tests, which drive the store's retry loop directly.
    let (store, path, _) = setup();
    let stamp: &str = "2024-01-01T00:00:00Z";
    let error = store
        .with_tx_retry_sync(move |tx| -> Result<bool, rusqlite::Error> {
            setup_in_tx(
                tx,
                Some(&DevAdmin {
                    password_hash: SYNTHETIC_HASH,
                }),
                stamp,
            )?;
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
                Some("forced not-busy failure".to_owned()),
            ))
        })
        .expect_err("SQLITE_LOCKED is not busy-class, so the attempt fails immediately");
    // `StoreError::Other` is the store's "not busy, fail immediately" arm.
    assert!(matches!(error, nd_db::store::StoreError::Other(_)));
    let mapped: SetupStoreError = error.into();
    assert!(!matches!(
        mapped,
        SetupStoreError::Store(nd_db::store::StoreError::BusyRetriesExhausted { .. })
    ));
    assert!(!format!("{mapped}").contains(SYNTHETIC_HASH));
    let (users, marker) = (user_rows(&store).0, marker_value(&store));
    assert_eq!(users, 0, "a failed attempt commits no user row");
    assert_eq!(marker, None, "a failed attempt commits no marker");
    drop(store);
    let _ = std::fs::remove_file(path);
}
