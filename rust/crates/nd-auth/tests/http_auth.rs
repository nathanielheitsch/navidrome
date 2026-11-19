//! Authentication against the executing SQLite user repository.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use axum::Router;
use nd_auth::claims::TokenSubject;
use nd_auth::credentials::{password, AuthError, Authenticator, SigningSecrets};
use nd_auth::http_context::TokenEpochHolder;
use nd_auth::middleware::{authenticate, AuthGate, AuthenticatedUser, MiddlewareState, ProxyPeer};
use nd_auth::refresh::refresh;
use nd_auth::verifier::ResolutionConfig;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::SQLStore;
use nd_db::user_repo::{PutOutcome, UserError, UserPutValues, UserRepo};
use nd_dbschema::schema::apply_fresh_schema;
use tower::ServiceExt;

const STAMP: &str = "2024-01-01T00:00:00Z";

fn fixture() -> (SQLStore, std::path::PathBuf, Authenticator) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-auth-", ".db"));
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
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

/// Legacy Go-parity shape end-to-end: the aud-less token authenticates the
/// Session gate, is rejected by the strict API gate, and dies when a real
/// password change bumps the epoch. The deliberate native expiring control
/// lives in `native_session_token_authenticates_the_api_gate_and_epoch_revokes_it`
/// and the explicit aud-less API rejection in
/// `legacy_aud_less_token_is_a_deliberate_api_gate_negative_control`.
///
/// BOOT-02: login no longer mints this aud-less shape; `create_token` is kept
/// only for Go-wire compatibility and as a negative control.
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
#[tokio::test]
async fn conflicting_auth_sources_are_rejected_not_silently_preferred() {
    // A4: Go's precedence walk (server/auth.go:257-266) would authenticate these
    // requests from the highest-priority arm and never check the other credential.
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth.create_token(&subject, "alice", true, now()).unwrap();
    let other = auth
        .create_token(
            &TokenSubject {
                id: "someone-else".into(),
                token_epoch: 0,
            },
            "mallory",
            true,
            now(),
        )
        .unwrap();

    // Dev auto-login as alice + a token naming mallory: Go authenticates as alice
    // and ignores the token entirely. Agreeing sources still authenticate.
    let dev_app = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig {
            dev_username: "alice".into(),
            ..ResolutionConfig::default()
        },
    );
    for (creds, expected, why) in [
        (
            other.clone(),
            StatusCode::UNAUTHORIZED,
            "disagreeing token must be a conflict",
        ),
        (
            token.clone(),
            StatusCode::OK,
            "agreeing sources must still authenticate",
        ),
    ] {
        let request = Request::builder()
            .uri("/")
            .header("x-nd-authorization", format!("Bearer {creds}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            dev_app.clone().oneshot(request).await.unwrap().status(),
            expected,
            "{why}"
        );
    }

    // Trusted proxy header naming alice + a token naming mallory: a conflict.
    let proxy_app = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig {
            external_user_header: "x-user".into(),
            trusted_sources: "10.0.0.0/8".into(),
            ..ResolutionConfig::default()
        },
    );
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "alice")
        .header("x-nd-authorization", format!("Bearer {other}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("10.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "trusted proxy header plus a disagreeing token must be rejected"
    );

    // An UNTRUSTED peer cannot authenticate through the header alone.
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "alice")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("11.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "an untrusted proxy header cannot authenticate"
    );

    // Untrusted peer + header naming alice + a token naming mallory: the header is
    // not counted as a source at all, so only the token decides — and mallory is
    // not in the DB. An untrusted proxy must not be able to manufacture a conflict.
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "alice")
        .header("x-nd-authorization", format!("Bearer {other}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("11.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "an untrusted header is not a source, so the mallory token decides alone"
    );

    // Control: same untrusted peer, same disagreeing header, but the ALICE token.
    // This succeeds, proving the rejection above came from the token's user and not
    // from a spurious conflict.
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "mallory")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("11.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::OK,
        "the alice token authenticates even beside an untrusted disagreeing header"
    );

    // Agreeing sources (token names alice, TRUSTED header names Alice) authenticate.
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "Alice")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("10.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::OK,
        "sources naming the same user must still be accepted"
    );

    // And the trusted header beside a DISAGREEING token is rejected, completing the
    // contrast with the untrusted control above.
    let mut request = Request::builder()
        .uri("/")
        .header("x-user", "alice")
        .header("x-nd-authorization", format!("Bearer {other}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ProxyPeer("10.1.2.3".into()));
    assert_eq!(
        proxy_app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "a trusted header plus a disagreeing token must be a conflict"
    );

    // A forged token next to a valid dev-config arm is rejected, not ignored.
    let bogus = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig {
            dev_username: "alice".into(),
            ..ResolutionConfig::default()
        },
    );
    let request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", "Bearer not-a-valid-token")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        bogus.oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn token_bound_to_a_different_id_fails_revalidation() {
    // This proves `check_session_claims` is actually invoked on the authenticated
    // path (the user_id check inside it is what rejects the token), not that the
    // middleware's defense-in-depth `sub` comparison is reachable — after A4's
    // conflict detection that comparison cannot trigger.
    let (store, path, auth) = fixture();
    // Valid signature, sub matches the DB user, but the token is bound to a
    // different user id: check_session_claims must reject it.
    let mismatched = auth
        .create_token(
            &TokenSubject {
                id: "wrong-id".into(),
                token_epoch: 0,
            },
            "alice",
            true,
            now(),
        )
        .unwrap();
    let router = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig::default(),
    );
    let request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {mismatched}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "a token bound to another id must fail revalidation even when sub matches"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

// ---- BOOT-02: native expiring session tokens at the strict API gate ----
//
// Every token below is checked against the real executing SQLite user
// repository through `authenticate` with `AuthGate::Api` and
// `ResolutionConfig::default()` (no dev username, no trusted header), so a 200
// can only come from the credential itself. No production route is mounted.

/// Strict `AuthGate::Api` fixture. The handler asserts the authenticated
/// identity, so a 200 proves real revalidation rather than a middleware bypass.
fn api_app(store: SQLStore, auth: Authenticator) -> Router {
    Router::new()
        .route(
            "/",
            get(
                |axum::Extension(user): axum::Extension<AuthenticatedUser>| async move {
                    assert_eq!(user.username, "alice");
                    assert_eq!(user.id, "user-id");
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

fn bearer(token: &str) -> Request<Body> {
    Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

/// Sign `claims` with the fixture's session secret, so a control differs from
/// the valid twin in exactly the claim under test.
fn sign(claims: &nd_auth::claims::Claims) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        claims,
        &jsonwebtoken::EncodingKey::from_secret(b"test signing secret"),
    )
    .unwrap()
}

/// Commit a real password change through the repository, which bumps
/// `token_epoch` (`persistence/user_repository.go:129-144`).
fn change_password(store: &SQLStore, password: &str) {
    let hash = password::hash_password(password).unwrap();
    store
        .with_connection_sync(move |conn| {
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
}

#[tokio::test]
async fn native_session_token_authenticates_the_api_gate_and_epoch_revokes_it() {
    // BOOT-02's positive path plus epoch revocation, on one native token.
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let native = auth
        .create_native_session_token(&subject, "alice", true, now())
        .unwrap();
    let api = api_app(store.clone(), auth.clone());
    assert_eq!(
        api.clone().oneshot(bearer(&native)).await.unwrap().status(),
        StatusCode::OK,
        "the native expiring session token must authenticate the strict API"
    );

    // A real password change bumps the epoch: the old token is dead.
    change_password(&store, "new password");
    assert_eq!(
        api.clone().oneshot(bearer(&native)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "a stale epoch must revoke an otherwise valid native token"
    );

    // The token minted at the committed epoch still works: the rejection above
    // came from the epoch, not from audience or expiry.
    let user = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
        .unwrap()
        .unwrap();
    assert_eq!(user.token_epoch, 1);
    let fresh = auth
        .create_native_session_token(
            &TokenSubject {
                id: "user-id".into(),
                token_epoch: user.token_epoch,
            },
            "alice",
            true,
            now(),
        )
        .unwrap();
    assert_eq!(
        api.oneshot(bearer(&fresh)).await.unwrap().status(),
        StatusCode::OK,
        "a token at the committed epoch must authenticate"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn legacy_aud_less_token_is_a_deliberate_api_gate_negative_control() {
    // The aud-less Go-parity mint is now an explicit control, not an accident
    // of the login path: it verifies cryptographically and is accepted by the
    // Session gate, but the strict API gate rejects it (`MissingAudience`).
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let legacy = auth.create_token(&subject, "alice", true, now()).unwrap();
    let session = app(
        store.clone(),
        auth.clone(),
        AuthGate::Session,
        ResolutionConfig::default(),
    );
    assert_eq!(
        session
            .clone()
            .oneshot(bearer(&legacy))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "the legacy shape still authenticates the Session gate"
    );
    assert_eq!(
        auth.validate(&legacy).unwrap().aud,
        Vec::<String>::new(),
        "the control must really be aud-less"
    );

    let api = api_app(store.clone(), auth);
    assert_eq!(
        api.oneshot(bearer(&legacy)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "an aud-less token must never be accepted for API use (Part C #10)"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn nonexpiring_api_token_lacks_expiry_and_is_not_a_login_substitute() {
    // Pinning the forbidden substitution: `create_api_token` IS accepted by
    // the API gate but carries no expiry, so it cannot stand in for the login
    // mint. The login-shaped native token is native AND expiring.
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let api_token = auth
        .create_api_token(&subject, "alice", true, now())
        .unwrap();
    let claims = auth.validate(&api_token).unwrap();
    assert_eq!(claims.aud, vec!["native".to_string()]);
    assert_eq!(claims.exp, 0, "the API mint stays non-expiring");

    let api = api_app(store.clone(), auth.clone());
    assert_eq!(
        api.clone()
            .oneshot(bearer(&api_token))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let session_token = auth
        .create_native_session_token(&subject, "alice", true, now())
        .unwrap();
    let claims = auth.validate(&session_token).unwrap();
    assert_eq!(claims.aud, vec!["native".to_string()]);
    assert_eq!(
        claims.exp,
        claims.iat + auth.session_timeout_secs(),
        "the session mint must carry the configured lifetime"
    );
    assert!(claims.exp > now());
    assert_ne!(
        session_token, api_token,
        "the login shape must be a distinct token, not the API mint"
    );
    assert_eq!(
        api.oneshot(bearer(&session_token)).await.unwrap().status(),
        StatusCode::OK
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn native_token_bound_to_a_different_uid_is_rejected_at_the_api_gate() {
    // Valid signature, native audience, matching sub and epoch — but a
    // different uid. The correct-uid twin proves the rejection came from the
    // uid revalidation rather than audience or a missing user.
    let (store, path, auth) = fixture();
    let mismatched = auth
        .create_native_session_token(
            &TokenSubject {
                id: "wrong-id".into(),
                token_epoch: 0,
            },
            "alice",
            true,
            now(),
        )
        .unwrap();
    let correct = auth
        .create_native_session_token(
            &TokenSubject {
                id: "user-id".into(),
                token_epoch: 0,
            },
            "alice",
            true,
            now(),
        )
        .unwrap();

    let api = api_app(store.clone(), auth);
    assert_eq!(
        api.clone()
            .oneshot(bearer(&mismatched))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
        "a native token bound to another id must fail revalidation"
    );
    assert_eq!(
        api.oneshot(bearer(&correct)).await.unwrap().status(),
        StatusCode::OK,
        "the only difference is the uid"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn native_api_gate_rejects_foreign_audience_and_present_wrong_issuer() {
    // Signed with the fixture's own secret so the only differences are the
    // claim under test. `exp` is left intact, so neither rejection can come
    // from expiry.
    let (store, path, auth) = fixture();
    let mint_now = now();
    let base = nd_auth::claims::Claims::for_user(
        &TokenSubject {
            id: "user-id".into(),
            token_epoch: 0,
        },
        "alice",
        true,
        Some(nd_auth::claims::AUDIENCE_NATIVE),
        mint_now,
    )
    .with_expires_at(mint_now + auth.session_timeout_secs());

    let mut foreign = base.clone();
    foreign.aud = vec!["subsonic".to_string()];
    let foreign = sign(&foreign);

    let mut wrong_issuer = base.clone();
    wrong_issuer.iss = "NOT-ND".to_string();
    let wrong_issuer = sign(&wrong_issuer);

    let valid = sign(&base);
    let api = api_app(store.clone(), auth);
    assert_eq!(
        api.clone().oneshot(bearer(&valid)).await.unwrap().status(),
        StatusCode::OK,
        "the valid twin must authenticate"
    );
    assert_eq!(
        api.clone()
            .oneshot(bearer(&foreign))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
        "a foreign audience must be rejected before the handler"
    );
    assert_eq!(
        api.oneshot(bearer(&wrong_issuer)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "a present wrong issuer must be rejected (an ABSENT issuer is not
        required by the current validator — that is a separate residual)"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn native_token_expiry_is_enforced_at_the_api_gate_with_safe_margins() {
    // The dependency comparator is strict-less with leeway 0
    // (`validation.rs:274-277`), so exp == the verifier's sampled second is
    // ACCEPTED. Only strictly-outside margins are asserted; no boundary claim,
    // no sleep, no injected clock. Both tokens differ solely in `exp`.
    let (store, path, auth) = fixture();
    let mint_now = now();
    let base = nd_auth::claims::Claims::for_user(
        &TokenSubject {
            id: "user-id".into(),
            token_epoch: 0,
        },
        "alice",
        true,
        Some(nd_auth::claims::AUDIENCE_NATIVE),
        mint_now,
    );

    let live = base.clone().with_expires_at(mint_now + 3600);
    let expired = base.clone().with_expires_at(mint_now - 3600);
    assert_eq!(
        live.exp - expired.exp,
        7200,
        "expiry is the only difference between the pair"
    );
    let live = sign(&live);
    let expired_token = sign(&expired);

    // Non-vacuity at the verifier level: same claims, only exp decides.
    assert!(auth.validate(&live).is_ok());
    assert_eq!(auth.validate(&expired_token), Err(AuthError::InvalidToken));

    // And the rejection is not observable in the handler at all: a counter
    // proves the expired request never entered it, rather than a boolean that
    // the live request already set.
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flag = entered.clone();
    let app = Router::new()
        .route(
            "/",
            get(move || {
                let flag = flag.clone();
                async move {
                    flag.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "ok"
                }
            }),
        )
        .layer(from_fn_with_state(
            MiddlewareState {
                store: store.clone(),
                auth,
                gate: AuthGate::Api,
                resolution: ResolutionConfig::default(),
            },
            authenticate,
        ));
    assert_eq!(
        app.clone().oneshot(bearer(&live)).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(bearer(&expired_token)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "a safely-past expiry must be rejected"
    );
    assert_eq!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only the live request entered the handler"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn refresh_preserves_native_audience_and_committed_epoch() {
    // Amend of `refresh_carries_committed_handler_epoch`: the input is a NATIVE
    // expiring token authenticated at the API gate, the epoch bump happens in a
    // real transaction that is COMMITTED before the holder is told, and the
    // refreshed token is then re-authenticated at a separate read-only API
    // route. That proves "refresh keeps aud=[native] and the committed epoch"
    // end-to-end instead of only decoding an epoch.
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth
        .create_native_session_token(&subject, "alice", true, now())
        .unwrap();
    let input_claims = auth.validate(&token).unwrap();

    let handler_store = store.clone();
    let hash = password::hash_password("new password").unwrap();
    let mutating = get(
        move |axum::Extension(holder): axum::Extension<TokenEpochHolder>| {
            let store = handler_store.clone();
            let hash = hash.clone();
            async move {
                // Commit first, then report — `record_put` must only ever see an
                // outcome from a transaction that actually committed.
                let outcome = store
                    .with_tx(move |tx| {
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
                        UserRepo::new(tx)
                            .put(&values, STAMP, true)
                            .map_err(|error| match error {
                                UserError::Sqlite(e) => e,
                                UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
                            })
                    })
                    .await
                    .unwrap();
                holder.record_put(&outcome);
                "ok"
            }
        },
    );
    let state = MiddlewareState {
        store: store.clone(),
        auth: auth.clone(),
        gate: AuthGate::Api,
        resolution: ResolutionConfig::default(),
    };
    let app = Router::new()
        .route("/", mutating)
        .layer(from_fn_with_state(auth.clone(), refresh))
        .layer(from_fn_with_state(state, authenticate));

    let response = app.oneshot(bearer(&token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let refreshed = response
        .headers()
        .get("x-nd-authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Claims: aud/iss/uid/sub/adm unchanged, epoch == the committed DB epoch,
    // a fresh positive lifetime, and iat left at the input's.
    let back = auth.validate(&refreshed).unwrap();
    assert_eq!(back.aud, vec!["native".to_string()]);
    assert_eq!(back.iss, "ND");
    assert_eq!(back.user_id, "user-id");
    assert_eq!(back.sub, "alice");
    assert!(back.is_admin);
    assert_eq!(back.epoch, 1, "the committed epoch must be carried");
    assert_eq!(back.iat, input_claims.iat, "iat is not re-stamped");
    // exp is re-stamped from the refresh's own clock: a fresh full lifetime,
    // never the input's stale one and never absent.
    let check_now = now();
    assert!(back.exp > check_now, "the refreshed token must be expiring");
    assert!(
        back.exp <= check_now + auth.session_timeout_secs() + 1,
        "the refreshed lifetime is the configured timeout, not an immortal claim"
    );

    // The DB really moved.
    let user = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
        .unwrap()
        .unwrap();
    assert_eq!(user.token_epoch, back.epoch);

    // The refreshed token authenticates a separate, non-mutating API route; the
    // now-stale input token does not.
    let api = api_app(store.clone(), auth.clone());
    let refreshed_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {refreshed}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.clone()
            .oneshot(refreshed_request)
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "the refreshed native token must re-authenticate the strict API"
    );
    assert_eq!(
        api.oneshot(bearer(&token)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "the pre-bump token is revoked by the committed epoch"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn rolled_back_password_change_does_not_publish_a_new_epoch() {
    // Makes "committed epoch" meaningful: the repository put runs inside a
    // transaction that is then forced to roll back, no outcome is recorded, and
    // the refresh keeps the input epoch — so the caller's token still works.
    let (store, path, auth) = fixture();
    let subject = TokenSubject {
        id: "user-id".into(),
        token_epoch: 0,
    };
    let token = auth
        .create_native_session_token(&subject, "alice", true, now())
        .unwrap();

    let handler_store = store.clone();
    let hash = password::hash_password("never committed").unwrap();
    let failing = get(
        move |axum::Extension(holder): axum::Extension<TokenEpochHolder>| {
            let store = handler_store.clone();
            let hash = hash.clone();
            async move {
                let outcome: Result<PutOutcome, _> = store
                    .with_tx(move |tx| {
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
                        let put: PutOutcome = UserRepo::new(tx).put(&values, STAMP, true).map_err(
                            |error| match error {
                                UserError::Sqlite(e) => e,
                                UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
                            },
                        )?;
                        // Fail after the put, so the transaction rolls back and the
                        // epoch bump is never committed.
                        let _ = put;
                        Err(rusqlite::Error::QueryReturnedNoRows)
                    })
                    .await;
                assert!(outcome.is_err(), "the transaction must have rolled back");
                // Nothing committed, so nothing was reported to the holder: the
                // refresh has no new epoch to publish.
                assert_eq!(holder.epoch(), None, "a rollback publishes no epoch");
                "error"
            }
        },
    );
    let state = MiddlewareState {
        store: store.clone(),
        auth: auth.clone(),
        gate: AuthGate::Api,
        resolution: ResolutionConfig::default(),
    };
    let app = Router::new()
        .route("/", failing)
        .layer(from_fn_with_state(auth.clone(), refresh))
        .layer(from_fn_with_state(state, authenticate));
    let response = app.oneshot(bearer(&token)).await.unwrap();
    let refreshed = response
        .headers()
        .get("x-nd-authorization")
        .map(|v| v.to_str().unwrap().to_string());

    // Stored password and epoch are untouched; any emitted refresh keeps epoch 0.
    let user = store
        .with_connection_sync(|conn| Ok(UserRepo::new(conn).find_by_username("alice")))
        .unwrap()
        .unwrap();
    assert_eq!(user.token_epoch, 0, "a rollback must not bump the epoch");
    assert!(password::verify_password("password", &user.password).unwrap());
    let refreshed = refreshed.expect("a buffered response still gets a refresh header");
    assert_eq!(auth.validate(&refreshed).unwrap().epoch, 0);

    let api = api_app(store.clone(), auth);
    let refreshed_request = Request::builder()
        .uri("/")
        .header("x-nd-authorization", format!("Bearer {refreshed}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        api.oneshot(refreshed_request).await.unwrap().status(),
        StatusCode::OK,
        "the refreshed token still authenticates at the untouched epoch"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
}
