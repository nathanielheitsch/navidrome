//! Explicit route manifest, BasePath mounting, public ping, and the shell's own
//! unmatched-path receipt (BOOT-03 shell-only increment).
//!
//! Scope is deliberately narrow: a public ping, a BasePath-exact prefix, and the
//! 404 receipt. No protected mount, no authentication, no compression, no
//! CORS/security headers, no proxy trust, no static UI — all named in
//! `subplans/boot-api.md:249` but landing in later increments.

use axum::extract::{Extension, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;

/// The public heartbeat path, **relative to `BasePath`**.
///
/// Source: `server/server.go:177` installs `middleware.Heartbeat("/ping")` in
/// `defaultMiddlewares`; chi matches it against the request path the mux sees,
/// which already carries the `BasePath` prefix because every mount here is
/// `path.Join(conf.Server.BasePath, urlPath)` (`server/server.go:51`). So the
/// externally reachable ping path is `BasePath`-joined `/ping`.
pub const PING_PATH: &str = "/ping";

/// The UI app root relative to `BasePath` (`consts/consts.go:57`
/// `URLPathUI = "/app"`). Named because the manifest must be able to say what
/// this increment does **not** serve: static UI assets are explicitly excluded
/// (`subplans/boot-api.md:249` "No image throttle/compression/rendered
/// index/static UI in minimum shell").
pub const UI_PATH: &str = "/app";

/// One entry of the explicit route manifest (`subplans/boot-api.md:249`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Public heartbeat, answered ahead of the injected router, GET/HEAD only.
    Ping,
    /// The injected API router, mounted at the exact `BasePath` prefix.
    Injected {
        /// Exact mount prefix (slash-terminated, or empty for no base path).
        prefix: String,
    },
    /// Declared-removed path: answered by the shell's 404, never forwarded.
    ///
    /// **Wire status: canonical decision, not source.** The plan names
    /// `removedpaths404` (`subplans/boot-api.md:249`), but the Go server has no
    /// removed-path registry and no `410` handler (grep `removed`, `410`,
    /// `StatusGone` under `server/` finds nothing). So there is no native wire
    /// to capture: this increment answers a removed path exactly like an
    /// unmatched one — `404` with Go's `http.NotFound` body — inventing no new
    /// semantics. Whether removed paths should instead return `410` or an empty
    /// body is an open decision, recorded as a gap in `tests/listener.rs`.
    Removed {
        /// Path relative to `BasePath`.
        path: String,
    },
    /// Anything else: the shell's own unmatched-path receipt.
    Unmatched,
}

/// The manifest this shell enacts, given a base path. The removed-path list is
/// empty here: with no source registry to port (see [`Route::Removed`]),
/// inventing one is an open decision rather than sourced behavior, and
/// `match_path` keeps the removed arm exercised only by unit tests.
#[must_use]
pub fn manifest(base_path: &str) -> Vec<Route> {
    vec![
        Route::Ping,
        Route::Injected {
            prefix: base_prefix(base_path),
        },
        Route::Unmatched,
    ]
}

/// Why a configured transport mode cannot be served by this increment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedTransport {
    /// `tls_cert`/`tls_key` are set. Serving them is not implemented, and
    /// quietly binding insecure TCP instead would be a downgrade
    /// (`subplans/boot-api.md:249`).
    TlsConfigured {
        /// Configured certificate path, echoed for operator diagnosis.
        cert: String,
        /// Configured key path.
        key: String,
    },
    /// `address` starts with `unix:`. No Unix listener exists yet, and one must
    /// verify socket ownership/type/mode before replacing any path
    /// (`subplans/boot-api.md:249`); no unlink is performed here.
    UnixConfigured {
        /// The configured `unix:`-prefixed address.
        address: String,
    },
}

/// Errors that must be reported **before** a socket is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// The configured address/port is not a numeric socket address.
    InvalidAddress {
        /// Address as configured.
        address: String,
        /// Port as configured.
        port: i64,
    },
    /// A configured TLS or Unix mode this increment refuses to downgrade.
    Unsupported(UnsupportedTransport),
    /// The OS refused the bind.
    Io {
        /// Address actually passed to the bind.
        address: String,
        /// OS error text.
        reason: String,
    },
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::InvalidAddress { address, port } => {
                write!(f, "invalid bind address {address}:{port}")
            }
            BindError::Unsupported(UnsupportedTransport::TlsConfigured { cert, key }) => write!(
                f,
                "TLS is configured (cert {cert:?}, key {key:?}) but not implemented: refusing to \
                 serve insecure TCP instead (TLS is an approved follow-up)"
            ),
            BindError::Unsupported(UnsupportedTransport::UnixConfigured { address }) => write!(
                f,
                "Unix socket transport is configured ({address:?}) but not implemented: refusing \
                 to bind TCP under a Unix address"
            ),
            BindError::Io { address, reason } => {
                write!(f, "creating tcp listener on {address}: {reason}")
            }
        }
    }
}

impl std::error::Error for BindError {}

/// Outcome of matching a request path against the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Serve the public ping.
    Ping,
    /// Strip `BasePath` and hand the remainder to the injected router.
    Injected {
        /// Path with the prefix removed, always beginning with `/`.
        remainder: String,
    },
    /// Answer the shell's unmatched-path receipt.
    NotFound {
        /// Whether the path was declared removed.
        removed: bool,
    },
}

/// Normalize a configured `BasePath` into a mount prefix.
///
/// Go builds mounts with `path.Join(conf.Server.BasePath, urlPath)`: empty
/// contributes nothing, duplicate slashes collapse, trailing slash drops. The
/// result is `""` for no base path, otherwise a single-leading-slash,
/// **slash-terminated** prefix such as `"/nd/"`. The terminator is what makes
/// prefix matching exact rather than confusable.
#[must_use]
pub fn base_prefix(base_path: &str) -> String {
    let trimmed = base_path.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}/")
    }
}

/// Exact BasePath containment: the path equals the prefix (with or without its
/// terminating slash) or starts with the terminated prefix.
///
/// Deliberately **not** prefix-confusable: a naive `starts_with("/nd")` would
/// let `/ndx` or `/ndother` be treated as mounted under `/nd`.
#[must_use]
pub fn is_under_base(base_path: &str, path: &str) -> bool {
    let prefix = base_prefix(base_path);
    if prefix.is_empty() {
        return true;
    }
    path == prefix.trim_end_matches('/') || path.starts_with(&prefix)
}

/// Remove the `BasePath` prefix from a path already known to be under it,
/// yielding the path the injected router actually sees.
///
/// The remainder always begins with `/`: axum's `Router::nest` re-adds the
/// leading slash it consumed (verified over a real socket in
/// `tests/listener.rs`, where the injected handler reports `/nothing-here` for a
/// request to `/nd/nothing-here`), and a path equal to the base without its
/// terminating slash yields `"/"`. Keeping the model aligned with that
/// behaviour is the point — a bare `strip_prefix` would report `nothing-here`
/// and drift from what the injected router receives.
#[must_use]
pub fn strip_base(base_path: &str, path: &str) -> String {
    let prefix = base_prefix(base_path);
    if prefix.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(&prefix) {
        // Already slash-led.
        Some(rest) if rest.starts_with('/') => rest.to_string(),
        // Nest re-adds the slash the prefix consumed (`/nd/ping` -> `/ping`),
        // and a path equal to the base with no terminating slash -> `/`.
        Some(rest) => format!("/{rest}"),
        None => "/".to_string(),
    }
}

/// Match a request path against the shell manifest, in the order the shell
/// actually applies (see [`build_shell`] layer order):
///
/// 1. Literal `/ping` (case-insensitive, `strings.EqualFold`,
///    `chi@v5.3.2/middleware/heartbeat.go:15`) → public heartbeat, evaluated on
///    the **unstripped** path, so it answers even when `BasePath` is set and
///    `BasePath`-joined `/nd/ping` is not a heartbeat.
/// 2. Outside `BasePath` → unmatched. Go mounts nothing above the base path
///    (`server/server.go:51`, `:192`, `:206`, `:227`), so a request there is a
///    404 and must not reach the injected router.
/// 3. A declared-removed path → the shell's 404 (see [`Route::Removed`]).
/// 4. Everything else under `BasePath` → the injected router.
#[must_use]
pub fn match_path(base_path: &str, path: &str, removed: &[&str]) -> Match {
    if is_ping_path(path) {
        return Match::Ping;
    }
    if !is_under_base(base_path, path) {
        return Match::NotFound { removed: false };
    }
    let remainder = strip_base(base_path, path);
    let is_removed = removed.iter().any(|gone| {
        let gone = base_prefix(gone).trim_end_matches('/').to_string();
        remainder == gone || remainder.starts_with(&format!("{gone}/"))
    });
    if is_removed {
        return Match::NotFound { removed: true };
    }
    Match::Injected { remainder }
}

/// The public ping response: `200`, `Content-Type: text/plain`, body `.`.
///
/// Byte-for-byte the Go heartbeat (`chi@v5.3.2/middleware/heartbeat.go:14-21`:
/// `Set("Content-Type", "text/plain")`, `WriteHeader(200)`, `Write([]byte("."))`).
/// It reads no database, configuration value, or filesystem, so it discloses no
/// server state (`subplans/boot-api.md:257`).
#[must_use]
pub fn ping_response() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
        ".",
    )
        .into_response()
}

/// The shell's unmatched-path receipt, for paths that never reach the injected
/// router (outside `BasePath`, or declared removed).
///
/// Go's default is `http.NotFound` (`net/http/server.go`), which writes `404`
/// with `Content-Type: text/plain; charset=utf-8` and body
/// `404 page not found\n`. axum's default fallback returns `404` with an
/// **empty** body and no `Content-Type`, a wire divergence, so unmatched paths
/// are answered explicitly here.
#[must_use]
pub fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "404 page not found\n",
    )
        .into_response()
}

/// Whether a request path is the public heartbeat.
///
/// **Source-exact:** chi's `Heartbeat(endpoint)` matches
/// `strings.EqualFold(r.URL.Path, endpoint)` (`chi@v5.3.2/middleware/
/// heartbeat.go:15`) against the path **as it arrives on the top-level mux**.
/// Navidrome installs it in `defaultMiddlewares` on the root router
/// (`server/server.go:171-184`), and chi only rewrites `rctx.RoutePath` later,
/// during routing (`chi@v5.3.2/mux.go:447-462`), so nothing strips `BasePath`
/// before this comparison.
///
/// Consequence, reproduced rather than "fixed": the endpoint is the literal
/// `/ping`, so the heartbeat answers at `/ping` **even when `BasePath` is set**,
/// and `BasePath`-joined `/nd/ping` is *not* a heartbeat — it falls through to
/// the routes and 404s. Whether Navidrome *should* also answer `BasePath/ping`
/// is an open decision; this increment mirrors the source and pins both cases in
/// `tests/listener.rs`.
#[must_use]
pub fn is_ping_path(path: &str) -> bool {
    path.eq_ignore_ascii_case(PING_PATH)
}

/// Short-circuit the public ping for GET/HEAD, mirroring chi's `Heartbeat`
/// (`chi@v5.3.2/middleware/heartbeat.go:13-22`). Other methods fall through —
/// Go's `POST /ping` is not handled by the heartbeat either. It reads no base
/// path, matching the source comparison exactly.
async fn ping_layer(request: Request, next: Next) -> Response {
    let method_is_heartbeat =
        request.method() == axum::http::Method::GET || request.method() == axum::http::Method::HEAD;
    if method_is_heartbeat && is_ping_path(request.uri().path()) {
        return ping_response();
    }
    next.run(request).await
}

/// Reject requests outside `BasePath` before the injected router runs.
async fn base_path_guard(
    Extension(base_path): Extension<String>,
    request: Request,
    next: Next,
) -> Response {
    if is_under_base(&base_path, request.uri().path()) {
        next.run(request).await
    } else {
        not_found_response()
    }
}

/// Build the shell router around an injected API router.
///
/// * `base_path` — committed `BasePath`; nothing is served above it.
/// * `injected` — the API router supplied by the caller, mounted at the exact
///   BasePath prefix and otherwise left untouched.
///
/// The shell owns only the **root 404**: a path outside `BasePath` is answered
/// with [`not_found_response`] and never reaches the injected router. The
/// unmatched-path wire *under* `BasePath` stays the injected router's own
/// fallback — in Go that is chi's `NotFound` (`http.NotFound`) — so the shell
/// does not rewrite injected semantics. If the injected router ships axum's
/// default fallback (empty body, no `Content-Type`), closing that divergence is
/// the API router's work, recorded as a gap in `tests/listener.rs`.
///
/// Layer order (outermost first): `ping_layer` → `Extension(BasePath)` →
/// `base_path_guard` → injected router. `nd-http` adds
/// [`crate::peer::record_peer`] outside this in [`crate::server::Running`], and
/// the ping sits outermost of the shell layers so it is evaluated on the
/// unstripped path before the BasePath guard can reject it — matching chi, where
/// `Heartbeat` runs ahead of routing (`server/server.go:177`).
///
/// No `#[must_use]` here: `axum::Router` is already must-use, so adding the
/// attribute would only trip `clippy::double_must_use`.
pub fn build_shell(base_path: &str, injected: Router) -> Router {
    let prefix = base_prefix(base_path);
    let body = if prefix.is_empty() {
        // No base path: every path is under the root, so there is no root 404
        // to own here and unmatched paths belong to the injected router.
        injected
    } else {
        Router::new()
            .fallback(axum::routing::any(|| async { not_found_response() }))
            .nest(&prefix, injected)
    };
    // Last-added layer is outermost. `Extension` is installed before the guard
    // reads it, and the ping is outermost of all three so a heartbeat is
    // answered on the raw path regardless of BasePath.
    body.layer(axum::middleware::from_fn(base_path_guard))
        .layer(axum::extract::Extension(base_path.to_string()))
        .layer(axum::middleware::from_fn(ping_layer))
}
