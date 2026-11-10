//! Authentication against the executing SQLite user repository.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::Router;
use nd_auth::claims::TokenSubject;
use nd_auth::credentials::{password, Authenticator, SigningSecrets};
use nd_auth::middleware::{authenticate, AuthGate, MiddlewareState, ProxyPeer};
use nd_auth::verifier::ResolutionConfig;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::SQLStore;
use nd_db::user_repo::{UserPutValues, UserRepo};
use nd_dbschema::schema::apply_fresh_schema;
use tower::ServiceExt;

const STAMP: &str = "2024-01-01T00:00:00Z";

fn fixture() -> (SQLStore, std::path::PathBuf, Authenticator) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-", ".db"));
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    let store = SQLStore::new(pool);
    let hash = password::hash_password("password").unwrap();
    store
        .with_connection_sync(|conn| {
            let values = UserPutValues {
                id: "user-id",
                user_name: "alice",
                name: "Alice",
                email: "",
                password: Some(&hash),
                is_admin: true,
                last_login_at: None,
                last_access_at: None,
                created_at: STAMP,
                updated_at: STAMP,
                scrobble_filter: "",
            };
            UserRepo::new(conn).put(&values, STAMP, false).unwrap();
            Ok(())
        })
        .unwrap();
    let auth = Authenticator::new(
        SigningSecrets {
            current: b"test signing secret".to_vec(),
            previous: None,
            public_current: b"other secret".to_vec(),
        },
        3600,
    );
    (store, path, auth)
}

fn app(
    store: SQLStore,
    auth: Authenticator,
    gate: AuthGate,
    resolution: ResolutionConfig,
) -> Router {
    Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(from_fn_with_state(
            MiddlewareState {
                store,
                auth,
                gate,
                resolution,
            },
            authenticate,
        ))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn session_epoch_and_api_audience_are_checked_against_real_user() {
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth.create_token(&subject, "alice", true, now()).unwrap();
    let session = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig::default(),
    );
    let api = app(
        store.clone(),
        auth,
        AuthGate::Api,
        ResolutionConfig::default(),
    );
    let request = || {
        Request::builder()
            .uri("/")
            .header("x-nd-authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        session.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        api.oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    store
        .with_connection_sync(|conn| {
            let hash = password::hash_password("new password").unwrap();
            let values = UserPutValues {
                id: "user-id",
                user_name: "alice",
                name: "Alice",
                email: "",
                password: Some(&hash),
                is_admin: true,
                last_login_at: None,
                last_access_at: None,
                created_at: STAMP,
                updated_at: STAMP,
                scrobble_filter: "",
            };
            UserRepo::new(conn).put(&values, STAMP, true).unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        session.oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn external_auth_checks_the_transport_peer() {
    let (store, path, auth) = fixture();
    let config = ResolutionConfig {
        external_user_header: "x-user".into(),
        trusted_sources: "10.0.0.0/8,@".into(),
        server_address: "unix:/run/server.sock".into(),
        ..ResolutionConfig::default()
    };
    let app = app(store.clone(), auth, AuthGate::Session, config);
    for (peer, expected) in [
        ("10.1.2.3", StatusCode::OK),
        ("11.1.2.3", StatusCode::UNAUTHORIZED),
        ("@", StatusCode::OK),
    ] {
        let mut request = Request::builder()
            .uri("/")
            .header("x-user", "alice")
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ProxyPeer(peer.into()));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            expected
        );
    }
    drop(app);
    drop(store);
    let _ = std::fs::remove_file(path);
}
