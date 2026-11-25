//! Transport-derived peer extension for the HTTP shell (BOOT-03 shell scope).
//!
//! The mount contract requires the acceptor to record an immutable peer and to
//! **never** accept preinstalled header claims (`subplans/boot-api.md:249`).
//! This module only installs the transport peer into the request extensions so
//! `nd-auth`'s authentication middleware can find it
//! (`nd-auth/src/middleware.rs:78-81`); trusted-proxy resolution, effective
//! client-IP canonicalization, and CIDR/trustlist matching are **not**
//! implemented here and are explicitly not claimed (see the gaps in
//! `tests/listener.rs`).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;

use nd_auth::middleware::ProxyPeer;

/// The address the acceptor actually observed, as a numeric `host:port` string,
/// before any header is consulted. Kept separate from [`ProxyPeer`] so a later
/// trusted-proxy step can preserve the original peer while deriving a distinct
/// effective client IP (`subplans/boot-api.md:253`); overwriting the peer would
/// break ExtAuth trust and rate-limit bucketing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedPeer(pub String);

/// Layer that derives [`ProxyPeer`] from the transport's [`ConnectInfo`].
///
/// `ConnectInfo` is only present when the server was started through
/// `into_make_service_with_connect_info` (see [`crate::server`]); when it is
/// absent — for example an in-process router test — the request is passed on
/// **without** a peer rather than trusting a forwarded header. The installed
/// `ProxyPeer` string is the numeric address plus port exactly as the transport
/// reported it, so a client cannot influence it.
pub async fn record_peer(mut request: Request, next: Next) -> Response {
    // Copy the address out before touching the extensions mutably, and install
    // no peer when ConnectInfo is absent (never infer one from a header).
    let observed = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| AcceptedPeer(addr.to_string()));
    if let Some(peer) = observed {
        request.extensions_mut().insert(ProxyPeer(peer.0.clone()));
        request.extensions_mut().insert(peer);
    }
    next.run(request).await
}
