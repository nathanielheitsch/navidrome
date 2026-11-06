//! # nd-events — Rust port of the Go `server/events/*` SSE hub.
//!
//! Owned by issue #11 (`docs/rust-port/issues.md` Part B):
//! * [`event`] — `Event`/`baseEvent` and the concrete event types
//!   (`server/events/events.go`);
//! * [`broker`] — the `Broker` broadcast hub (`server/events/sse.go`):
//!   fan-out, drop-on-full client queues, `shouldSend` filtering;
//! * [`sse`] — the `/api/events` SSE endpoint: frame shape, headers and
//!   disconnect cancellation.
//!
//! Design freeze (`rust/Cargo.toml`): `thiserror` for library errors, `tokio`
//! for the async runtime, explicit construction instead of Go's
//! `singleton.GetInstance` (see [`Broker::new`]).

pub mod broker;
pub mod event;
pub mod sse;

pub use broker::{should_send, Broker, Client, Frame, NoopBroker, SenderCtx, Subscription};
pub use event::{Event, KeepAlive, NowPlayingCount, RefreshResource, ScanStatus, ServerStart, ANY};
pub use sse::{sse_handler, write_event, SseRequestCtx, KEEP_ALIVE_FREQUENCY, WRITE_TIME_OUT};
