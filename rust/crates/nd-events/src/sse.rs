//! SSE wire endpoint — port of `broker.ServeHTTP` (`server/events/sse.go`).
//!
//! Exposes [`sse_handler`], an axum handler for `/api/events`
//! (`server/server.go:192`), plus the frame serializer [`write_event`].

use std::convert::Infallible;
use std::time::Duration;

use axum::http::{header, HeaderValue};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};

use crate::broker::{Broker, Client, Frame};

/// Keep-alive period (`keepAliveFrequency`, `sse.go:26`).
pub const KEEP_ALIVE_FREQUENCY: Duration = Duration::from_secs(15);
/// Write timeout (`writeTimeOut`, `sse.go:28`): must exceed
/// [`KEEP_ALIVE_FREQUENCY`], "or the lack of activity will cause the channel
/// to close". Rust owns no socket write deadline; kept for the port record.
pub const WRITE_TIME_OUT: Duration = Duration::from_secs(20);

/// Request context captured by the endpoint — the Go request ctx values the
/// broker reads (`request.UserFrom`, `request.ClientUniqueIdFrom`,
/// `r.RemoteAddr`, `r.UserAgent()`).
#[derive(Debug, Clone, Default)]
pub struct SseRequestCtx {
    /// `X-ND-Client-Unique-Id` header/cookie value.
    pub client_unique_id: String,
    /// Authenticated username (`user.UserName`).
    pub username: String,
    /// `r.RemoteAddr`.
    pub remote_addr: String,
    /// `r.UserAgent()`.
    pub user_agent: String,
}

/// Serializes a frame — `writeEvent` (`sse.go:103`): the exact wire text
/// `fmt.Fprintf(w, "id: %d\nevent: %s\ndata: %s\n\n", ...)` emits.
pub fn write_event(frame: &Frame) -> String {
    format!(
        "id: {}\nevent: {}\ndata: {}\n\n",
        frame.id, frame.event, frame.data
    )
}

/// Converts a broker frame to an axum SSE event (`id:`/`event:`/`data:`).
fn to_sse(frame: Frame) -> SseEvent {
    SseEvent::default()
        .id(frame.id.to_string().as_str())
        .event(frame.event.as_str())
        .data(frame.data.as_str())
}

/// `GET /api/events` — `broker.ServeHTTP` (`sse.go:133`).
///
/// Registers the client with the broker, streams frames until the
/// connection drops (`for event := range pl.ReadOrDone(ctx, c.msgC)`,
/// `sse.go:157`), and detaches on disconnect (`defer b.unsubscribe(c)`,
/// `sse.go:154`). The `shouldSend` filter applies at fan-out time
/// (`sse.go:251`), which is where Go applies it.
pub async fn sse_handler(
    axum::extract::State(broker): axum::extract::State<Broker>,
    req_ctx: axum::extract::Request,
) -> Response {
    // `request.UserFrom(ctx)` / `request.ClientUniqueIdFrom(ctx)` — axum
    // extensions are the Rust equivalent of Go's context values; the
    // middleware layer that sets them is owned by the API issues (#18/#19).
    let ctx = req_ctx
        .extensions()
        .get::<SseRequestCtx>()
        .cloned()
        .unwrap_or_default();

    let client_id = nd_id::new_random();
    let client = Client::new(
        &client_id,
        &ctx.username,
        &ctx.remote_addr,
        &ctx.client_unique_id,
    );

    let rx = broker.subscribe(client).await.rx;
    let _ = client; // identity captured by the broker; kept for symmetry

    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        // `None` is a detach or broker shutdown: both close the stream,
        // matching `range` over a closed `c.msgC`.
        rx.recv()
            .await
            .map(|frame| (Ok::<_, Infallible>(to_sse(frame)), rx))
    });

    // `text/event-stream` comes from `Sse` itself; the rest are the
    // headers `ServeHTTP` sets (`sse.go:146-150`).
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    // Tells Nginx to not buffer this response. See https://stackoverflow.com/a/33414076
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}
