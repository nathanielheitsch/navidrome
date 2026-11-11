//! Session-token refresh (`server/auth.go:308-370`).
//!
//! Go's `refreshingWriter` sets the header on the first write during the
//! handler (`server/auth.go:308-337`), whereas Axum fixes headers when the
//! response returns. For ordinary nonempty bodies the client observes the
//! same refreshed header. Empty and unknown-length responses are left
//! untouched. An exact-length streaming body is indistinguishable from a
//! buffered body here; an epoch bump during its polling is not propagated.
//! SSE and other unknown-length streams therefore receive no refresh header.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::HttpBody;
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
    // Unknown-length bodies may perform work after the headers are committed.
    // Do not advertise a refresh that could miss a mid-stream epoch bump.
    if !matches!(response.body().size_hint().exact(), Some(size) if size > 0) {
        return response;
    }
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
