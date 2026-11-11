//! Authentication against the executing SQLite user repository.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::Router;
use nd_auth::claims::TokenSubject;
use nd_auth::credentials::{password, Authenticator, SigningSecrets};
use nd_auth::http_context::TokenEpochHolder;
use nd_auth::middleware::{authenticate, AuthGate, MiddlewareState, ProxyPeer};
use nd_auth::refresh::refresh;
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
    let api_token = auth
        .create_api_token(&subject, "alice", true, now())
        .unwrap();
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
        api.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let api_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {api_token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.oneshot(api_request).await.unwrap().status(),
        StatusCode::OK
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

#[tokio::test]
async fn refresh_carries_committed_handler_epoch() {
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth.create_token(&subject, "alice", true, now()).unwrap();
    let hash = password::hash_password("new password").unwrap();
    let handler_store = store.clone();
    let route = get(
        move |axum::Extension(holder): axum::Extension<TokenEpochHolder>| {
            let store = handler_store.clone();
            let hash = hash.clone();
            async move {
                let outcome = store
                    .with_connection(move |conn| {
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
                        Ok(UserRepo::new(conn).put(&values, STAMP, true))
                    })
                    .await
                    .unwrap()
                    .unwrap();
                holder.record_put(&outcome);
                "ok"
            }
        },
    );
    let state = MiddlewareState {
        store: store.clone(),
        auth: auth.clone(),
        gate: AuthGate::Session,
        resolution: ResolutionConfig::default(),
    };
    let app = Router::new()
        .route("/", route)
        .layer(from_fn_with_state(auth.clone(), refresh))
        .layer(from_fn_with_state(state, authenticate));
    let request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let refreshed = response
        .headers()
        .get("x-nd-authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(auth.validate(refreshed).unwrap().epoch, 1);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn refresh_includes_header_for_empty_response() {
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth.create_token(&subject, "alice", true, now()).unwrap();
    let state = MiddlewareState {
        store: store.clone(),
        auth: auth.clone(),
        gate: AuthGate::Session,
        resolution: ResolutionConfig::default(),
    };
    let app = Router::new()
        .route("/", get(|| async { StatusCode::NO_CONTENT }))
        .layer(from_fn_with_state(auth, refresh))
        .layer(from_fn_with_state(state, authenticate));
    let request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.headers().contains_key("x-nd-authorization"));
    drop(store);
    let _ = std::fs::remove_file(path);
}
