//! BOOT-03 shell-only increment: real loopback TCP lifecycle and shell wire.
//!
//! Every request here goes over a real socket to a real bound listener — a
//! mocked `Router::oneshot` would not satisfy this acceptance
//! (`subplans/boot-api.md:257` "Non-vacuity: main must actually accept loopback
//! requests"). The client is a hand-written HTTP/1.1 request over
//! `tokio::net::TcpStream`, so no new dependency is added (reqwest is not in
//! `Cargo.lock` at all, and adding one is out of scope).
//!
//! Loopback only (`127.0.0.1`). No non-loopback bind, no TLS, no Unix socket,
//! no real config files, no credentials. Every wait is bounded by
//! `tokio::time::timeout`; there is no startup sleep anywhere.
//!
//! ## Gaps this increment does not close (reported, not invented)
//!
//! * **removedpaths404 wire**: no source exists (`Route::Removed`), so removed
//!   paths get the same `404`/`http.NotFound` body as unmatched paths. Choosing
//!   `410` or an empty body is an open decision.
//! * **Protected mount, authentication, CORS/security headers, compression,
//!   request ID, proxy trust, effective client IP, static UI, TLS, Unix**: all
//!   named by the plan, none implemented here.
//! * **In-flight drain observability**: `ShutdownOutcome::DeadlineElapsed` says
//!   only that the accept task was still running when the deadline elapsed.
//!   There is no count of unfinished requests — API-04 owns that.
//! * **Hostname bind addresses**: only numeric literals are bound; Go's
//!   `net.Listen` also resolves names.
//! * **Signal selection** (`SIGINT`/`SIGTERM`) belongs to `nd-cli` (BOOT-04);
//!   these tests drive the shutdown handle directly.

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::{Extension, Request};
use axum::response::IntoResponse;
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;

use nd_http::peer::AcceptedPeer;
use nd_http::shell::{Match, Route};
use nd_http::{BindError, BoundListener, Running, Shutdown, ShutdownOutcome, UnsupportedTransport};

/// Generous upper bound for a local socket exchange: a pass should take
/// milliseconds, so hitting this is a real failure, and it bounds the test.
const EXCHANGE_DEADLINE: Duration = Duration::from_millis(2_000);

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client
// ---------------------------------------------------------------------------

/// A parsed response: status, lower-cased header map, and exact body bytes.
struct Reply {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn body_str(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("<non-utf8 body>")
    }
}

/// Send `request` over a real loopback connection and read the whole reply.
///
/// Requests in this file carry `Connection: close`, so reading to EOF is the
/// complete response. If the deadline elapses the helper panics with the bytes
/// received so far, which distinguishes "never answered" from "answered wrong".
async fn exchange(addr: SocketAddr, request: &str) -> Reply {
    let reply = tokio::time::timeout(EXCHANGE_DEADLINE, exchange_inner(addr, request))
        .await
        .unwrap_or_else(|_| panic!("no reply within {EXCHANGE_DEADLINE:?} for {request:?}"));
    reply
}

async fn exchange_inner(addr: SocketAddr, request: &str) -> Reply {
    let mut stream = TcpStream::connect(addr)
        .await
        .unwrap_or_else(|err| panic!("connect to {addr}: {err}"));
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|err| panic!("write request: {err}"));
    stream.flush().await.expect("flush request");

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .unwrap_or_else(|err| panic!("read reply: {err}"));

    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| {
            panic!(
                "response has no header terminator: {:?}",
                String::from_utf8_lossy(&buf)
            )
        });
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let body = buf[split + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let mut parts = status_line.splitn(2, ' ');
    let _version = parts.next().expect("HTTP version");
    let status: u16 = parts
        .next()
        .expect("status code")
        .split(' ')
        .next()
        .expect("status code digits")
        .parse()
        .expect("status code parses");
    let reason = status_line.splitn(3, ' ').nth(2).unwrap_or("").to_string();

    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    Reply {
        status,
        reason,
        headers,
        body,
    }
}

/// A plain GET with `Connection: close`.
fn get(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
}

fn method(path: &str, method: &str, extra_headers: &str) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra_headers}\r\n"
    )
}

// ---------------------------------------------------------------------------
// Routers under test
// ---------------------------------------------------------------------------

/// Injected router that proves (a) a request reached it and (b) which peer the
/// transport recorded. `GET /echo` replies with the observed peer; `GET /api/song`
/// replies with a body no ping could ever produce.
fn injected_router(entered: Option<watch::Sender<bool>>) -> Router {
    Router::new()
        .route("/echo", axum::routing::get(echo_peer))
        .route(
            "/api/song",
            axum::routing::get(|| async { r#"{"id":"1","title":"x"}"# }),
        )
        .fallback(axum::routing::any(move |req: Request| {
            let entered = entered.clone();
            async move {
                if let Some(tx) = entered {
                    let _ = tx.send(true);
                }
                // Echo the method and path so the test can distinguish "fell
                // through to the injected router" from "answered by the shell".
                (
                    axum::http::StatusCode::NOT_FOUND,
                    format!("injected:{} {}", req.method(), req.uri().path()),
                )
                    .into_response()
            }
        }))
}

async fn echo_peer(peer: Option<Extension<AcceptedPeer>>) -> String {
    match peer {
        Some(Extension(peer)) => format!("peer={}", peer.0),
        None => "peer=none".to_string(),
    }
}

async fn bind_and_serve(base_path: &str, router: Router) -> (Running, SocketAddr) {
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind loopback ephemeral port");
    let addr = listener.local_addr();
    let running =
        Running::start(listener, router, Shutdown::new(), base_path).expect("start serve");
    assert_eq!(
        running.local_addr(),
        addr,
        "ready address is the bound address"
    );
    (running, addr)
}

// ---------------------------------------------------------------------------
// Public ping — sourced wire
// ---------------------------------------------------------------------------

/// The ping is answered over a real socket with chi's exact bytes: `200`,
/// `Content-Type: text/plain`, body `.` (`chi@v5.3.2/middleware/heartbeat.go:14-21`).
#[tokio::test]
async fn ping_serves_source_wire_over_real_socket() {
    let (running, addr) = bind_and_serve("", injected_router(None)).await;

    let reply = exchange(addr, &get("/ping")).await;
    assert_eq!(reply.status, 200, "ping status; reason {:?}", reply.reason);
    assert_eq!(reply.header("content-type"), Some("text/plain"));
    assert_eq!(reply.body_str(), ".", "ping body is exactly `.`");

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// The ping discloses no server state: no DB, no config, no version. It also
/// never reaches the injected router, so no handler-side body can appear.
#[tokio::test]
async fn ping_discloses_no_state_and_never_reaches_the_injected_router() {
    let (tx, rx) = watch::channel(false);
    let (running, addr) = bind_and_serve("", injected_router(Some(tx.clone()))).await;

    let reply = exchange(addr, &get("/ping")).await;
    assert_eq!(
        reply.body, b".",
        "ping body is fixed bytes, so nothing about the server can leak into it"
    );
    // The ping carries no server-identifying headers: no server name/version, no
    // Location, no Set-Cookie, no auth challenge. Transport framing
    // (`content-length`, `date`, `connection`) is expected and harmless.
    let named: Vec<&str> = reply.headers.iter().map(|(k, _)| k.as_str()).collect();
    for forbidden in [
        "server",
        "location",
        "set-cookie",
        "www-authenticate",
        "x-nd-authorization",
    ] {
        assert!(
            !named.contains(&forbidden),
            "ping must not disclose server state via {forbidden}: {named:?}"
        );
    }
    assert!(!reply.body_str().contains("injected:"));
    assert!(!*rx.borrow(), "the injected router never ran for /ping");

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// With a BasePath, the heartbeat stays at the literal `/ping` — it is matched
/// on the unstripped path ahead of routing, exactly as chi does — and
/// `BasePath`-joined `/nd/ping` is **not** a ping. The second assertion is the
/// source-derived consequence, not a "fixed" behavior.
#[tokio::test]
async fn ping_is_literal_and_answers_regardless_of_basepath() {
    let (running, addr) = bind_and_serve("/nd", injected_router(None)).await;

    let literal = exchange(addr, &get("/ping")).await;
    assert_eq!(literal.status, 200, "/ping answers even with BasePath set");
    assert_eq!(literal.body_str(), ".");

    let joined = exchange(addr, &get("/nd/ping")).await;
    assert_eq!(
        joined.status,
        404,
        "BasePath-joined /nd/ping is not the literal heartbeat: {:?}",
        joined.body_str()
    );
    assert!(
        joined.body_str().contains("injected:") || joined.body_str() == "404 page not found\n",
        "/nd/ping falls through to routing: {:?}",
        joined.body_str()
    );

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// Ping matching follows chi: GET and HEAD only, path case-insensitive
/// (`strings.EqualFold`), and a sibling path is not a ping.
#[tokio::test]
async fn ping_method_and_case_follow_the_source() {
    let (running, addr) = bind_and_serve("", injected_router(None)).await;

    let head = exchange(addr, &method("/ping", "HEAD", "")).await;
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-type"), Some("text/plain"));

    let upper = exchange(addr, &get("/PING")).await;
    assert_eq!(upper.status, 200, "EqualFold on the path");
    assert_eq!(upper.body_str(), ".");

    let mixed = exchange(addr, &get("/PiNg")).await;
    assert_eq!(mixed.status, 200);

    let post = exchange(addr, &method("/ping", "POST", "")).await;
    assert_eq!(
        post.status,
        404,
        "POST /ping is not a heartbeat, so it falls through and is unmatched: {:?}",
        post.body_str()
    );

    let sibling = exchange(addr, &get("/pingx")).await;
    assert_eq!(sibling.status, 404, "/pingx is not the heartbeat");

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

// ---------------------------------------------------------------------------
// BasePath: exact prefix, not prefix-confusable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn basepath_prefix_normalization() {
    assert_eq!(nd_http::shell::base_prefix(""), "");
    assert_eq!(nd_http::shell::base_prefix("/"), "");
    assert_eq!(nd_http::shell::base_prefix("//"), "");
    assert_eq!(nd_http::shell::base_prefix("nd"), "/nd/");
    assert_eq!(nd_http::shell::base_prefix("/nd"), "/nd/");
    assert_eq!(nd_http::shell::base_prefix("/nd/"), "/nd/");
    assert_eq!(nd_http::shell::base_prefix("/a/b"), "/a/b/");
}

/// The manifest's own match table, including the confusable siblings and the
/// literal-ping consequence of chi's unstripped comparison.
#[tokio::test]
async fn basepath_exact_match_table() {
    // The heartbeat is the literal path, independent of BasePath.
    assert_eq!(nd_http::shell::match_path("/nd", "/ping", &[]), Match::Ping);
    assert_eq!(nd_http::shell::match_path("", "/ping", &[]), Match::Ping);
    assert_eq!(nd_http::shell::match_path("/nd", "/PING", &[]), Match::Ping);
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/ping", &[]),
        Match::Injected {
            remainder: "/ping".to_string()
        },
        "BasePath-joined /nd/ping is not the literal heartbeat"
    );

    // Under the base path.
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/", &[]),
        Match::Injected {
            remainder: "/".to_string()
        }
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/api/song", &[]),
        Match::Injected {
            remainder: "/api/song".to_string()
        }
    );
    // Base path itself with no trailing slash: `/` remainder, never a panic.
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd", &[]),
        Match::Injected {
            remainder: "/".to_string()
        }
    );

    // NOT under the base path — these are the prefix-confusable siblings.
    assert_eq!(
        nd_http::shell::match_path("/nd", "/ndx/api", &[]),
        Match::NotFound { removed: false }
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/ndother/ping", &[]),
        Match::NotFound { removed: false }
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/api", &[]),
        Match::NotFound { removed: false },
        "nothing but the ping is served above BasePath"
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/", &[]),
        Match::NotFound { removed: false }
    );

    // Declared-removed paths (canonical decision: same 404 receipt, see gaps).
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/scans", &["/scans"]),
        Match::NotFound { removed: true }
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/scans/1", &["/scans"]),
        Match::NotFound { removed: true }
    );
    assert_eq!(
        nd_http::shell::match_path("/nd", "/nd/scansx", &["/scans"]),
        Match::Injected {
            remainder: "/scansx".to_string()
        },
        "removed matching is also exact, not prefix-confusable"
    );
}

#[tokio::test]
async fn manifest_names_what_this_shell_enacts() {
    let routes = nd_http::shell::manifest("/nd");
    assert_eq!(
        routes,
        vec![
            Route::Ping,
            Route::Injected {
                prefix: "/nd/".to_string()
            },
            Route::Unmatched,
        ]
    );
}

/// With a base path, the ping and the injected router are reachable only under
/// it, and a sibling mount gets the shell's 404 rather than the router.
#[tokio::test]
async fn basepath_mount_is_exact_over_real_socket() {
    let (running, addr) = bind_and_serve("/nd", injected_router(None)).await;

    // `/ping` is the literal heartbeat and answers above the base path too.
    let ping = exchange(addr, &get("/ping")).await;
    assert_eq!(ping.status, 200, "literal /ping answers with BasePath set");
    assert_eq!(ping.body_str(), ".");

    // BasePath-joined /nd/ping is NOT the heartbeat (see the dedicated test).
    let joined = exchange(addr, &get("/nd/ping")).await;
    assert_ne!(
        joined.body_str(),
        ".",
        "/nd/ping is not answered as a heartbeat"
    );

    let api = exchange(addr, &get("/nd/api/song")).await;
    assert_eq!(api.status, 200);
    assert_eq!(api.body_str(), r#"{"id":"1","title":"x"}"#);

    let echo = exchange(addr, &get("/nd/echo")).await;
    assert_eq!(echo.status, 200);
    assert!(echo.body_str().starts_with("peer=127.0.0.1:"));

    // Above the base path (other than the literal ping): 404 with Go's
    // `http.NotFound` wire, and the injected router must not have run (its
    // fallback body would say "injected:").
    for path in ["/api/song", "/echo", "/app/assets.js"] {
        let reply = exchange(addr, &get(path)).await;
        assert_eq!(reply.status, 404, "{path} is above BasePath");
        assert_eq!(reply.body_str(), "404 page not found\n");
        assert_eq!(
            reply.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
    }

    // Prefix-confusable siblings.
    for path in ["/ndx/ping", "/ndother/ping", "/ndapi/song"] {
        let reply = exchange(addr, &get(path)).await;
        assert_eq!(reply.status, 404, "{path} must not be served by /nd");
        assert!(
            !reply.body_str().contains("injected:"),
            "{path} must not reach the injected router: {:?}",
            reply.body_str()
        );
    }

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// Ownership split for unmatched paths, asserted over a real socket:
///
/// * Under `BasePath`, the **injected router's own** fallback answers — the shell
///   does not rewrite injected semantics. This also documents the open gap: if
///   the injected router ships axum's default fallback, its 404 has an empty
///   body and no `Content-Type`, whereas Go's chi default is `http.NotFound`
///   (`404 page not found\n` + `text/plain; charset=utf-8`). Closing that
///   belongs to the API router (`nd-api`), not to this shell.
/// * At and above `BasePath`, the shell owns the root 404 and reproduces Go's
///   `http.NotFound` wire exactly.
#[tokio::test]
async fn unmatched_paths_are_split_between_shell_and_injected_router() {
    let (tx, rx) = watch::channel(false);
    let (running, addr) = bind_and_serve("/nd", injected_router(Some(tx.clone()))).await;

    let under = exchange(addr, &get("/nd/nothing-here")).await;
    assert!(
        *rx.borrow(),
        "the request reached the injected router (non-vacuity of this split)"
    );
    assert_eq!(under.status, 404);
    assert_eq!(
        under.body_str(),
        "injected:GET /nothing-here",
        "the injected router's fallback answers under BasePath, with its path stripped"
    );

    let root = exchange(addr, &get("/nothing-here")).await;
    assert_eq!(root.status, 404);
    assert_eq!(
        root.body_str(),
        "404 page not found\n",
        "shell-owned root 404"
    );
    assert_eq!(
        root.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "axum's default fallback sends no Content-Type; Go's http.NotFound does"
    );
    assert!(
        !root.body_str().contains("injected:"),
        "the root 404 short-circuits before the injected router"
    );

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// axum's default fallback is a wire divergence from Go's chi default, asserted
/// over a real socket: an empty body and no `Content-Type`, where Go's
/// `http.NotFound` writes `404 page not found\n` with
/// `text/plain; charset=utf-8`. This is why the shell answers the root 404
/// itself. Closing the same divergence *under* BasePath requires the injected
/// router to carry the Go body — recorded as a gap, since the shell must not
/// rewrite injected semantics.
#[tokio::test]
async fn axum_default_fallback_diverges_from_go_http_not_found() {
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    // No base path and no routes: axum's own fallback answers everything.
    let running = Running::start(listener, Router::new(), Shutdown::new(), "").expect("start");

    let axum_404 = exchange(addr, &get("/anything")).await;
    assert_eq!(axum_404.status, 404);
    assert_eq!(
        axum_404.body_str(),
        "",
        "axum's default fallback body is empty"
    );
    assert_eq!(
        axum_404.header("content-type"),
        None,
        "axum's default fallback sends no Content-Type: {:?}",
        axum_404.headers
    );

    running.stop_and_join(EXCHANGE_DEADLINE).await;

    // The shell's receipt, over a socket with a base path so the root 404 is
    // shell-owned, matches Go exactly.
    let (running, addr) = bind_and_serve("/nd", Router::new()).await;
    let shell_404 = exchange(addr, &get("/anything")).await;
    assert_eq!(shell_404.status, 404);
    assert_eq!(shell_404.body_str(), "404 page not found\n");
    assert_eq!(
        shell_404.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

// ---------------------------------------------------------------------------
// Transport peer
// ---------------------------------------------------------------------------

/// The peer comes from the transport, and forwarded-IP headers cannot change it
/// (`subplans/boot-api.md:249` "never accept preinstalled header claims").
/// Trusted-proxy resolution itself is not implemented — see gaps.
#[tokio::test]
async fn peer_is_transport_derived_and_headers_cannot_override_it() {
    let (running, addr) = bind_and_serve("", injected_router(None)).await;

    let plain = exchange(addr, &get("/echo")).await;
    assert_eq!(plain.status, 200);
    let observed = plain.body_str().trim_start_matches("peer=").to_string();
    assert!(
        observed.starts_with("127.0.0.1:"),
        "peer is the loopback peer: {observed}"
    );

    // A client claiming to come from elsewhere changes nothing.
    let spoof = concat!(
        "GET /echo HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",
        "True-Client-IP: 8.8.8.8\r\nX-Real-IP: 9.9.9.9\r\n",
        "X-Forwarded-For: 7.7.7.7, 6.6.6.6\r\n\r\n"
    );
    let reply = exchange(addr, spoof).await;
    assert_eq!(reply.status, 200);
    let spoofed_peer = reply.body_str().trim_start_matches("peer=").to_string();
    assert!(
        spoofed_peer.starts_with("127.0.0.1:"),
        "the peer is still the transport peer: {spoofed_peer}"
    );
    // The claimed addresses appear nowhere in what the handler observed. The
    // client ports differ (two connections), which is expected and not a
    // header effect.
    for claimed in ["8.8.8.8", "9.9.9.9", "7.7.7.7", "6.6.6.6"] {
        assert!(
            !reply.body_str().contains(claimed),
            "{claimed} must not influence the recorded peer: {:?}",
            reply.body_str()
        );
    }

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// The peer string is the numeric address the transport reported, including the
/// client's ephemeral port, and it is stable for the life of one connection
/// (immutable per connection, which is what a later limiter keys on). Two
/// distinct connections get distinct peer strings.
#[tokio::test]
async fn peer_is_stable_per_connection_and_distinct_across_connections() {
    let (running, addr) = bind_and_serve("", injected_router(None)).await;

    // Same connection, two pipelined requests: identical peer both times.
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /echo HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write 1");
    stream.flush().await.expect("flush 1");
    let mut buf = Vec::new();
    read_until_body_end(&mut stream, &mut buf).await;
    let first_peer = peer_of(&buf);

    stream
        .write_all(b"GET /echo HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write 2");
    stream.flush().await.expect("flush 2");
    let mut rest = Vec::new();
    tokio::time::timeout(EXCHANGE_DEADLINE, stream.read_to_end(&mut rest))
        .await
        .expect("read bounded")
        .expect("read");
    let second_peer = peer_of(&rest);
    assert_eq!(
        first_peer, second_peer,
        "peer is immutable for one connection"
    );
    assert!(
        first_peer.starts_with("127.0.0.1:"),
        "loopback peer: {first_peer}"
    );

    // A separate connection has a different peer port.
    let other = exchange(addr, &get("/echo")).await;
    let other_peer = other.body_str().trim_start_matches("peer=").to_string();
    assert_ne!(
        first_peer, other_peer,
        "distinct connections: {first_peer} vs {other_peer}"
    );

    running.stop_and_join(EXCHANGE_DEADLINE).await;
}

/// Read one complete HTTP response (headers + `Content-Length` body) from a
/// keep-alive connection.
async fn read_until_body_end(stream: &mut TcpStream, buf: &mut Vec<u8>) {
    loop {
        let mut chunk = [0u8; 4096];
        let n = tokio::time::timeout(EXCHANGE_DEADLINE, stream.read(&mut chunk))
            .await
            .expect("read bounded")
            .expect("read");
        assert!(
            n > 0,
            "connection closed before a complete response: {:?}",
            String::from_utf8_lossy(buf)
        );
        buf.extend_from_slice(&chunk[..n]);
        if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let len: usize = head
                .split("content-length:")
                .nth(1)
                .and_then(|v| v.split('\r').next())
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + len {
                return;
            }
        }
    }
}

fn peer_of(buf: &[u8]) -> String {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let body = String::from_utf8_lossy(&buf[split + 4..]).to_string();
    body.trim_start_matches("peer=").trim().to_string()
}

// ---------------------------------------------------------------------------
// Bind-before-ready, actual address, bind failures, rebind
// ---------------------------------------------------------------------------

/// Port 0 exposes the address the OS actually bound, before any request is made.
#[tokio::test]
async fn ephemeral_bind_exposes_actual_address_before_ready() {
    let listener = BoundListener::bind("127.0.0.1", 0).expect("ephemeral bind");
    let addr = listener.local_addr();
    assert_ne!(addr.port(), 0, "the real port is exposed, not the request");
    assert_eq!(addr.ip().to_string(), "127.0.0.1");

    // Bound means connectable already: no startup wait, no 50ms proof.
    let stream = tokio::time::timeout(EXCHANGE_DEADLINE, TcpStream::connect(addr))
        .await
        .expect("listener accepts immediately after bind")
        .expect("connect to the bound socket");
    drop(stream);

    // Nothing was ever served: no Running exists, so "ready" was never claimed
    // for a socket that is not listening.
}

/// An occupied port yields a bind error, not a silent fallback to another port
/// and not a success.
#[tokio::test]
async fn occupied_port_yields_bind_error() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = held.local_addr().expect("held port").port();

    match BoundListener::bind("127.0.0.1", i64::from(port)) {
        Err(BindError::Io { address, reason }) => {
            assert_eq!(address, format!("127.0.0.1:{port}"));
            assert!(
                reason.contains("Address")
                    || reason.contains("address")
                    || reason.contains("in use"),
                "expected an address-in-use refusal, got {reason:?}"
            );
        }
        other => panic!("occupied port must fail the bind, got {other:?}"),
    }

    // The failed attempt did not steal or replace the held listener.
    let still_owned = std::net::TcpStream::connect(("127.0.0.1", port));
    assert!(
        still_owned.is_ok(),
        "the original holder still owns the port"
    );
}

/// Repeated bind → serve → shutdown cycles: no residual listener keeps the port,
/// and the same port can be taken again.
#[tokio::test]
async fn repeated_bind_shutdown_leaves_no_residual_listener() {
    for round in 0..3 {
        let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
        let addr = listener.local_addr();
        let running =
            Running::start(listener, injected_router(None), Shutdown::new(), "").expect("start");

        let reply = exchange(addr, &get("/ping")).await;
        assert_eq!(reply.status, 200, "round {round} serves");

        let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
        assert_eq!(outcome, ShutdownOutcome::Drained, "round {round} drains");

        // The port is free again: a fresh bind on the same port succeeds, which
        // would fail with address-in-use if a listener survived.
        let again = BoundListener::bind("127.0.0.1", i64::from(addr.port()))
            .unwrap_or_else(|err| panic!("round {round}: port {} still held: {err}", addr.port()));
        assert_eq!(again.local_addr(), addr);
        drop(again);
    }
}

// ---------------------------------------------------------------------------
// Explicit shutdown + join
// ---------------------------------------------------------------------------

/// Shutdown is driven from outside, the accept task is joined, and the outcome
/// is `Drained`. Afterwards the port refuses connections, proving accepts really
/// stopped.
#[tokio::test]
async fn graceful_shutdown_joins_and_stops_accepting() {
    let shutdown = Shutdown::new();
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    let running =
        Running::start(listener, injected_router(None), shutdown.clone(), "").expect("start");

    assert_eq!(exchange(addr, &get("/ping")).await.status, 200);
    assert!(!shutdown.signalled(), "not signalled while serving");

    shutdown.signal();
    assert!(shutdown.signalled());

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(outcome, ShutdownOutcome::Drained);
    assert!(outcome.is_success());

    let refused = tokio::time::timeout(EXCHANGE_DEADLINE, TcpStream::connect(addr))
        .await
        .expect("connect attempt is bounded");
    assert!(
        refused.is_err(),
        "no accepts after join: {:?}",
        refused.map(|s| s.peer_addr())
    );
}

/// Signalling shutdown *before* serving still wakes the acceptor immediately —
/// no lost wakeup, so a shutdown racing startup cannot leave a listener running.
#[tokio::test]
async fn shutdown_signal_before_serve_is_not_lost() {
    let shutdown = Shutdown::new();
    shutdown.signal();

    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let running = Running::start(listener, injected_router(None), shutdown, "").expect("start");

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(
        outcome,
        ShutdownOutcome::Drained,
        "an already-fired signal stops the loop without waiting"
    );
}

/// A response in flight when shutdown fires is still delivered, and the join
/// completes — "shutdown while read pending wakes both".
#[tokio::test]
async fn shutdown_with_a_pending_response_still_delivers_it() {
    let (tx, mut rx) = watch::channel(false);
    let router = Router::new().route(
        "/slow",
        axum::routing::get(move |req: Request| {
            let tx = tx.clone();
            async move {
                // Report entry, then finish the response normally.
                let _ = tx.send(true);
                format!("done {}", req.uri().path()).into_response()
            }
        }),
    );
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    let running = Running::start(listener, router, Shutdown::new(), "").expect("start");

    // Write the request without reading the reply, so a response is pending.
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write");
    stream.flush().await.expect("flush");

    tokio::time::timeout(EXCHANGE_DEADLINE, rx.wait_for(|v| *v))
        .await
        .expect("handler entered")
        .expect("watch open");

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(
        outcome,
        ShutdownOutcome::Drained,
        "in-flight response drains"
    );

    let mut buf = Vec::new();
    tokio::time::timeout(EXCHANGE_DEADLINE, stream.read_to_end(&mut buf))
        .await
        .expect("reply read is bounded")
        .expect("read");
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "pending reply delivered: {text:?}"
    );
    assert!(
        text.contains("done /slow"),
        "handler body present: {text:?}"
    );
}

/// When the acceptor is still running at the deadline the outcome is reported
/// **incomplete**, never as success. The unfinished request is left unfinished
/// on purpose: axum exposes no drain count, which is exactly the observability
/// gap API-04 owns.
#[tokio::test]
async fn deadline_elapsed_is_reported_incomplete_not_success() {
    let (tx, mut rx) = watch::channel(false);
    let router = Router::new().route(
        "/hang",
        axum::routing::get(move |req: Request| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(true);
                // Longer than the deadline below, so the join cannot observe a
                // completed serve future.
                tokio::time::sleep(Duration::from_millis(600)).await;
                format!("late {}", req.uri().path()).into_response()
            }
        }),
    );
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    let running = Running::start(listener, router, Shutdown::new(), "").expect("start");

    // Put a request in flight and wait (bounded) for the handler to enter.
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(b"GET /hang HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write");
    stream.flush().await.expect("flush");
    tokio::time::timeout(EXCHANGE_DEADLINE, rx.wait_for(|v| *v))
        .await
        .expect("handler entered")
        .expect("watch open");

    let outcome = running.stop_and_join(Duration::from_millis(100)).await;
    assert_eq!(
        outcome,
        ShutdownOutcome::DeadlineElapsed { deadline_ms: 100 },
        "an unfinished accept loop is reported as incomplete"
    );
    assert!(
        !outcome.is_success(),
        "an incomplete drain is never reported as success"
    );

    // The still-pending request then completes on its own, proving the deadline
    // was a reporting boundary and not a fake kill.
    let mut buf = Vec::new();
    tokio::time::timeout(EXCHANGE_DEADLINE, stream.read_to_end(&mut buf))
        .await
        .expect("late reply is bounded")
        .expect("read");
    assert!(
        String::from_utf8_lossy(&buf).contains("late /hang"),
        "the handler completed after the deadline was reported"
    );
}

// ---------------------------------------------------------------------------
// Transport modes refused before bind
// ---------------------------------------------------------------------------

/// Configured TLS is refused rather than downgraded to insecure TCP, and nothing
/// is bound.
#[tokio::test]
async fn tls_is_refused_before_bind() {
    let err = BoundListener::bind_from_config(&tls_config("/tmp/nd-cert.pem", "/tmp/nd-key.pem"))
        .expect_err("TLS config must be refused");
    assert_eq!(
        err,
        BindError::Unsupported(UnsupportedTransport::TlsConfigured {
            cert: "/tmp/nd-cert.pem".to_string(),
            key: "/tmp/nd-key.pem".to_string(),
        })
    );
    assert!(
        err.to_string().contains("not implemented"),
        "the message says what is missing: {err}"
    );
}

/// A configured Unix address is refused instead of silently binding TCP under
/// it; no path is created or unlinked.
#[tokio::test]
async fn unix_address_is_refused_before_bind() {
    let err = BoundListener::bind("unix:/tmp/nd.sock", 0).expect_err("unix must be refused");
    assert_eq!(
        err,
        BindError::Unsupported(UnsupportedTransport::UnixConfigured {
            address: "unix:/tmp/nd.sock".to_string()
        })
    );
    assert!(
        !std::path::Path::new("/tmp/nd.sock").exists(),
        "no socket file was created or unlinked"
    );
}

#[tokio::test]
async fn invalid_bind_addresses_are_rejected_without_binding() {
    // These are rejected before any bind attempt, as InvalidAddress — not as an
    // OS-level refusal. `localhost` is a name, and this increment resolves only
    // numeric literals (recorded as a gap).
    for (address, port) in [
        ("not-an-ip", 8080_i64),
        ("localhost", 8080),
        ("127.0.0.1", 70_000),
        ("127.0.0.1", -1),
        ("", 8080),
    ] {
        match BoundListener::bind(address, port) {
            Ok(ok) => panic!(
                "{address}:{port} must be rejected, bound {}",
                ok.local_addr()
            ),
            Err(BindError::InvalidAddress { .. }) => {}
            Err(other) => panic!("{address}:{port} must be InvalidAddress, got {other:?}"),
        }
    }
}

/// Build a config with only bind-relevant fields set (no real config file).
fn tls_config(cert: &str, key: &str) -> nd_conf::Config {
    nd_conf::Config {
        address: "127.0.0.1".to_string(),
        port: 0,
        tls_cert: cert.to_string(),
        tls_key: key.to_string(),
        ..nd_conf::Config::default()
    }
}

/// Parent-requested shutdown determinism cases: the signalled state must be
/// persistent storage, not a notification that can be missed.
#[tokio::test]
async fn signal_is_persistent_state_with_no_receiver_at_all() {
    // Zero receivers ever subscribed: `send` would store nothing, so `signal`
    // must use unconditional replacement.
    let shutdown = Shutdown::new();
    assert!(!shutdown.signalled());
    shutdown.signal();
    assert!(
        shutdown.signalled(),
        "the state persists with zero receivers"
    );

    // A subscriber created *after* the signal still sees it immediately.
    tokio::time::timeout(EXCHANGE_DEADLINE, shutdown.wait())
        .await
        .expect("a late subscriber observes the already-persisted signal");
}

#[tokio::test]
async fn cancel_waiter_before_it_subscribes_then_signal_and_wait_again() {
    let shutdown = Shutdown::new();

    // Start a wait, drop it before it can observe anything, then signal.
    let early = shutdown.clone();
    let dropped = tokio::spawn(async move { early.wait().await });
    dropped.abort();
    let _ = tokio::time::timeout(EXCHANGE_DEADLINE, dropped).await;

    shutdown.signal();
    assert!(shutdown.signalled());

    // A fresh waiter must resolve, not hang: cancellation of an earlier waiter
    // cannot consume the signal.
    tokio::time::timeout(EXCHANGE_DEADLINE, shutdown.wait())
        .await
        .expect("signal survives a cancelled waiter");
}

#[tokio::test]
async fn already_waiting_waiter_wakes_on_signal() {
    let shutdown = Shutdown::new();
    let waiting = shutdown.clone();
    let waiter = tokio::spawn(async move { waiting.wait().await });

    // Bound the wait for the waiter to reach its await point without a sleep:
    // poll the runtime until it is not immediately resolvable, then signal.
    tokio::task::yield_now().await;
    shutdown.signal();

    tokio::time::timeout(EXCHANGE_DEADLINE, waiter)
        .await
        .expect("waiting waiter wakes")
        .expect("wait task ran");
}

#[tokio::test]
async fn repeated_shutdown_signals_are_idempotent() {
    let shutdown = Shutdown::new();
    shutdown.signal();
    shutdown.signal();
    shutdown.signal();
    assert!(shutdown.signalled());

    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    let running =
        Running::start(listener, injected_router(None), shutdown.clone(), "").expect("start");

    // A clone sees the same state.
    assert!(
        shutdown.clone().signalled(),
        "clones share the persistent state"
    );

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(outcome, ShutdownOutcome::Drained);

    // stop_and_join consumed the handle; signalling afterwards is still safe.
    shutdown.signal();
    assert!(shutdown.signalled());

    // No residual listener on that port.
    let again = BoundListener::bind("127.0.0.1", i64::from(addr.port()))
        .expect("port free after repeated signals");
    assert_eq!(again.local_addr(), addr);
}

/// Real loopback: bind, serve, signal, join, rebind the same port — all bounded,
/// no startup sleep.
#[tokio::test]
async fn real_loopback_start_immediate_shutdown_join_and_port_rebind() {
    let listener = BoundListener::bind("127.0.0.1", 0).expect("bind");
    let addr = listener.local_addr();
    let running =
        Running::start(listener, injected_router(None), Shutdown::new(), "").expect("start");

    // One real request proves it served before shutdown.
    assert_eq!(exchange(addr, &get("/ping")).await.status, 200);

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(outcome, ShutdownOutcome::Drained);
    assert!(outcome.is_success());

    let rebound = BoundListener::bind("127.0.0.1", i64::from(addr.port()))
        .unwrap_or_else(|err| panic!("rebind port {}: {err}", addr.port()));
    assert_eq!(
        rebound.local_addr(),
        addr,
        "same port rebinds after a joined shutdown"
    );

    let running2 =
        Running::start(rebound, injected_router(None), Shutdown::new(), "").expect("rebind serve");
    assert_eq!(
        exchange(addr, &get("/ping")).await.status,
        200,
        "rebound server serves"
    );
    assert_eq!(
        running2.stop_and_join(EXCHANGE_DEADLINE).await,
        ShutdownOutcome::Drained
    );
}

/// Configuration-driven bind uses the configured address/port and refuses TLS,
/// without re-resolving anything else.
#[tokio::test]
async fn serve_from_config_binds_configured_loopback_and_serves() {
    let config = nd_conf::Config {
        address: "127.0.0.1".to_string(),
        port: 0,
        base_path: "/nd".to_string(),
        ..nd_conf::Config::default()
    };
    let running =
        Running::serve_from_config(&config, injected_router(None), Shutdown::new()).expect("serve");
    let addr = running.local_addr();
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert_ne!(addr.port(), 0);

    // Literal heartbeat answers; BasePath-joined ping does not (source-exact).
    assert_eq!(exchange(addr, &get("/ping")).await.status, 200);
    assert_eq!(exchange(addr, &get("/nd/ping")).await.status, 404);
    assert_eq!(exchange(addr, &get("/nd/echo")).await.status, 200);

    let outcome = running.stop_and_join(EXCHANGE_DEADLINE).await;
    assert_eq!(outcome, ShutdownOutcome::Drained);
}
