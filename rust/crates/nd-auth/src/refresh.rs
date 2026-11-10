//! Session-token refresh (`server/auth.go:308-370`).
//!
//! Axum handlers return a complete response before its headers can be emitted.
//! Refresh immediately after the handler returns, which is the latest point
//! before headers are committed, including when the body is streamed. Epoch
//! updates made while subsequently polling a streaming body are too late, just
//! as they are after Go's first `WriteHeader`/`Write`/`Flush`.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

use crate::credentials::Authenticator;
use crate::http_context::TokenEpochHolder;
use crate::middleware::VerifiedClaims;
use crate::verifier::UI_AUTHORIZATION_HEADER;

/// Refreshes verified tokens after the handler and before response headers
/// are emitted (`server/auth.go:308-370`). Install inside the authentication
/// middleware so it receives [`VerifiedClaims`]. A handler may report a
/// committed password change through [`TokenEpochHolder::record_put`].
pub async fn refresh(
    State(auth): State<Authenticator>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(VerifiedClaims(mut claims)) = request.extensions().get::<VerifiedClaims>().cloned()
    else {
        return next.run(request).await;
    };
    let holder = TokenEpochHolder::default();
    request.extensions_mut().insert(holder.clone());
    let mut response = next.run(request).await;
    if let Some(epoch) = holder.epoch() {
        claims.epoch = epoch;
    }
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        if let Ok(now) = i64::try_from(now.as_secs()) {
            if let Ok(token) = auth.touch_claims(&claims, now) {
                if let Ok(value) = HeaderValue::from_str(&token) {
                    response
                        .headers_mut()
                        .insert(UI_AUTHORIZATION_HEADER, value);
                }
            }
        }
    }
    response
}
