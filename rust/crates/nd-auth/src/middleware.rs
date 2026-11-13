//! HTTP authentication over a leased user repository.
//!
//! Username selection follows `server/auth.go:258-278`; token checks apply
//! only when the verified token identifies that resolved user (`:296-306`).

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use nd_db::store::SQLStore;
use nd_db::user_repo::UserRepo;

use crate::claims::{Claims, TokenSubject, AUDIENCE_NATIVE};
use crate::credentials::Authenticator;
use crate::verifier::{resolve_username, ResolutionConfig};

/// The transport peer (`server/auth.go:214-218`). The server must install
/// this extension from the accepted connection, never from a request header.
#[derive(Clone, Debug)]
pub struct ProxyPeer(
    /// Peer address supplied by the transport.
    pub String,
);

/// An authenticated user without a password hash (`server/auth.go:246-255`).
#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    /// Stable user ID.
    pub id: String,
    /// Canonical username.
    pub username: String,
    /// Display name.
    pub name: String,
    /// Whether the user is an administrator.
    pub is_admin: bool,
    /// Current token revocation epoch.
    pub token_epoch: i64,
}

/// Signed claims retained for refresh (`server/auth.go:308-370`).
#[derive(Clone, Debug)]
pub struct VerifiedClaims(
    /// Signature-verified claims, additionally checked when identifying the user.
    pub Claims,
);

/// Audience policy for the authenticated route (`core/auth/auth.go:139-158`).
#[derive(Clone, Copy, Debug)]
pub enum AuthGate {
    /// UI session gate; audience may be empty.
    Session,
    /// API gate; `native` audience is mandatory (Part C #10).
    Api,
}

/// Dependencies for the authentication middleware (`server/auth.go:280-306`).
#[derive(Clone)]
pub struct MiddlewareState {
    /// Pooled executing user store.
    pub store: SQLStore,
    /// Signature verifier and claim checker.
    pub auth: Authenticator,
    /// Development and external authentication configuration.
    pub resolution: ResolutionConfig,
    /// Session or native API claim policy.
    pub gate: AuthGate,
}

/// Authenticates an Axum request (`server/auth.go:280-306`). Install via
/// `axum::middleware::from_fn_with_state`, and add [`ProxyPeer`] from the
/// transport before this middleware when ExtAuth is enabled.
pub async fn authenticate(
    State(state): State<MiddlewareState>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ProxyPeer>()
        .map(|p| p.0.as_str());
    let Some(resolution) = resolve_username(
        &state.auth,
        &state.resolution,
        request.headers(),
        request.uri(),
        peer,
    ) else {
        return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
    };
    let username = resolution.username;
    let result = state
        .store
        .with_connection(move |conn| Ok(UserRepo::new(conn).find_by_username(&username)))
        .await;
    let Ok(Ok(user)) = result else {
        return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
    };
    // Claim revalidation (`server/auth.go:249-255`).
    //
    // Go only consults the token when it happened to be the arm that won the
    // precedence walk. Here a token can only reach this point if it validated AND
    // agreed with every other supplied source, so whenever `claims` is present it
    // is the credential that authenticated the request, and it is always checked.
    //
    // The `sub` comparison below is defense-in-depth, NOT a reachable branch: after
    // A4's conflict detection `claims.sub` necessarily equals the username used for
    // the NOCASE lookup, so it cannot mismatch. It is kept as a cheap invariant
    // assertion in case resolution is ever relaxed again, and no test claims to
    // exercise it. The load-bearing A4 behavior lives in `verifier::resolve_inner`,
    // covered by the conflict tests there and by
    // `conflicting_auth_sources_are_rejected_not_silently_preferred`.
    if let Some(claims) = resolution.claims.as_ref() {
        if !claims.sub.eq_ignore_ascii_case(&user.user_name) {
            return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
        }
        let subject = TokenSubject {
            id: user.id.clone(),
            token_epoch: user.token_epoch,
        };
        let verdict = match state.gate {
            AuthGate::Session => {
                state
                    .auth
                    .check_session_claims(claims, &subject, &user.user_name, AUDIENCE_NATIVE)
            }
            AuthGate::Api => state
                .auth
                .check_api_claims(claims, &subject, &user.user_name),
        };
        if verdict.is_err() {
            return (StatusCode::UNAUTHORIZED, "Not authenticated").into_response();
        }
    }
    request.extensions_mut().insert(AuthenticatedUser {
        id: user.id,
        username: user.user_name,
        name: user.name,
        is_admin: user.is_admin,
        token_epoch: user.token_epoch,
    });
    if let Some(claims) = resolution.claims {
        request.extensions_mut().insert(VerifiedClaims(claims));
    }
    next.run(request).await
}
