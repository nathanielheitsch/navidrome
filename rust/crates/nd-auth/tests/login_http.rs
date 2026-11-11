//! Executed login and first-admin HTTP tests against a fresh schema.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::Router;
use nd_auth::credentials::{Authenticator, SigningSecrets};
use nd_auth::login::{create_admin, login, login_from_headers, HeaderLoginState, LoginState};
use nd_auth::middleware::{authenticate, AuthGate, MiddlewareState, ProxyPeer};
use nd_auth::setup_store::run_initial_setup;
use nd_auth::verifier::ResolutionConfig;
use nd_db::pool::{Pool, PoolConfig};
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
    let pool = Pool::new(&PoolConfig::with_size(dsn, 4)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
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
