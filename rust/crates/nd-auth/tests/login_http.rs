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
    let app =
        routes(LoginState::new(store.clone(), auth, 1, Duration::from_secs(60), false).unwrap());
    let oversized = Request::builder()
        .method("POST")
        .uri("/login")
        .body(Body::from(vec![b'x'; 8193]))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
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
