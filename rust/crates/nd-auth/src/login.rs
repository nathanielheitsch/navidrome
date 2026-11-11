//! Login and first-admin handlers (`server/auth.go:37-190`).
//!
//! Unlike Go's non-atomic count-then-insert (`server/auth.go:134-146` and
//! `:399-425`), the first-admin count and insert here share one retryable
//! SQLite transaction (`create_admin` below). This deliberately prevents two
//! simultaneous unauthenticated requests from creating separate admins.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nd_db::store::SQLStore;
use nd_db::user_repo::{UserError, UserPutValues, UserRecord, UserRepo};
use serde::{Deserialize, Serialize};

use crate::claims::TokenSubject;
use crate::credentials::{password, Authenticator};
use crate::guarded_limiter::{GuardedLimiter, GuardedLimiterError};
use crate::middleware::ProxyPeer;
use crate::trusted_sources::MAX_LOGIN_BODY_SIZE;

/// Shared login dependencies (`server/auth.go:37-68,126-158`).
#[derive(Clone)]
pub struct LoginState {
    store: SQLStore,
    auth: Authenticator,
    limiter: Arc<GuardedLimiter>,
    enable_gravatar: bool,
}

impl LoginState {
    /// Builds login state with a strictly positive limit (`server/server.go:202-221`,
    /// Part C #9 intentionally rejects Go's disabled-limiter configuration).
    ///
    /// # Errors
    /// Returns an error for a non-positive limit or unavailable limiter entropy.
    pub fn new(
        store: SQLStore,
        auth: Authenticator,
        limit: i64,
        window: Duration,
        enable_gravatar: bool,
    ) -> Result<Self, GuardedLimiterError> {
        Ok(Self {
            store,
            auth,
            limiter: Arc::new(GuardedLimiter::new(limit, window)?),
            enable_gravatar,
        })
    }
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginPayload {
    id: String,
    name: String,
    username: String,
    is_admin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
    token: String,
}

fn failed(status: StatusCode, text: &'static str) -> Response {
    (status, text).into_response()
}

async fn credentials(request: Request) -> Result<Credentials, StatusCode> {
    let bytes = to_bytes(request.into_body(), MAX_LOGIN_BODY_SIZE)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    Json::<Credentials>::from_bytes(&bytes)
        .map(|Json(value)| value)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)
}

async fn do_login(state: &LoginState, credentials: Credentials) -> Response {
    let found = state
        .store
        .with_connection(move |conn| {
            let repo = UserRepo::new(conn);
            let user = match repo.find_by_username(&credentials.username) {
                Ok(user) => user,
                Err(UserError::NotFound) => return Ok(Ok(None)),
                Err(error) => return Ok(Err(error)),
            };
            if !password::verify_password(&credentials.password, &user.password).unwrap_or(false) {
                return Ok(Ok(None));
            }
            if let Ok(now) =
                conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                    r.get::<_, String>(0)
                })
            {
                let _ = repo.update_last_login_at(&user.id, &now);
            }
            Ok(Ok(Some(user)))
        })
        .await;
    match found {
        Ok(Ok(Some(user))) => payload(state, user),
        Ok(Ok(None)) => failed(StatusCode::UNAUTHORIZED, "Invalid username or password"),
        _ => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unknown error authenticating user",
        ),
    }
}

fn payload(state: &LoginState, user: UserRecord) -> Response {
    let now = match SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
    {
        Some(now) => now,
        None => {
            return failed(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unknown error authenticating user",
            )
        }
    };
    let subject = TokenSubject {
        id: user.id.clone(),
        token_epoch: user.token_epoch,
    };
    let token = match state
        .auth
        .create_token(&subject, &user.user_name, user.is_admin, now)
    {
        Ok(token) => token,
        Err(_) => {
            return failed(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unknown error authenticating user",
            )
        }
    };
    let avatar = (state.enable_gravatar && !user.email.is_empty())
        .then(|| nd_utils::gravatar::url(&user.email, 50));
    Json(LoginPayload {
        id: user.id,
        name: user.name,
        username: user.user_name,
        is_admin: user.is_admin,
        avatar,
        token,
    })
    .into_response()
}

fn peer(request: &Request) -> String {
    request
        .extensions()
        .get::<ProxyPeer>()
        .map_or_else(|| "unknown".into(), |p| p.0.clone())
}

/// Authenticates a JSON username/password (`server/auth.go:37-68,161-177`).
/// The request's transport peer should be supplied as [`ProxyPeer`].
pub async fn login(State(state): State<LoginState>, request: Request) -> Response {
    let key = peer(&request);
    let Ok(slot) = state.limiter.acquire(&key) else {
        return failed(StatusCode::TOO_MANY_REQUESTS, "Too many attempts");
    };
    let response = match credentials(request).await {
        Ok(credentials) => do_login(&state, credentials).await,
        Err(status) => failed(status, "invalid request payload"),
    };
    slot.release(response.status().is_client_error());
    response
}

fn sql_error(error: UserError) -> rusqlite::Error {
    match error {
        UserError::Sqlite(error) => error,
        UserError::NotFound => rusqlite::Error::QueryReturnedNoRows,
    }
}

/// Creates the initial admin only in an empty database (`server/auth.go:126-158`).
/// Count and insert are transactional, deliberately unlike the Go oracle.
pub async fn create_admin(State(state): State<LoginState>, request: Request) -> Response {
    let key = peer(&request);
    let Ok(slot) = state.limiter.acquire(&key) else {
        return failed(StatusCode::TOO_MANY_REQUESTS, "Too many attempts");
    };
    let response = create_admin_inner(&state, request).await;
    slot.release(response.status().is_client_error());
    response
}

async fn create_admin_inner(state: &LoginState, request: Request) -> Response {
    let input = match credentials(request).await {
        Ok(input) => input,
        Err(status) => return failed(status, "invalid request payload"),
    };
    let cleartext = input.password.clone();
    let hash = match state
        .store
        .with_connection(move |_| Ok(password::hash_password(&cleartext)))
        .await
    {
        Ok(Ok(hash)) => hash,
        _ => return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot create admin"),
    };
    let username = input.username.clone();
    let created = state
        .store
        .with_tx_retry(move |tx| {
            let repo = UserRepo::new(tx);
            if repo.count_all().map_err(sql_error)? > 0 {
                return Ok(false);
            }
            let now: String =
                tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                    r.get(0)
                })?;
            let values = UserPutValues {
                id: "",
                user_name: &username,
                name: &username,
                email: "",
                password: Some(&hash),
                is_admin: true,
                last_login_at: Some(&now),
                last_access_at: None,
                created_at: &now,
                updated_at: &now,
                scrobble_filter: "",
            };
            repo.put(&values, &now, false).map_err(sql_error)?;
            Ok(true)
        })
        .await;
    match created {
        Ok(true) => do_login(state, input).await,
        Ok(false) => failed(StatusCode::FORBIDDEN, "Cannot create another first admin"),
        Err(_) => failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot create admin"),
    }
}

/// Dependencies for trusted-header auto-provisioning (`server/auth.go:371-415`).
#[derive(Clone)]
pub struct HeaderLoginState {
    /// Login dependencies, including the fail-closed limiter.
    pub login: LoginState,
    /// Config and trusted-proxy resolution rules.
    pub resolution: crate::verifier::ResolutionConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeaderPayload {
    id: String,
    name: String,
    username: String,
    is_admin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

/// Authenticates a configured or trusted-header user, creating the user if
/// absent (`server/auth.go:371-415`). First-user admin selection is performed
/// in a transaction, unlike Go's non-atomic count and insert at `:390-407`.
pub async fn login_from_headers(
    State(state): State<HeaderLoginState>,
    request: Request,
) -> Response {
    let peer = peer(&request);
    let Some(username) = crate::verifier::header_username(
        &state.resolution,
        request.headers(),
        request
            .extensions()
            .get::<ProxyPeer>()
            .map(|p| p.0.as_str()),
    ) else {
        return failed(StatusCode::UNAUTHORIZED, "Not authenticated");
    };
    let Ok(slot) = state.login.limiter.acquire(&peer) else {
        return failed(StatusCode::TOO_MANY_REQUESTS, "Too many attempts");
    };
    let response = header_login_inner(&state.login, username).await;
    slot.release(response.status().is_client_error());
    response
}

async fn header_login_inner(state: &LoginState, username: String) -> Response {
    let lookup = username.clone();
    let existing = state
        .store
        .with_connection(move |conn| Ok(UserRepo::new(conn).find_by_username(&lookup)))
        .await;
    let user = match existing {
        Ok(Ok(user)) => user,
        Ok(Err(UserError::NotFound)) => {
            let mut random = [0_u8; 32];
            if getrandom::getrandom(&mut random).is_err() {
                return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot create user");
            }
            let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let cleartext = format!("__NAVIDROME_AUTOGEN__{suffix}");
            let hash = match state
                .store
                .with_connection(move |_| Ok(password::hash_password(&cleartext)))
                .await
            {
                Ok(Ok(hash)) => hash,
                _ => return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot create user"),
            };
            let lookup = username.clone();
            let created = state
                .store
                .with_tx_retry(move |tx| {
                    let repo = UserRepo::new(tx);
                    match repo.find_by_username(&lookup) {
                        Ok(user) => return Ok(user),
                        Err(UserError::NotFound) => {}
                        Err(error) => return Err(sql_error(error)),
                    }
                    let is_admin = repo.count_all().map_err(sql_error)? == 0;
                    let now: String =
                        tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                            r.get(0)
                        })?;
                    let values = UserPutValues {
                        id: "",
                        user_name: &lookup,
                        name: &lookup,
                        email: "",
                        password: Some(&hash),
                        is_admin,
                        last_login_at: Some(&now),
                        last_access_at: None,
                        created_at: &now,
                        updated_at: &now,
                        scrobble_filter: "",
                    };
                    repo.put(&values, &now, false).map_err(sql_error)?;
                    repo.find_by_username(&lookup).map_err(sql_error)
                })
                .await;
            match created {
                Ok(user) => user,
                Err(_) => return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot create user"),
            }
        }
        _ => return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot access user"),
    };
    let id = user.id.clone();
    let updated = state
        .store
        .with_connection(move |conn| {
            let now: String =
                conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                    r.get(0)
                })?;
            Ok(UserRepo::new(conn).update_last_login_at(&id, &now))
        })
        .await;
    if !matches!(updated, Ok(Ok(()))) {
        return failed(StatusCode::INTERNAL_SERVER_ERROR, "Cannot update login");
    }
    let avatar = (state.enable_gravatar && !user.email.is_empty())
        .then(|| nd_utils::gravatar::url(&user.email, 50));
    Json(HeaderPayload {
        id: user.id,
        name: user.name,
        username: user.user_name,
        is_admin: user.is_admin,
        avatar,
    })
    .into_response()
}
