//! Serving half of `core/stream` — the handler that turns a stream request into
//! a response body. Port of the three layers Go stacks on top of
//! `MediaStreamer.Serve`:
//!
//!   - `core/stream/media_streamer.go:66-133` (`NewStream`) — resolve raw vs
//!     transcode, open the file or launch ffmpeg through the cache/limiter;
//!   - `core/stream/media_streamer.go:135-209` (`Stream`, `Serve`) — the
//!     seekable vs non-seekable write arms, headers, HEAD drain;
//!   - `server/subsonic/stream.go:19-53` (`Stream`) and
//!     `server/public/handle_streams.go:18-90` (`handleStream`) — the two
//!     callers: headers they set, the 429 arm, and the close-on-exit.
//!
//! ## What this owns, and what it deliberately does not
//!
//! It owns **one request → one response body**: plan the headers, pick the body
//! kind ([`BodyKind`]), stream it through a [`Sink`], and tear the process down.
//! It does **not** own HTTP. There is no `ResponseWriter` here; the plan
//! ([`ServePlan`] from [`crate::range`]) plus [`ResponseHeaders`] are the
//! contract any HTTP framework can implement — hyper, axum, or a test buffer.
//!
//! It also does **not** own the transcoding **file cache**
//! (`utils/cache/file_caches.go`): Go's `ms.cache.Get`
//! (`media_streamer.go:112`) dedupes identical pending transcodes and persists
//! them on disk. That eviction contract is a separate lane and is *not*
//! reimplemented here. Instead the cache seam is injectable:
//! [`StreamSource::transcode`] is the seam a cache implementation plugs into,
//! and [`ProcessTranscode`] is the uncached default (one call = one ffmpeg
//! process, capped by the limiter). A cache lane can implement
//! [`TranscodeSource`] and hand it to [`StreamHandler`] without touching this
//! module.
//!
//! ## Divergences from Go, frozen here
//!
//!   - **No ambient context.** Go's handlers read `r.Context()`; here the
//!     client going away is an explicit [`Disconnect`] future handed to
//!     [`StreamHandler::serve`], and the transcode is refused while the cap is
//!     enabled if that channel is absent (see [`Disconnect`] and
//!     [`GateError::CancellationRequired`]).
//!   - **The handler always closes.** Go's handlers `defer stream.Close()`
//!     (`server/subsonic/stream.go:42-46`, `server/public/handle_streams.go:77-81`),
//!     which closes the `io.ReadCloser` only — Go never waits for ffmpeg to be
//!     reaped (`core/ffmpeg/ffmpeg.go:361` has no `Close`, and
//!     `releasingReadCloser.Close` frees the slot right after closing the reader,
//!     `core/stream/limiter.go:63-66`). Here [`CloseState`] reports both facts
//!     separately: the reader is always closed, and the process is reaped through
//!     the owner that holds the concurrency slot, so the slot is only freed by a
//!     **confirmed** reap. An unresolved teardown is reported as
//!     [`CloseState::cleanup_pending`], never swallowed.
//!   - **Content sniffing is not ported.** Go's seekable arm lets
//!     `http.ServeContent` sniff 512 bytes when the extension yields no MIME
//!     type (`net/http/fs.go:292-301`). That is [`crate::range`]'s deviation 6
//!     and is reproduced here: the caller's `content_type` wins, else the
//!     builtin extension table, else no header.
//!   - **Go's MIME table includes `/etc/mime.types` on unix**
//!     (`mime.TypeByExtension`). The port ships Go's builtin entries only
//!     ([`crate::transcode_body::content_type_for_format`]), which is also what
//!     Go answers for an unknown extension (`""` ⇒ header omitted).
//!   - **`http.Error` bodies are not written by this layer.** Go's error arms
//!     write `<code> message: <text>\n` with `Content-Type: text/plain`
//!     (`net/http/server.go:2387-2391`). [`ResponseHeaders::error_body`] carries
//!     Go's message text and the `text/plain; charset=utf-8` type so the HTTP
//!     layer can render exactly that; the port does not decide the framing.
//!   - **Subsonic envelope is not composed here.** `server/subsonic/api.go:309-323`
//!     wraps the 429 in a Subsonic `subsonic-response` with `errorCode 0`
//!     (`responses.ErrorGeneric`, `responses/errors.go:4`). The handler exposes
//!     the status, the `Retry-After`, the message, and the Subsonic error code
//!     ([`StreamError::subsonic_error_code`]) so the API layer can build the
//!     envelope without guessing.

use std::fmt::Write as _;
use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::archiver::BoxFuture;
use crate::direct::{meta_from_metadata, plan_and_open, RangeBody};
use crate::limiter::TranscodeLimiter;
use crate::range::{plan, plan_streaming, CondRequest, FileMeta, ServePlan};
use crate::transcode_body::{
    content_type_for_format, default_command_for, estimated_content_length, open_transcode_gated,
    Disconnect, GateError, GatedTranscode, TranscodeJob,
};
use crate::types::{MediaFile, Request};

/// Go's `X-Content-Type-Options` on both stream handlers
/// (`server/subsonic/stream.go:48`, `server/public/handle_streams.go:83`).
pub const HEADER_CONTENT_TYPE_OPTIONS: &str = "X-Content-Type-Options";
/// The value both handlers set for it.
pub const NOSNIFF: &str = "nosniff";
/// Go's `X-Content-Duration` (`server/subsonic/stream.go:49`,
/// `server/public/handle_streams.go:84`).
pub const HEADER_CONTENT_DURATION: &str = "X-Content-Duration";
/// Go's `Content-Disposition` on the Download arm
/// (`server/subsonic/stream.go:115`), for a single MediaFile download.
pub const HEADER_CONTENT_DISPOSITION: &str = "Content-Disposition";
/// `estimateContentLength` query parameter (`media_streamer.go:168`).
pub const PARAM_ESTIMATE_CONTENT_LENGTH: &str = "estimateContentLength";

/// The client-facing 429 message, verbatim from
/// `server/public/handle_streams.go:68` and `server/subsonic/api.go:309-310`.
pub const TOO_MANY_TRANSCODES_MESSAGE: &str =
    "too many concurrent transcodes, please retry shortly";
/// `http.Error(w, "invalid request", 400)` — share token decode failure
/// (`server/public/handle_streams.go:25`).
pub const INVALID_REQUEST: &str = "invalid request";
/// `http.Error(w, "not found", 404)` (`server/public/handle_streams.go:48,59`).
pub const NOT_FOUND: &str = "not found";
/// `http.Error(w, "internal error", 500)` (`server/public/handle_streams.go:72`,
/// and `:88` when the serve produced nothing).
pub const INTERNAL_ERROR: &str = "internal error";
/// `responses.ErrorGeneric` (`server/subsonic/responses/errors.go:4`), the code
/// `sendError` uses for the rate-limit rejection (`server/subsonic/api.go:319-323`).
pub const SUBSONIC_ERROR_GENERIC: i32 = 0;
/// Content type Go's `http.Error` sets for a plain error body
/// (`net/http/server.go:2388`).
pub const ERROR_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// `strconv.FormatFloat(float64(mf.Duration), 'G', -1, 32)`
/// (`server/subsonic/stream.go:49`, `server/public/handle_streams.go:84`): the
/// shortest decimal that round-trips the **float32** value, in exponential form
/// when Go's rule says so.
///
/// Go's Duration is a `float32` widened to `float64` without gain or loss, so the
/// emitted digits are exactly the float32's shortest round-trip decimal — which
/// is what Rust's `{:e}` also produces. This header is user-visible, so it is
/// reproduced digit-for-digit rather than approximated: `300` ⇒ `"300"`,
/// `0.3` ⇒ `"0.3"`, `100000` ⇒ `"100000"`, `1e6` ⇒ `"1E+06"`, `1e-5` ⇒ `"1E-05"`.
///
/// The form rule is Go's `fmtEFG` (`internal/strconv/ftoa.go:348-365`): with the
/// shortest precision, `eprec` is 6 and the decimal exponent `dp-1` selects
/// exponential when it is `< -4` or `>= 6`. `internal/strconv` is not public, so
/// the rule is ported here and pinned against a Go oracle table in
/// `tests/handler.rs`.
#[must_use]
pub fn format_duration_header(duration: f64) -> String {
    let value = duration as f32;
    if !value.is_finite() {
        // Go's 'G' formatting for these (`format.go`'s Inf/NaN arms).
        if value.is_nan() {
            return "NaN".to_string();
        }
        return if value > 0.0 {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        };
    }
    if value == 0.0 {
        return "0".to_string();
    }

    let (negative, digits, exponent) = shortest_decimal(value);
    // Go's rule: exponential when `dp-1` is outside `[-4, 6)`.
    if !(-4..6).contains(&exponent) {
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        out.push_str(&digits[..1]);
        // Go sets `prec = nd - 1` for the exponential form, so every remaining
        // digit is printed and none is trimmed.
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "E{:+03}", exponent);
        return out;
    }

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&fixed_point(&digits, exponent));
    out
}

/// Split the shortest round-trip decimal of `value` into sign, digit string
/// (no dot, no leading zeros), and the **decimal exponent** — the power of ten
/// that puts the first digit in front of the decimal point.
///
/// Rust's `{:e}` is `d.dddde±dd` with the shortest mantissa, so the exponent it
/// prints is exactly Go's `dp - 1`.
fn shortest_decimal(value: f32) -> (bool, String, i32) {
    let text = format!("{value:e}");
    let (mantissa, exp) = match text.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (text.as_str(), 0),
    };
    let negative = mantissa.starts_with('-');
    let digits = mantissa.replace(['-', '.', '+'], "");
    (negative, digits, exp)
}

/// Fixed-point rendering of a digit string whose first digit sits at
/// `10^exponent` — Go's `fmtF` without a precision (`ftoa.go:390-440`):
/// `0.000ddd`, `ddd`, or `ddd.ddd` depending on where the point lands.
fn fixed_point(digits: &str, exponent: i32) -> String {
    let point = exponent + 1; // digits before the decimal point
    if point <= 0 {
        let mut out = String::from("0.");
        for _ in 0..(-point) {
            out.push('0');
        }
        out.push_str(digits);
        out
    } else if point as usize >= digits.len() {
        let mut out = digits.to_string();
        for _ in 0..(point as usize - digits.len()) {
            out.push('0');
        }
        out
    } else {
        let split = point as usize;
        let mut out = String::new();
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
        out
    }
}

/// A minimal async byte sink — the port's stand-in for Go's `http.ResponseWriter`
/// body. `write_all` is the only verb the serve loop needs, which is why the
/// same trait serves a socket, a `Vec<u8>`, or a channel-backed test client.
pub trait Sink {
    /// Write every byte of `data`. An error here is what Go sees from `io.Copy`
    /// when the client went away (`media_streamer.go:181`).
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>>;
}

impl Sink for Vec<u8> {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.extend_from_slice(data);
            Ok(())
        })
    }
}

/// Sink that discards bytes — Go's `io.Discard` for the HEAD arm
/// (`media_streamer.go:175`).
#[derive(Debug, Default, Clone, Copy)]
pub struct DiscardSink;

impl Sink for DiscardSink {
    fn write_all<'a>(&'a mut self, _data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { Ok(()) })
    }
}

/// Sink that fails once `after` bytes have been written: a client that vanished
/// mid-body, when the 200 is already flushed.
#[derive(Debug)]
pub struct DisconnectingSink {
    written: usize,
    after: usize,
}

impl DisconnectingSink {
    /// Fail on the first write after `after` bytes have gone through.
    #[must_use]
    pub fn new(after: usize) -> Self {
        DisconnectingSink { written: 0, after }
    }

    /// Bytes accepted before the failure.
    #[must_use]
    pub fn written(&self) -> usize {
        self.written
    }
}

impl Sink for DisconnectingSink {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if self.written >= self.after {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "client went away",
                ));
            }
            self.written += data.len();
            Ok(())
        })
    }
}

/// Sink backed by an unbounded channel, so a test can drop the receiving end
/// mid-body and the write fails exactly like a closed socket.
#[derive(Debug)]
pub struct ChannelSink {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
}

impl Default for ChannelSink {
    fn default() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        ChannelSink { tx, rx: Some(rx) }
    }
}

impl ChannelSink {
    /// Detach the receiving end, so every subsequent write reports the client
    /// as gone.
    pub fn drop_client(&mut self) {
        self.rx = None;
    }

    /// Drain the bytes written so far.
    #[must_use]
    pub fn collected(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(chunk) = rx.try_recv() {
                out.extend_from_slice(chunk.as_slice());
            }
        }
        out
    }
}

impl Sink for ChannelSink {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.tx
                .send(data.to_vec())
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client went away"))
        })
    }
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// The response headers the HTTP layer must emit, in the order Go writes them.
///
/// Modeled as an ordered list rather than a map because Go's `Header().Set`
/// replaces a value while keeping insertion order, and because the port must be
/// able to say "delete this header" the way `writeNotModified`
/// (`net/http/fs.go:627-643`) and `serveError` (`:196-206`) do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseHeaders {
    entries: Vec<(String, String)>,
}

impl ResponseHeaders {
    /// Empty header set.
    #[must_use]
    pub fn new() -> Self {
        ResponseHeaders::default()
    }

    /// `Header().Set`: replace an existing value in place, append when absent.
    pub fn set(&mut self, name: &str, value: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| k == name) {
            entry.1 = value.to_string();
        } else {
            self.entries.push((name.to_string(), value.to_string()));
        }
    }

    /// `Header().Del`.
    pub fn del(&mut self, name: &str) {
        self.entries.retain(|(k, _)| k != name);
    }

    /// `Header().Get`: `None` when absent (Go returns `""`).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// All entries, in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[(String, String)] {
        self.entries.as_slice()
    }

    /// True when nothing is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Which body backs the response. Go decides this with `Stream.Seekable()`
/// (`media_streamer.go:160`): a raw file is seekable and goes through
/// `http.ServeContent`; an ffmpeg pipe is not and is copied directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// Seekable direct file — the Range/conditional matrix in [`crate::range`].
    Direct,
    /// Non-seekable transcode pipe — `Accept-Ranges: none`, no Range support.
    Transcoded,
}

impl BodyKind {
    /// Go's `Stream.Seekable()` for this kind.
    #[must_use]
    pub fn is_seekable(&self) -> bool {
        matches!(self, BodyKind::Direct)
    }
}

/// `Stream.ContentType` (`media_streamer.go:146`) for a served format, then
/// Go's `mime.TypeByExtension` for the direct-file name
/// (`http.ServeContent` derives it from `Stream.Name()` =
/// `mf.Title + "." + format`, `media_streamer.go:147`).
///
/// The caller's explicit `content_type` wins — Go only computes a type when the
/// writer has none (`fs.go:287-305`). When the table has no entry the header is
/// omitted: Go gets `""` and writes an empty Content-Type, which the wire layer
/// drops (`transcode_body.rs` module note).
#[must_use]
pub fn content_type_for(format: &str, explicit: Option<&str>) -> Option<String> {
    if let Some(value) = explicit {
        return Some(value.to_string());
    }
    content_type_for_format(format).map(str::to_string)
}

/// Everything the response needs before a byte is written: the plan (status and
/// Range/conditional headers), the caller headers to add on top, and the body
/// kind the loop must drive.
#[derive(Debug, Clone)]
pub struct StreamPlan {
    /// 200/206/304/412/416 plus the byte window.
    pub plan: ServePlan,
    /// Direct file or transcode pipe.
    pub kind: BodyKind,
    /// Headers to emit with the status, in order.
    pub headers: ResponseHeaders,
    /// Media metadata the plan was computed from.
    pub meta: FileMeta,
    /// Transcode parameters, set only for [`BodyKind::Transcoded`].
    pub job: Option<TranscodeJob>,
    /// Content type actually chosen, for logs and the empty-output message Go
    /// prints (`media_streamer.go:192`, which quotes `s.ContentType()`).
    pub content_type: Option<String>,
}

impl StreamPlan {
    /// Bytes the body must produce. Zero for 304/412/416 and for HEAD.
    #[must_use]
    pub fn bytes_to_write(&self) -> u64 {
        self.plan.bytes_to_write()
    }

    /// Whether no body may be written.
    #[must_use]
    pub fn no_body(&self) -> bool {
        self.plan.no_body
    }
}

/// Request the handler serves: a resolved [`Request`] plus the authorized path,
/// the conditional headers, and the two caller-set extras.
///
/// `path` is Go's `mf.AbsolutePath()` (`media_streamer.go:84`) and must already
/// be authorized — see the trust boundary in [`crate::direct`] and
/// [`crate::transcode_body`]. `username` is Go's `limiterKey(ctx)`
/// (`media_streamer.go:300-307`): `""` for anonymous (public share) requests.
#[derive(Debug, Clone, Default)]
pub struct StreamRequest {
    /// Conditional + Range headers, flattened from the request.
    pub cond: CondRequest,
    /// The authorized media row's fields (`suffix`, `duration`, `updated_at`, …).
    pub media_file: MediaFile,
    /// Resolved stream parameters — Go's `ResolveRequest` output.
    pub request: Request,
    /// Already-authorized absolute path to the media file.
    pub path: String,
    /// Limiter bucket key: username, or `""` for anonymous.
    pub username: String,
    /// `estimateContentLength` query parameter (`media_streamer.go:168`).
    pub estimate_content_length: bool,
    /// `Content-Disposition` filename for the Download arm
    /// (`server/subsonic/stream.go:115`); `None` on Stream and share routes.
    pub download_filename: Option<String>,
    /// Caller-set ETag in wire form. `None` — Navidrome sets none
    /// (`media_streamer.go:161`); see [`crate::range`] deviation 4.
    pub etag: Option<String>,
    /// Caller-set Content-Type, which takes precedence over the table.
    pub content_type: Option<String>,
    /// Caller-set Content-Encoding; a plain 200 then omits Content-Length
    /// (`fs.go:426`).
    pub content_encoding: Option<String>,
}

impl StreamRequest {
    /// The format actually served: `"raw"` (and bitrate 0) when the request asks
    /// for raw or nothing, exactly as `NewStream` normalizes it
    /// (`media_streamer.go:77-82`).
    #[must_use]
    pub fn served_format(&self) -> String {
        if self.request.format.is_empty() || self.request.format == "raw" {
            "raw".to_string()
        } else {
            self.request.format.clone()
        }
    }

    /// Go's `Stream.Name()` (`media_streamer.go:147`): `Title + "." + format`.
    /// The port carries no title, so the media file's id stands in — it is only
    /// used for the Content-Disposition filename and Go's logs.
    #[must_use]
    pub fn stream_name(&self) -> String {
        let format = self.served_format();
        let base = if self.media_file.id.is_empty() {
            "stream".to_string()
        } else {
            self.media_file.id.clone()
        };
        format!("{base}.{format}")
    }

    /// `Stream.EstimatedContentLength` for this request, only meaningful for a
    /// transcode pipe (`media_streamer.go:148-150`).
    #[must_use]
    pub fn estimated_content_length(&self) -> u64 {
        let bit_rate = if self.served_format() == "raw" {
            0
        } else {
            self.request.bit_rate
        };
        estimated_content_length(self.media_file.duration as f32, bit_rate) as u64
    }
}

// ---------------------------------------------------------------------------
// Errors and the response verdict
// ---------------------------------------------------------------------------

/// Everything a stream request can fail with, each arm carrying the status,
/// header and message Go produces for that failure.
///
/// The mapping is split across Go's two callers and this port keeps both:
///   - the rate-limit arm is 429 + `Retry-After: 5` and
///     `TOO_MANY_TRANSCODES_MESSAGE` (`server/subsonic/api.go:319-323`,
///     `server/public/handle_streams.go:66-69`);
///   - a missing media file is 404 `"not found"`
///     (`server/public/handle_streams.go:47-49`), which the Subsonic router
///     renders as `ErrorDataNotFound` (`api.go:303-304`);
///   - every other failure is 500, with Go's share-handler message
///     `"invalid request"` (`handle_streams.go:72`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// The concurrency cap refused the transcode before anything launched
    /// (`core/stream/limiter.go:87-111` via `media_streamer.go:234-241`).
    #[error("{TOO_MANY_TRANSCODES_MESSAGE}")]
    TooManyTranscodes,

    /// The media file could not be read (`media_streamer.go:91-94`'s `os.Open`
    /// error, surfaced as `not found` by the share handler).
    #[error("{NOT_FOUND}")]
    NotFound,

    /// Launch, resolution or argument-building failed before any byte was
    /// produced — Go's `NewStream` error arm (`media_streamer.go:111-121`),
    /// which the handlers turn into a 500.
    #[error("{message}")]
    ServeStart {
        /// Go's error text from the transcode layer.
        message: String,
    },

    /// A gated launch was declared non-cancellable while the cap is enabled
    /// (`GateError::CancellationRequired`): Go forces the request context onto
    /// the process in that case (`media_streamer.go:243-260`), so the
    /// combination cannot be served here either.
    #[error("{INVALID_REQUEST}")]
    CancellationRequired,

    /// The body failed **with 0 bytes written**, so the status is still
    /// swappable — Go deletes `Content-Length` and returns the error
    /// (`media_streamer.go:181-186`), and the caller answers 500
    /// (`server/subsonic/stream.go:52` → `sendError`;
    /// `server/public/handle_streams.go:87-89`).
    #[error("{message}")]
    TruncatedBeforeFirstByte {
        /// Go's `Error sending transcoded file` message plus the cause.
        message: String,
    },

    /// The body failed **after** bytes were written: the 200 is on the wire and
    /// the only signal is to drop the connection — Go's
    /// `panic(http.ErrAbortHandler)` (`media_streamer.go:187-188`).
    #[error("response aborted after {written} bytes: {message}")]
    Aborted {
        /// Bytes already flushed.
        written: u64,
        /// The write or read failure Go logs.
        message: String,
    },

    /// Opening or reading the media file failed for a reason Go logs and returns
    /// as-is (`media_streamer.go:91-94`). The kind is kept rather than the
    /// `io::Error` itself, so the failure stays comparable and clonable while a
    /// caller can still branch on NotFound/PermissionDenied the way Go's
    /// `errors.Is(err, model.ErrNotFound)` arm does.
    #[error("streaming failed: {message}")]
    Io {
        /// Go's error text.
        message: String,
        /// The `ErrorKind` the failure arrived as.
        kind: io::ErrorKind,
    },
}

impl StreamError {
    /// Status Go answers for this failure.
    #[must_use]
    pub fn status_code(&self) -> u16 {
        match self {
            StreamError::TooManyTranscodes => 429,
            StreamError::NotFound => 404,
            StreamError::ServeStart { .. }
            | StreamError::CancellationRequired
            | StreamError::TruncatedBeforeFirstByte { .. }
            | StreamError::Aborted { .. }
            | StreamError::Io { .. } => 500,
        }
    }

    /// `Retry-After`, only for the rate-limit arm (`RetryAfterSeconds`,
    /// `core/stream/limiter.go:16-19`).
    #[must_use]
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            StreamError::TooManyTranscodes => Some(crate::limiter::RETRY_AFTER_SECONDS),
            _ => None,
        }
    }

    /// The message Go writes. `None` when Go writes no body at all: an aborted
    /// response has nothing left to send, and 412/416 come from
    /// `ServePlan::error_message` instead.
    #[must_use]
    pub fn client_message(&self) -> Option<&'static str> {
        match self {
            StreamError::TooManyTranscodes => Some(TOO_MANY_TRANSCODES_MESSAGE),
            StreamError::NotFound => Some(NOT_FOUND),
            StreamError::ServeStart { .. }
            | StreamError::CancellationRequired
            | StreamError::TruncatedBeforeFirstByte { .. }
            | StreamError::Io { .. } => Some(INTERNAL_ERROR),
            StreamError::Aborted { .. } => None,
        }
    }

    /// The Subsonic error code for the envelope (`api.go:297-311`):
    /// `ErrorDataNotFound` (70) for a missing file, `ErrorGeneric` (0)
    /// otherwise — which is what `sendError` uses for the 429.
    #[must_use]
    pub fn subsonic_error_code(&self) -> i32 {
        match self {
            StreamError::NotFound => 70,
            _ => SUBSONIC_ERROR_GENERIC,
        }
    }

    /// Whether the response must be dropped rather than completed: Go's
    /// `panic(http.ErrAbortHandler)` arm.
    #[must_use]
    pub fn aborts_response(&self) -> bool {
        matches!(self, StreamError::Aborted { .. })
    }

    /// True for the rate-limit arm — the check Go performs with
    /// `errors.Is(err, stream.ErrTooManyTranscodes)` in three places, including
    /// the archive swallow-and-finalize arm (`server/subsonic/stream.go:139-144`).
    #[must_use]
    pub fn is_too_many_transcodes(&self) -> bool {
        matches!(self, StreamError::TooManyTranscodes)
    }
}

impl From<GateError> for StreamError {
    fn from(value: GateError) -> Self {
        match value {
            GateError::Rejected(_) => StreamError::TooManyTranscodes,
            GateError::CancellationRequired => StreamError::CancellationRequired,
            GateError::Body(body) => StreamError::ServeStart {
                message: body.to_string(),
            },
        }
    }
}

impl From<crate::transcode_body::TranscodeBodyError> for StreamError {
    fn from(value: crate::transcode_body::TranscodeBodyError) -> Self {
        StreamError::ServeStart {
            message: value.to_string(),
        }
    }
}

/// A body that produced no bytes. Go logs this and does **not** treat it as an
/// error (`media_streamer.go:190-193`), but the share handler still answers 500
/// because `Serve` returned `n == 0` (`server/public/handle_streams.go:87-89`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyOutput {
    /// Bytes written — always 0.
    pub written: u64,
    /// Go's log line, quoted so the operator can act on it.
    pub log_message: String,
    /// Status the share handler answers for `n == 0`.
    pub status: u16,
    /// Message the share handler writes for that status.
    pub client_message: &'static str,
}

/// The complete outcome of one request: status, headers, byte count, and the
/// cleanup facts a caller must not lose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamResponse {
    /// Status written (200/206/304/412/416, or an error status).
    pub status: u16,
    /// Headers to emit with the status.
    pub headers: ResponseHeaders,
    /// Bytes written to the sink.
    pub written: u64,
    /// Whether the body was seekable (direct file) or a pipe: Go's
    /// `Stream.Seekable()` (`media_streamer.go:144`), which chooses between
    /// `http.ServeContent` and a direct copy.
    pub seekable: bool,
    /// The client went away and the process was killed (`ServeOutcome::disconnected`).
    pub disconnected: bool,
    /// Empty transcode output — logged, not an error, but a 500 for the share
    /// handler (`server/public/handle_streams.go:87-89`).
    pub empty_output: Option<EmptyOutput>,
    /// Failure, if any. `Some` with `aborts_response()` is Go's abort arm.
    pub error: Option<StreamError>,
    /// Response body teardown ran (Go's `defer stream.Close()`).
    pub reader_closed: bool,
    /// The ffmpeg process is still owned because a reap could not be confirmed.
    /// The concurrency slot stays held in that case, so an operator must see it.
    pub cleanup_pending: bool,
    /// Rendering of the unresolved cleanup, for the log.
    pub cleanup_error: Option<String>,
    /// Unconfirmed teardown handed to a detached tracker, which re-signals the
    /// group, reaps it, and frees the concurrency slot only on a confirmed reap
    /// (`crate::transcode_body::GatedTranscode::spawn_cleanup`).
    pub handed_off_to_tracker: bool,
    /// Pid of the transcode child, when one was launched.
    pub pid: Option<u32>,
    /// Process teardown confirmed (killed or exited and reaped).
    pub child_reaped: bool,
}

impl StreamResponse {
    /// Whether the body was cut short after the status was sent.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(StreamError::aborts_response)
    }

    /// The status the **public share handler** answers for this outcome
    /// (`server/public/handle_streams.go:87-89`): `if err != nil || n == 0 {
    /// http.Error(w, "internal error", 500) }`.
    ///
    /// This is deliberately not the same as [`Self::status`]: Go's share handler
    /// overwrites whatever `Serve` wrote with a 500 whenever the copy returned
    /// an error **or zero bytes**, which is why a HEAD on a shared stream answers
    /// 500 and why an empty ffmpeg output answers 500 even though `Serve` only
    /// logged it. The Subsonic handler instead reports `err` and leaves the
    /// status alone (`server/subsonic/stream.go:52`).
    #[must_use]
    pub fn share_handler_status(&self) -> u16 {
        if self.error.is_some() || self.written == 0 {
            500
        } else {
            self.status
        }
    }

    /// Teardown summary, for the log Go's handlers print when closing fails
    /// (`server/subsonic/stream.go:43-45`).
    #[must_use]
    pub fn close_state(&self) -> CloseState {
        CloseState {
            reader_closed: self.reader_closed,
            has_child: self.pid.is_some(),
            child_reaped: self.child_reaped,
            cleanup_pending: self.cleanup_pending,
            cleanup_error: self.cleanup_error.clone(),
            handed_off_to_tracker: self.handed_off_to_tracker,
        }
    }
}

/// Teardown facts, reported separately because Go's `stream.Close()` and a
/// confirmed process reap are different things and the port must say which
/// happened.
///
/// Go's `defer stream.Close()` closes the `io.ReadCloser` and frees the limiter
/// slot immediately without proving ffmpeg exited (`core/stream/limiter.go:63-66`);
/// Go's `ffCmd` has no `Close` at all (`core/ffmpeg/ffmpeg.go:361`). Here the
/// reader is always closed and the child is reaped through the owner that holds
/// the slot, so the slot is freed **only** by a confirmed reap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseState {
    /// The response reader was closed (fd / stdout pipe dropped).
    pub reader_closed: bool,
    /// A transcode child was launched for this request at all.
    pub has_child: bool,
    /// The child was reaped: gone, kernel-confirmed.
    pub child_reaped: bool,
    /// Teardown could not confirm the reap; the child and its slot are still held.
    pub cleanup_pending: bool,
    /// Rendering of the [`CleanupError`] behind `cleanup_pending`.
    pub cleanup_error: Option<String>,
    /// Cleanup handed to the detached tracker, which frees the slot only after a
    /// confirmed reap — the fail-closed rule from `transcode_body.rs`.
    pub handed_off_to_tracker: bool,
}

// ---------------------------------------------------------------------------
// The cache seam
// ---------------------------------------------------------------------------

/// A cached transcode body — the seam the transcoding **file cache** plugs into.
///
/// Go's `ms.cache.Get` (`media_streamer.go:112`) returns
/// `cache.FileCacheItem{ReadCloser, Seeker, Cached}`; when the entry is complete
/// it hands back a seekable file and the response goes through
/// `http.ServeContent`, and when it is still being produced it hands back the
/// live ffmpeg pipe. That eviction contract is **not** ported here (it is a
/// separate #15 lane), so this trait is the injection point: a cache lane
/// implements it and is handed to [`StreamHandler::new`] unchanged. The default
/// implementation, [`ProcessTranscode`], is the uncached one — one request, one
/// ffmpeg process, capped by the limiter.
pub trait CachedTranscode {
    /// Whether the cached entry is complete and seekable. `true` means the
    /// handler re-plans the response as a direct (Range-capable) body, which is
    /// Go's `s.Seekable()` arm (`media_streamer.go:160`).
    fn seekable(&self) -> bool {
        false
    }

    /// Path of the complete cached file, when one exists. `Some` requires
    /// [`Self::seekable`] to be true; the handler opens and plans it through
    /// [`crate::direct::plan_and_open`], so Range and conditional semantics are
    /// the same as a raw file.
    fn path(&self) -> Option<&Path> {
        None
    }

    /// Read the next chunk of a non-seekable cached body.
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>>;

    /// Close the cached body.
    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>>;
}

/// What a transcode open produced.
pub enum TranscodeLease<D> {
    /// A live ffmpeg process, with its concurrency slot held until a confirmed
    /// reap (`crate::transcode_body::GatedTranscode`). The gate owns the
    /// disconnect signal, because Go binds ffmpeg to the request context
    /// (`media_streamer.go:243-260`).
    Process(Box<GatedTranscode<D>>),
    /// A cached body: complete (seekable file) or still streaming.
    ///
    /// `signal` hands the disconnect back when the lease binds nothing to it, so
    /// the caller's pump loop can still race writes against it; a source that
    /// produces the entry lazily may keep it instead and return `None`.
    Cached {
        /// The cached body itself.
        body: Box<dyn CachedTranscode>,
        /// The signal, when the lease did not consume it.
        signal: Option<D>,
    },
}

impl<D> TranscodeLease<D> {
    /// Pid of the launched process, when there is one.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        match self {
            TranscodeLease::Process(gate) => gate.pid(),
            TranscodeLease::Cached { .. } => None,
        }
    }
}

/// The transcode-open seam: [`StreamHandler`] calls this instead of Go's
/// `cache.Get` → limiter → `Transcode` chain.
pub trait TranscodeSource<D> {
    /// Resolve the body for `job`. Errors are the refusal/launch arms Go
    /// distinguishes: [`GateError::Rejected`] is the 429, everything else a 500.
    ///
    /// `disconnect` must be `Disconnect::When` whenever the limiter is enabled —
    /// Go forces the request context onto the process in that case
    /// (`media_streamer.go:243-260`), and [`GateError::CancellationRequired`] is
    /// the port's equivalent refusal.
    fn open<'a>(
        &'a mut self,
        mf: &'a MediaFile,
        job: &'a TranscodeJob,
        user: &'a str,
        disconnect: Disconnect<D>,
    ) -> BoxFuture<'a, Result<TranscodeLease<D>, GateError>>;
}

/// Uncached transcode open: reserve a slot, launch ffmpeg. This is the port of
/// Go's cache-producer body (`media_streamer.go:226-282`) minus the cache —
/// acquire before launch (`:234`), release on launch failure (`:273-277`), slot
/// tied to the process thereafter (`:281`), and release only on a **confirmed**
/// reap (see the permit-lifetime note in `transcode_body.rs`).
#[derive(Clone)]
pub struct ProcessTranscode {
    /// `Transcoding.MaxConcurrent` / `MaxConcurrentPerUser` caps.
    pub limiter: TranscodeLimiter,
}

impl ProcessTranscode {
    /// Wrap a limiter as the transcode source.
    #[must_use]
    pub fn new(limiter: TranscodeLimiter) -> Self {
        ProcessTranscode { limiter }
    }
}

impl<D> TranscodeSource<D> for ProcessTranscode
where
    D: Future<Output = ()> + Send + 'static,
{
    fn open<'a>(
        &'a mut self,
        mf: &'a MediaFile,
        job: &'a TranscodeJob,
        user: &'a str,
        disconnect: Disconnect<D>,
    ) -> BoxFuture<'a, Result<TranscodeLease<D>, GateError>> {
        Box::pin(async move {
            let gate = open_transcode_gated(&self.limiter, user, mf, job, disconnect).await?;
            Ok(TranscodeLease::Process(Box::new(gate)))
        })
    }
}

/// Assemble the launch job from a resolved [`Request`] — Go's `streamJob`
/// (`media_streamer.go:101-111`). `command` comes from the caller's
/// `LookupTranscodeCommand` result; empty means the built-in default for the
/// format (`crate::transcode_body::default_command_for`).
#[must_use]
pub fn job_for_request(req: &StreamRequest, command: &str) -> TranscodeJob {
    TranscodeJob {
        file_path: req.path.clone(),
        format: req.served_format(),
        bit_rate: req.request.bit_rate,
        sample_rate: req.request.sample_rate,
        bit_depth: req.request.bit_depth,
        channels: req.request.channels,
        offset: req.request.offset,
        command: command.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// The response body after planning.
enum Body<D> {
    /// Seekable window into the authorized file (or a complete cached file).
    Direct(Option<Box<RangeBody>>),
    /// Live ffmpeg process plus its concurrency slot.
    Transcode(Option<Box<GatedTranscode<D>>>),
    /// Cached body that is still being produced (a pipe).
    Cached(Option<Box<dyn CachedTranscode>>),
    /// Nothing to write: 304/412/416, HEAD, or a zero-length window.
    None,
}

/// A prepared request: plan plus whatever body was opened for it. Holding the
/// body means holding the process and its slot.
///
/// `disconnect` is `Some` unless a gated launch consumed it: that future is moved
/// into the [`GatedTranscode`] (Go's request context bound to the process,
/// `media_streamer.go:243-260`), so the write loop inside the gate is the only
/// poller. The other arms keep it here and race their pumps against it.
struct Prepared<D> {
    plan: StreamPlan,
    body: Body<D>,
    pid: Option<u32>,
    disconnect: Option<D>,
}

/// The stream handler: Go's `MediaStreamer` + `Stream.Serve` + the two callers,
/// as one service struct (workspace freeze: explicit construction, no DI).
///
/// `D` is the client-disconnect channel, carried the way Go carries
/// `r.Context()` inside the stream (`media_streamer.go:136`).
pub struct StreamHandler<D> {
    source: Box<dyn TranscodeSource<D>>,
}

impl<D> StreamHandler<D>
where
    D: Future<Output = ()> + Send + 'static,
{
    /// Build a handler over a transcode source. [`ProcessTranscode`] is the
    /// uncached default; a cache-backed [`TranscodeSource`] plugs in unchanged
    /// when the transcoding-file cache lane lands.
    #[must_use]
    pub fn new(source: Box<dyn TranscodeSource<D>>) -> Self {
        StreamHandler { source }
    }

    /// Plan the response without opening a body: the pure half of `NewStream`
    /// plus the header writes of `Serve`.
    ///
    /// Raw requests plan against the file's real size (Go's `os.Open` +
    /// `http.ServeContent`); transcoded requests plan through
    /// [`crate::range::plan_streaming`], Go's non-seekable arm
    /// (`media_streamer.go:165-172`).
    pub async fn plan(&self, req: &StreamRequest) -> Result<StreamPlan, StreamError> {
        let format = req.served_format();
        if format == "raw" {
            let path = Path::new(req.path.as_str());
            let md = tokio::fs::metadata(path).await.map_err(stream_io_error)?;
            if md.is_dir() {
                // Go's `fileExists` directory arm; the share handler answers 404.
                return Err(StreamError::NotFound);
            }
            let meta = meta_from_metadata(&md, req.etag.as_deref());
            // Raw: Go sets `s.format = mf.Suffix` (`media_streamer.go:97`) and
            // `http.ServeContent` derives Content-Type from `Stream.Name()`'s
            // extension (`fs.go:290`).
            let content_type =
                content_type_for(raw_extension(req).as_str(), req.content_type.as_deref());
            let planned = plan(
                &FileMeta {
                    content_type: content_type.clone(),
                    content_encoding: req.content_encoding.clone(),
                    ..meta.clone()
                },
                &req.cond,
            );
            let headers = headers_for(req, &planned, BodyKind::Direct);
            return Ok(StreamPlan {
                plan: planned,
                kind: BodyKind::Direct,
                headers,
                meta,
                job: None,
                content_type,
            });
        }

        let meta = FileMeta {
            // A pipe has no size; `plan_streaming` never reads it.
            size: 0,
            last_modified_secs: non_zero_time(req.media_file.updated_at),
            etag: crate::direct::normalize_etag(req.etag.as_deref()),
            content_type: None,
            content_encoding: req.content_encoding.clone(),
        };
        let content_type = content_type_for(format.as_str(), req.content_type.as_deref());
        let estimate = req
            .estimate_content_length
            .then(|| req.estimated_content_length());
        let planned = plan_streaming(
            &FileMeta {
                content_type: content_type.clone(),
                ..meta.clone()
            },
            &req.cond,
            estimate,
        );
        let headers = headers_for(req, &planned, BodyKind::Transcoded);
        let job = job_for_request(req, "");
        Ok(StreamPlan {
            plan: planned,
            kind: BodyKind::Transcoded,
            headers,
            meta,
            job: Some(job),
            content_type,
        })
    }

    /// Plan, then open the body — which for a transcode means reserve a slot and
    /// launch ffmpeg. The returned value owns the process, so the concurrency
    /// slot is held from here until the teardown in [`Self::serve`] confirms the
    /// reap.
    async fn prepare(
        &mut self,
        req: &StreamRequest,
        disconnect: D,
    ) -> Result<Prepared<D>, StreamError> {
        let plan = self.plan(req).await?;
        // A HEAD on a **seekable** body writes nothing at all
        // (`fs.go:429-431` skips the copy), so there is nothing to open. A HEAD
        // on a transcode still goes through `NewStream` in Go
        // (`server/subsonic/stream.go:37`, `server/public/handle_streams.go:61`)
        // — ffmpeg is launched and then drained by the HEAD arm
        // (`media_streamer.go:174-177`) so it cannot stall on a full pipe. Only
        // the seekable short-circuit is faithful here.
        if plan.no_body() && plan.kind.is_seekable() {
            return Ok(Prepared {
                plan,
                body: Body::None,
                pid: None,
                disconnect: Some(disconnect),
            });
        }
        match plan.kind {
            BodyKind::Direct => Ok(self.open_direct(req, &plan, disconnect).await?),
            BodyKind::Transcoded => Ok(self.open_transcode(req, plan, disconnect).await?),
        }
    }

    /// Seekable arm: open the authorized file, seek to the planned window. Go's
    /// `os.Open` (`media_streamer.go:91`) plus `http.ServeContent`'s
    /// `content.Seek(ra.start, io.SeekStart)` (`fs.go:361`).
    async fn open_direct(
        &self,
        req: &StreamRequest,
        plan: &StreamPlan,
        disconnect: D,
    ) -> Result<Prepared<D>, StreamError> {
        let (meta, planned, body) =
            plan_and_open(Path::new(req.path.as_str()), &req.cond, req.etag.as_deref())
                .await
                .map_err(stream_io_error)?;
        let content_type =
            content_type_for(raw_extension(req).as_str(), req.content_type.as_deref());
        // Re-apply the type after the open, exactly as serveContent does when the
        // writer has no Content-Type set (`fs.go:287-305`).
        let planned = ServePlan {
            content_type: if planned.status == 304 || planned.no_body {
                planned.content_type.clone()
            } else {
                content_type.clone()
            },
            ..planned
        };
        let headers = headers_for(req, &planned, BodyKind::Direct);
        let prepared = StreamPlan {
            plan: planned,
            kind: BodyKind::Direct,
            headers,
            meta,
            job: None,
            content_type,
        };
        let _ = plan;
        Ok(Prepared {
            plan: prepared,
            body: Body::Direct(body.map(Box::new)),
            pid: None,
            disconnect: Some(disconnect),
        })
    }

    /// Non-seekable arm: resolve the ffmpeg command, then open through the seam
    /// (Go's `LookupTranscodeCommand` + `cache.Get` → limiter → `Transcode`).
    async fn open_transcode(
        &mut self,
        req: &StreamRequest,
        mut plan: StreamPlan,
        disconnect: D,
    ) -> Result<Prepared<D>, StreamError> {
        let mut job = plan.job.take().unwrap_or_default();
        if job.command.is_empty() {
            job.command = default_command_for(job.format.as_str());
        }
        job.file_path = req.path.clone();
        let lease = self
            .source
            .open(
                &req.media_file,
                &job,
                req.username.as_str(),
                Disconnect::When(disconnect),
            )
            .await?;

        match lease {
            TranscodeLease::Process(gate) => {
                let pid = gate.pid();
                Ok(Prepared {
                    plan,
                    body: Body::Transcode(Some(gate)),
                    pid,
                    // Consumed by the gate: its write loop polls it, and killing the
                    // process group is what cancellation does there.
                    disconnect: None,
                })
            }
            TranscodeLease::Cached {
                body: cached,
                signal,
            } => {
                if cached.seekable() {
                    // Go's complete-cache-entry arm hands back a Seeker, so
                    // `Serve` goes through `http.ServeContent`
                    // (`media_streamer.go:160-162`).
                    let path = cached.path().map(Path::to_path_buf);
                    let open = match path {
                        Some(ref path) => plan_and_open(path, &req.cond, req.etag.as_deref()).await,
                        None => {
                            plan_and_open(
                                Path::new(req.path.as_str()),
                                &req.cond,
                                req.etag.as_deref(),
                            )
                            .await
                        }
                    };
                    let (meta, planned, body) = open.map_err(stream_io_error)?;
                    let content_type =
                        content_type_for(req.served_format().as_str(), req.content_type.as_deref());
                    let planned = ServePlan {
                        content_type: content_type.clone(),
                        ..planned
                    };
                    let headers = headers_for(req, &planned, BodyKind::Direct);
                    plan.plan = planned;
                    plan.kind = BodyKind::Direct;
                    plan.headers = headers;
                    plan.meta = meta;
                    plan.content_type = content_type;
                    plan.job = None;
                    Ok(Prepared {
                        plan,
                        body: Body::Direct(body.map(Box::new)),
                        pid: None,
                        disconnect: signal,
                    })
                } else {
                    Ok(Prepared {
                        plan,
                        body: Body::Cached(Some(cached)),
                        pid: None,
                        disconnect: signal,
                    })
                }
            }
        }
    }

    /// Serve one request end to end: plan, launch if needed, write the body, then
    /// tear it down. Returns the status, headers, byte count, and the cleanup
    /// facts a caller must not lose.
    ///
    /// `disconnect` resolves when the client goes away — the Rust shape of Go's
    /// `r.Context()` cancellation. A gated launch requires it, because Go binds
    /// ffmpeg to the request context whenever the cap is enabled
    /// (`media_streamer.go:243-260`); the refusal is
    /// [`StreamError::CancellationRequired`].
    ///
    /// The process is always torn down before this returns. Go's handlers
    /// `defer stream.Close()` (`server/subsonic/stream.go:42-46`,
    /// `server/public/handle_streams.go:77-81`); here the teardown additionally
    /// **reaps** the child, so [`StreamResponse::child_reaped`] is true unless the
    /// kernel could not confirm the reap. An unconfirmed teardown is handed to the
    /// detached tracker, which re-signals the group, reaps it, and frees the
    /// concurrency slot only on a confirmed reap.
    pub async fn serve<W>(
        &mut self,
        req: &StreamRequest,
        sink: &mut W,
        disconnect: D,
    ) -> StreamResponse
    where
        W: Sink,
        D: Unpin,
    {
        let prepared = match self.prepare(req, disconnect).await {
            Ok(prepared) => prepared,
            Err(error) => return error_response(error),
        };

        let Prepared {
            plan,
            mut body,
            pid,
            disconnect,
        } = prepared;
        let seekable = plan.kind.is_seekable();
        let mut headers = plan.headers.clone();
        let mut status = plan.plan.status;
        let head = req.cond.is_head();

        let mut written: u64 = 0;
        let mut disconnected = false;
        let mut failure: Option<(u64, String)> = None;
        let mut empty_output: Option<EmptyOutput> = None;
        let reader_closed = true;
        let mut cleanup_pending = false;
        let mut cleanup_error: Option<String> = None;
        let mut child_reaped = false;
        let mut handed_off = false;
        // `None` when the gate owns the signal, so nothing here polls it twice.
        let mut disconnect = disconnect;

        match &mut body {
            Body::None => {}
            Body::Direct(file) => {
                if let Some(file) = file.as_mut() {
                    // `http.ServeContent` writes no body for HEAD (`fs.go:429-431`)
                    // and never reads the file either, so `written` stays 0.
                    if !head {
                        if let Some(signal) = disconnect.as_mut() {
                            let pump =
                                pump_reader(file, sink, signal, plan.bytes_to_write(), false).await;
                            written = pump.0;
                            failure = pump.1.map(|message| (written, message));
                            disconnected = pump.2;
                        }
                    }
                    file.close().await.ok();
                }
            }
            Body::Cached(cached) => {
                // HEAD drains the pipe so the producer cannot stall on a full
                // stdout (`media_streamer.go:174-177`), and Go reports **0 bytes
                // written** for that arm — so `written` stays 0 here too, which is
                // what makes the share handler answer 500 for `n == 0`
                // (`server/public/handle_streams.go:87-89`). Reproduced, not fixed.
                if let (Some(cached), Some(signal)) = (cached.as_mut(), disconnect.as_mut()) {
                    let pump = pump_reader(
                        &mut CachedAdapter {
                            inner: cached.as_mut(),
                        },
                        sink,
                        signal,
                        u64::MAX,
                        head,
                    )
                    .await;
                    written = if head { 0 } else { pump.0 };
                    failure = pump.1.map(|message| (written, message));
                    disconnected = pump.2;
                    cached.close().await.ok();
                }
            }
            Body::Transcode(gate) => {
                if let Some(gate) = gate.as_mut() {
                    let outcome = if head {
                        gate.drain().await
                    } else {
                        gate.serve(&mut SinkAdapter { inner: sink }).await
                    };
                    // Go's HEAD arm spawns a goroutine that drains to
                    // `io.Discard` and returns `0, nil`
                    // (`media_streamer.go:174-177`), so the handler reports no
                    // bytes written even though ffmpeg was drained. Carrying the
                    // drained byte count here instead would hide the `n == 0`
                    // condition the share handler answers with a 500
                    // (`server/public/handle_streams.go:87-89`).
                    written = if head { 0 } else { outcome.written };
                    disconnected = outcome.disconnected;
                    if let Some(message) = outcome.error.as_ref() {
                        failure = Some((outcome.written, message.clone()));
                    }

                    // Go's `defer stream.Close()`: close the reader and finish the
                    // process. A disconnect already killed the group inside the write
                    // loop, so the teardown here confirms the reap; a failed stream is
                    // killed for the same reason.
                    let kill = outcome.disconnected || outcome.truncated || outcome.error.is_some();
                    let teardown = if kill {
                        gate.cancel().await
                    } else {
                        gate.close().await
                    };
                    match teardown {
                        Ok(_) => child_reaped = true,
                        Err(cleanup) => {
                            cleanup_error = Some(cleanup.to_string());
                            if gate.cleanup_pending() {
                                // Never conclude the process is gone: hand it to the
                                // tracker, which kills and reaps outside this future
                                // and frees the slot only on a confirmed reap.
                                gate.spawn_cleanup();
                                handed_off = true;
                                cleanup_pending = true;
                            }
                        }
                    }

                    if outcome.empty_output {
                        // Go logs this and does not treat it as an error
                        // (`media_streamer.go:190-193`).
                        let reported = if head { 0 } else { outcome.written };
                        empty_output = Some(EmptyOutput {
                            written: reported,
                            log_message: empty_output_message(plan.content_type.as_deref()),
                            status: 500,
                            client_message: INTERNAL_ERROR,
                        });
                    }
                }
            }
        }

        // Go's two write arms (`media_streamer.go:181-188`): nothing on the wire
        // yet ⇒ the status is still swappable; bytes on the wire ⇒ the only way to
        // say "truncated" is to drop the connection.
        let mut error = None;
        if let Some((at, message)) = failure {
            if at == 0 {
                headers.del("Content-Length");
                status = 500;
                error = Some(StreamError::TruncatedBeforeFirstByte { message });
            } else {
                error = Some(StreamError::Aborted {
                    written: at,
                    message,
                });
            }
        }

        StreamResponse {
            status,
            headers,
            written,
            seekable,
            disconnected,
            empty_output,
            error,
            reader_closed,
            cleanup_pending,
            cleanup_error,
            handed_off_to_tracker: handed_off,
            pid,
            child_reaped,
        }
    }
}

// ---------------------------------------------------------------------------
// Body pumps
// ---------------------------------------------------------------------------

/// Copy a body to the sink, racing every await against `disconnect`.
///
/// Returns `(written, error_message, disconnected)`. The disconnect is polled
/// inside the read as well as around the write, for the reason
/// `transcode_body::serve_stream` documents: a sink can hang or fail mid-flush
/// with the status already sent, and a read-only loop would never notice.
///
/// `head` drains without writing — Go's HEAD arm for a non-seekable stream
/// (`media_streamer.go:174-177`). `want` bounds the copy the way
/// `io.CopyN(w, sendContent, sendSize)` does for the seekable arm
/// (`fs.go:431`).
async fn pump_reader<W, D>(
    body: &mut dyn ChunkReader,
    sink: &mut W,
    disconnect: &mut D,
    want: u64,
    head: bool,
) -> (u64, Option<String>, bool)
where
    W: Sink,
    D: Future<Output = ()> + Unpin,
{
    tokio::pin!(disconnect);
    let mut written: u64 = 0;
    loop {
        if written >= want {
            return (written, None, false);
        }
        let chunk = tokio::select! {
            biased;
            () = &mut disconnect => return (written, None, true),
            r = body.read_chunk(crate::direct::MAX_CHUNK_BYTES) => r,
        };
        let bytes = match chunk {
            Ok(None) => return (written, None, false),
            Ok(Some(bytes)) => bytes,
            Err(message) => return (written, Some(message), false),
        };
        if bytes.is_empty() {
            return (written, None, false);
        }
        if head {
            written += bytes.len() as u64;
            continue;
        }
        let result = tokio::select! {
            biased;
            () = &mut disconnect => return (written, None, true),
            r = sink.write_all(bytes.as_slice()) => r,
        };
        match result {
            Ok(()) => written += bytes.len() as u64,
            Err(error) => return (written, Some(error.to_string()), false),
        }
    }
}

/// Adapter from [`Sink`] onto [`tokio::io::AsyncWrite`], so the transcoding lane's
/// own write loop (`TranscodeBody::serve_with`) is reused as-is rather than
/// re-implemented here. Writes are never buffered: `poll_flush` and
/// `poll_shutdown` are no-ops because a sink hands bytes straight to the client.
struct SinkAdapter<'a, W> {
    inner: &'a mut W,
}

impl<W: Sink> tokio::io::AsyncWrite for SinkAdapter<'_, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let inner = &mut self.get_mut().inner;
        let mut future = inner.write_all(buf);
        match Pin::new(&mut future).poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(buf.len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Read verb shared by the two body shapes, so one pump serves both. `Ok(None)`
/// is EOF; `Err` carries the message Go would log (`media_streamer.go:182`).
pub trait ChunkReader {
    /// Read the next chunk, at most `max` bytes; `None` at EOF.
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;
}

impl<T: ChunkReader + ?Sized> ChunkReader for Box<T> {
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move { self.as_mut().read_chunk(max).await })
    }
}

impl ChunkReader for RangeBody {
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move { self.read_chunk(max).await.map_err(|e| e.to_string()) })
    }
}

/// Adapter from the cache seam's `io`-shaped reads onto [`ChunkReader`], so one
/// pump loop serves a file window and a cached body alike.
struct CachedAdapter<'a> {
    inner: &'a mut dyn CachedTranscode,
}

impl ChunkReader for CachedAdapter<'_> {
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move { self.inner.read_chunk(max).await.map_err(|e| e.to_string()) })
    }
}

// ---------------------------------------------------------------------------
// Header assembly and small helpers
// ---------------------------------------------------------------------------

/// Headers to emit with the status, in Go's write order.
///
/// The caller-set headers come first because Go's handlers set them before
/// calling `Serve`: `X-Content-Type-Options: nosniff` and `X-Content-Duration`
/// (`server/subsonic/stream.go:48-49`, `server/public/handle_streams.go:83-84`),
/// and the Download arm's `Content-Disposition` (`server/subsonic/stream.go:115`).
/// Then `serveContent`'s order: `Accept-Ranges` (`fs.go:398`, before the status is
/// chosen), `Content-Range` (`:367`, `:333`), `Content-Length` (`:425-427`),
/// `Last-Modified` (`setLastModified`, `:623-627`, written *before* the
/// preconditions but carried here as a plan field), `ETag`, `Content-Type`.
///
/// The plan already pruned what `writeNotModified` (`fs.go:627-643`) and
/// `serveError` (`:196-206`) delete, so a `None` field means "do not send it"
/// rather than "send an empty value".
pub fn headers_for(req: &StreamRequest, plan: &ServePlan, kind: BodyKind) -> ResponseHeaders {
    let mut headers = ResponseHeaders::new();
    headers.set(HEADER_CONTENT_TYPE_OPTIONS, NOSNIFF);
    headers.set(
        HEADER_CONTENT_DURATION,
        format_duration_header(req.media_file.duration).as_str(),
    );
    if let Some(name) = req.download_filename.as_deref() {
        headers.set(
            HEADER_CONTENT_DISPOSITION,
            nd_utils::str::content_disposition_attachment(name).as_str(),
        );
    }
    if let Some(value) = plan.accept_ranges.as_deref() {
        headers.set("Accept-Ranges", value);
    }
    if let Some(value) = plan.content_range.as_deref() {
        headers.set("Content-Range", value);
    }
    if let Some(value) = plan.content_length {
        headers.set("Content-Length", value.to_string().as_str());
    }
    if let Some(value) = plan.last_modified.as_deref() {
        headers.set("Last-Modified", value);
    }
    if let Some(value) = plan.etag.as_deref() {
        headers.set("ETag", value);
    }
    // 304 deletes Content-Type (`fs.go:634`); an error status replaces it with
    // text/plain, which the error arm sets separately.
    if plan.status != 304 {
        if let Some(value) = plan.content_type.as_deref() {
            headers.set("Content-Type", value);
        }
    }
    // Content-Encoding survives only where Go leaves it: preconditions that
    // return early delete it (`fs.go:200,:202,:635`).
    let keeps_encoding = matches!(plan.status, 200 | 206);
    if keeps_encoding {
        if let Some(value) = req.content_encoding.as_deref() {
            headers.set("Content-Encoding", value);
        }
    }
    // A non-seekable body never advertises range support beyond `none`, which the
    // plan already carries (`media_streamer.go:165`).
    let _ = kind;
    headers
}

/// Extension that drives the Content-Type lookup for a **raw** stream: Go sets
/// `s.format = mf.Suffix` (`media_streamer.go:97`) and `http.ServeContent` looks
/// up `filepath.Ext(Stream.Name())` (`fs.go:290`), i.e. the media file's suffix.
#[must_use]
pub fn raw_extension(req: &StreamRequest) -> String {
    if req.media_file.suffix.is_empty() {
        // Go's builtin table has no entry for an unknown extension ⇒ no header.
        return String::new();
    }
    req.media_file.suffix.to_lowercase()
}

/// Go's zero-time handling: a `time.Time` that is zero (or epoch 0, which
/// `isZeroTime` also treats as unknown, `fs.go:616-621`) means no `Last-Modified`
/// and inert timestamp conditionals.
#[must_use]
pub fn non_zero_time(epoch_secs: i64) -> Option<i64> {
    if epoch_secs == 0 {
        None
    } else {
        Some(epoch_secs)
    }
}

/// Go's empty-output warning, which quotes the content type it tried to produce
/// (`media_streamer.go:191-193`).
#[must_use]
pub fn empty_output_message(content_type: Option<&str>) -> String {
    format!(
        "Transcoding returned empty output, ffmpeg may have failed. Check that ffmpeg \
         supports the requested codec. Enable Trace logging for ffmpeg stderr details \
         (format={})",
        content_type.unwrap_or("")
    )
}

/// Map a file-system failure onto Go's status choice: a missing media file is
/// `not found` (`server/public/handle_streams.go:47-49`), anything else an
/// internal error (`:50-52`).
fn stream_io_error(error: io::Error) -> StreamError {
    let kind = error.kind();
    match kind {
        io::ErrorKind::NotFound => StreamError::NotFound,
        _ => StreamError::Io {
            message: error.to_string(),
            kind,
        },
    }
}

/// Headers Go's error arms set. The rate-limit arm adds `Retry-After`
/// (`server/subsonic/api.go:320`, `server/public/handle_streams.go:67`);
/// `http.Error` sets `Content-Type: text/plain; charset=utf-8`
/// (`net/http/server.go:2388`) and no `Content-Length`, since Go lets the
/// connection close delimit the message body.
fn error_headers(error: &StreamError) -> ResponseHeaders {
    let mut headers = ResponseHeaders::new();
    if let Some(secs) = error.retry_after() {
        headers.set("Retry-After", secs.to_string().as_str());
    }
    if error.client_message().is_some() {
        headers.set(HEADER_CONTENT_TYPE_OPTIONS, NOSNIFF);
        headers.set("Content-Type", ERROR_CONTENT_TYPE);
    }
    headers
}

/// The response Go sends when the failure happens before any body byte: the
/// status, the message headers, and no bytes.
fn error_response(error: StreamError) -> StreamResponse {
    let headers = error_headers(&error);
    StreamResponse {
        status: error.status_code(),
        headers,
        written: 0,
        seekable: false,
        disconnected: false,
        empty_output: None,
        error: Some(error),
        reader_closed: true,
        cleanup_pending: false,
        cleanup_error: None,
        handed_off_to_tracker: false,
        pid: None,
        child_reaped: false,
    }
}

/// Build the 429 response without a request, for a caller that only has the
/// rejection: the header pair Go's two handlers set, plus the Subsonic fields.
#[must_use]
pub fn too_many_transcodes_response() -> StreamResponse {
    error_response(StreamError::TooManyTranscodes)
}
