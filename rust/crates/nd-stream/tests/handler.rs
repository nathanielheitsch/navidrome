//! #15 serving-half tests for [`nd_stream::handler`].
//!
//! Three layers, in the order the code runs:
//!   1. **Response contract** — status, header set and order, byte counts, and
//!      the error arms, pinned to the Go sources
//!      (`core/stream/media_streamer.go`, `server/subsonic/stream.go`,
//!      `server/public/handle_streams.go`). `X-Content-Duration` is pinned
//!      against a Go oracle (`tests/data/go_duration/`, harness in
//!      `tests/testdata/go_duration/main.go`).
//!   2. **Process lifecycle** — a real ffmpeg child launched *through the
//!      handler* with the client dropped mid-body. Liveness is checked by pid,
//!      never by trusting `Drop`, and every kill-path assertion carries a
//!      control that fails if the kill were removed.
//!   3. **Seam** — the injectable transcode source stands in for the
//!      transcoding-file cache lane (complete-entry and in-progress arms) with no
//!      cache implementation ported here.
//!
//! Fixture paths derive from `CARGO_MANIFEST_DIR`: scratch lives under
//! `target/handler-tests/`, never an absolute path.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use nd_stream::archiver::BoxFuture;
use nd_stream::handler::{
    content_type_for, empty_output_message, format_duration_header, headers_for, job_for_request,
    non_zero_time, raw_extension, too_many_transcodes_response, BodyKind, CachedTranscode,
    ChannelSink, CloseState, DiscardSink, DisconnectingSink, EmptyOutput, ProcessTranscode,
    ResponseHeaders, StreamError, StreamHandler, StreamRequest, StreamResponse, TranscodeLease,
    TranscodeSource, ERROR_CONTENT_TYPE, HEADER_CONTENT_DISPOSITION, HEADER_CONTENT_DURATION,
    HEADER_CONTENT_TYPE_OPTIONS, INTERNAL_ERROR, INVALID_REQUEST, NOSNIFF, NOT_FOUND,
    SUBSONIC_ERROR_GENERIC, TOO_MANY_TRANSCODES_MESSAGE,
};
use nd_stream::limiter::{TranscodeLimiter, RETRY_AFTER_SECONDS};
use nd_stream::range::{http_date, plan, CondRequest, FileMeta};
use nd_stream::transcode_body::{Disconnect, GateError, TranscodeJob};
use nd_stream::types::{MediaFile, Request};

/// The disconnect signal: resolves when the client hangs up — the Rust shape of
/// Go's `r.Context()` cancellation (`media_streamer.go:136`).
///
/// `tokio::sync::oneshot::Receiver` resolves to a `Result`, so it is mapped to
/// `()` here: Go's ctx fires on cancellation *and* on the parent going away, and
/// a dropped sender is exactly that — the socket closed with no explicit signal.
struct HangUp(tokio::sync::oneshot::Receiver<()>);

impl Future for HangUp {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// fixture and process helpers
// ---------------------------------------------------------------------------

/// Scratch dir for fixtures these tests write, under the crate's own `target/`
/// so it is never an absolute path and never escapes the checkout.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("handler-tests")
        .join(name);
    std::fs::create_dir_all(&dir).expect("scratch dir under target/");
    dir
}

/// Repo root, derived from the crate manifest dir (`rust/crates/nd-stream`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("crate manifest dir is rust/crates/<crate>")
        .to_path_buf()
}

/// Go's own committed media fixture.
fn repo_fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures").join(name)
}

/// Is `pid` running? A zombie does not count: it has terminated, and only a
/// `wait` (which `nd-ffmpeg`'s reap performs) clears it. Same `ps` probe
/// `transcode_body.rs` and `nd-ffmpeg`'s lifecycle tests use.
fn pid_alive(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .expect("ps must be available for process lifecycle assertions");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// Every pid in `pids` still running.
fn survivors(pids: &[u32]) -> Vec<u32> {
    pids.iter().copied().filter(|pid| pid_alive(*pid)).collect()
}

/// ffmpeg binary, or `None` (skip-if-missing is the house style: ffmpeg 9 is on
/// the dev box but is not guaranteed on every CI host).
fn ffmpeg_bin() -> Option<String> {
    let (bin, ok) = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
    if ok {
        Some(bin)
    } else {
        None
    }
}

/// Encode a source of `secs` seconds with ffmpeg itself, so the transcode under
/// test always decodes a real file. Returns `None` when this ffmpeg build lacks
/// the encoder (skip-if-missing).
fn make_source(bin: &str, name: &str, secs: f64) -> Option<PathBuf> {
    let dir = scratch_dir(name);
    let path = dir.join("src.mp3");
    let ok = Command::new(bin)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:duration={secs}"),
            "-y",
            &path.display().to_string(),
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if ok && path.exists() {
        Some(path)
    } else {
        None
    }
}

/// A template command that keeps producing for the whole lane: no `%b`, so the
/// request's bitrate cannot bound the encode. `-f mp3 -` matches the mp3
/// Content-Type the assertions expect.
fn endless_mp3_command(bin: &str) -> String {
    format!("{bin} -hide_banner -loglevel error -i %s -f mp3 -")
}

/// Media row for a file on disk. `duration` drives `X-Content-Duration`,
/// `suffix` drives the Content-Type lookup.
fn media_file(path: &Path, suffix: &str, duration: f64) -> MediaFile {
    let size = std::fs::metadata(path).map(|md| md.len()).unwrap_or(0);
    MediaFile {
        id: "mf-1".to_string(),
        suffix: suffix.to_string(),
        bit_rate: 128,
        sample_rate: 44_100,
        duration,
        size: size as i64,
        updated_at: 1_700_000_000,
        ..MediaFile::default()
    }
}

/// The repo fixture must have no `If-Modified-Since`-style conditions and a
/// clean `Range`-free request; see [`get_request`].
fn get_request(path: &Path, mf: MediaFile) -> StreamRequest {
    StreamRequest {
        cond: CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        },
        media_file: mf,
        request: Request {
            format: "raw".to_string(),
            ..Request::default()
        },
        path: path.display().to_string(),
        username: "alice".to_string(),
        ..StreamRequest::default()
    }
}

/// A transcode request: format + bitrate, what the decider hands the handler
/// after `ResolveRequest`.
fn transcode_request(path: &Path, mf: MediaFile, format: &str, bit_rate: i64) -> StreamRequest {
    StreamRequest {
        request: Request {
            format: format.to_string(),
            bit_rate,
            ..Request::default()
        },
        ..get_request(path, mf)
    }
}

/// Handler over the uncached transcode source with `limiter`.
fn handler(limiter: TranscodeLimiter) -> StreamHandler<HangUp> {
    StreamHandler::new(Box::new(ProcessTranscode::new(limiter)))
}

/// A client that never hangs up. The sender is returned so a test can hang up.
fn stays_connected() -> (tokio::sync::oneshot::Sender<()>, HangUp) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (tx, HangUp(rx))
}

fn header<'a>(response: &'a StreamResponse, name: &str) -> Option<&'a str> {
    response.headers.get(name)
}

/// The pid of the ffmpeg child of this process running an encode to stdout.
/// Used only where a handler response cannot report it (a dropped serve future).
/// Every live ffmpeg child of this test process. Tests run in parallel in one
/// process, so a single-valued probe cannot tell "the child this test launched"
/// from another lane's; the callers use the set to take a **baseline** and then
/// require a pid that was not in it.
fn ffmpeg_children() -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &std::process::id().to_string(), "ffmpeg"])
        .output()
        .ok();
    match out {
        Some(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .filter(|pid| pid_alive(*pid))
            .collect(),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// X-Content-Duration: pinned against Go's own strconv output
// ---------------------------------------------------------------------------

/// Oracle harness: `tests/testdata/go_duration/main.go` prints
/// `strconv.FormatFloat(float64(float32), 'G', -1, 32)` — exactly what
/// `server/subsonic/stream.go:49` and `server/public/handle_streams.go:84`
/// write. Regenerate with:
///
/// ```text
/// go run ./rust/crates/nd-stream/tests/testdata/go_duration \
///   "$PWD/rust/crates/nd-stream/tests/data/go_duration"
/// ```
fn duration_oracle() -> Vec<(f64, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/go_duration")
        .join("format_float.txt");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (input, output) = line
                .split_once('|')
                .unwrap_or_else(|| panic!("oracle line {line:?} must be input|output"));
            let value = match input {
                "NaN" => f64::NAN,
                "+Inf" => f64::INFINITY,
                "-Inf" => f64::NEG_INFINITY,
                other => other
                    .parse::<f32>()
                    .unwrap_or_else(|err| panic!("oracle input {input:?}: {err}"))
                    as f64,
            };
            (value, output.to_string())
        })
        .collect()
}

#[test]
fn content_duration_header_matches_go_strconv_digit_for_digit() {
    // Arrange: the Go-produced table, covering the exponent-form boundary
    // (`>= 6` / `< -4`), negative zero, the float32 subnormal extreme, and Inf/NaN.
    let cases = duration_oracle();
    assert!(
        cases.len() >= 30,
        "oracle table must cover the boundary cases"
    );

    // Act / Assert
    for (duration, expected) in &cases {
        let actual = format_duration_header(*duration);
        assert_eq!(
            &actual, expected,
            "FormatFloat(float64(float32({duration})), 'G', -1, 32) ⇒ {expected}, got {actual}"
        );
    }
}

#[test]
fn caller_headers_are_written_before_servecontent_headers() {
    // Go's handlers set both headers before calling Serve
    // (`server/subsonic/stream.go:48-49`, `server/public/handle_streams.go:83-84`),
    // and the Download arm sets Content-Disposition (`stream.go:115`).
    let req = StreamRequest {
        media_file: MediaFile {
            duration: 300.0,
            suffix: "mp3".to_string(),
            ..MediaFile::default()
        },
        cond: CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        },
        download_filename: Some("song.mp3".to_string()),
        ..StreamRequest::default()
    };
    let meta = FileMeta {
        size: 100,
        last_modified_secs: Some(1_700_000_000),
        etag: Some("\"x\"".to_string()),
        content_type: Some("audio/mpeg".to_string()),
        content_encoding: None,
    };
    let headers = headers_for(&req, &plan(&meta, &req.cond), BodyKind::Direct);

    // Assert the exact order Go writes them in.
    let names: Vec<&str> = headers.entries().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            HEADER_CONTENT_TYPE_OPTIONS,
            HEADER_CONTENT_DURATION,
            HEADER_CONTENT_DISPOSITION,
            "Accept-Ranges",
            "Content-Length",
            "Last-Modified",
            "ETag",
            "Content-Type",
        ],
        "caller-set headers first, then serveContent's order"
    );
    assert_eq!(headers.get(HEADER_CONTENT_TYPE_OPTIONS), Some(NOSNIFF));
    assert_eq!(headers.get(HEADER_CONTENT_DURATION), Some("300"));
    assert_eq!(
        headers.get(HEADER_CONTENT_DISPOSITION),
        Some("attachment; filename=\"song.mp3\"")
    );
}

#[test]
fn header_writes_follow_go_for_the_special_statuses() {
    let req = |cond: CondRequest| StreamRequest {
        media_file: MediaFile {
            duration: 0.3,
            suffix: "flac".to_string(),
            ..MediaFile::default()
        },
        cond,
        content_encoding: Some("gzip".to_string()),
        ..StreamRequest::default()
    };
    let meta = FileMeta {
        size: 10,
        last_modified_secs: Some(1_700_000_000),
        etag: Some("\"x\"".to_string()),
        content_type: Some("audio/flac".to_string()),
        content_encoding: None,
    };

    // 304 deletes Content-Type and Content-Encoding (`fs.go:634-635`) and keeps
    // no Content-Length (`:636`).
    let cond = CondRequest {
        method: "GET".to_string(),
        if_none_match: Some("\"x\"".to_string()),
        ..CondRequest::default()
    };
    let headers = headers_for(&req(cond.clone()), &plan(&meta, &cond), BodyKind::Direct);
    assert_eq!(headers.get("Content-Type"), None);
    assert_eq!(headers.get("Content-Encoding"), None);
    assert_eq!(headers.get("Content-Length"), None);
    // `X-Content-Duration` survives: the caller set it before Serve.
    assert_eq!(headers.get(HEADER_CONTENT_DURATION), Some("0.3"));

    // 416 keeps Content-Range and has no Content-Type (`fs.go:199-206`).
    let cond = CondRequest {
        method: "GET".to_string(),
        range: Some("bytes=1000-2000".to_string()),
        ..CondRequest::default()
    };
    let headers = headers_for(&req(cond.clone()), &plan(&meta, &cond), BodyKind::Direct);
    assert_eq!(headers.get("Content-Range"), Some("bytes */10"));
    assert_eq!(headers.get("Content-Length"), None);
    assert_eq!(
        headers.get("Content-Encoding"),
        None,
        "serveError deletes it"
    );
}

#[test]
fn content_type_lookup_and_small_helpers() {
    // The caller's explicit type wins (`fs.go:287-305` computes one only when the
    // writer has none).
    assert_eq!(
        content_type_for("mp3", Some("audio/mp4")),
        Some("audio/mp4".to_string())
    );
    assert_eq!(
        content_type_for("flac", None),
        Some("audio/flac".to_string())
    );
    // Unknown extension ⇒ no header, like Go's `""` from `TypeByExtension`.
    assert_eq!(content_type_for("weird", None), None);

    // A raw stream looks up the *media file's* suffix, not the request format
    // (`media_streamer.go:97` sets `s.format = mf.Suffix`).
    let req = StreamRequest {
        media_file: MediaFile {
            suffix: "FLAC".to_string(),
            ..MediaFile::default()
        },
        ..StreamRequest::default()
    };
    assert_eq!(raw_extension(&req), "flac");
    assert_eq!(raw_extension(&StreamRequest::default()), "");

    assert_eq!(non_zero_time(0), None);
    assert_eq!(non_zero_time(1_700_000_000), Some(1_700_000_000));

    // Go's empty-output log line quotes the content type it tried to produce
    // (`media_streamer.go:191-193`).
    assert_eq!(
        empty_output_message(Some("audio/mpeg")),
        "Transcoding returned empty output, ffmpeg may have failed. Check that ffmpeg \
         supports the requested codec. Enable Trace logging for ffmpeg stderr details \
         (format=audio/mpeg)"
    );
    assert!(empty_output_message(None).ends_with("(format=)"));

    // streamJob assembly (`media_streamer.go:101-111`).
    let req = StreamRequest {
        path: "/music/a.flac".to_string(),
        request: Request {
            format: "wav".to_string(),
            bit_rate: 320,
            sample_rate: 48_000,
            bit_depth: 24,
            channels: 2,
            offset: 12,
        },
        ..StreamRequest::default()
    };
    let job = job_for_request(&req, "ffmpeg -i %s -b:a %bk mp3 -");
    let want = TranscodeJob {
        file_path: "/music/a.flac".to_string(),
        format: "wav".to_string(),
        bit_rate: 320,
        sample_rate: 48_000,
        bit_depth: 24,
        channels: 2,
        offset: 12,
        command: "ffmpeg -i %s -b:a %bk mp3 -".to_string(),
    };
    // TranscodeJob carries no PartialEq (transcode_body.rs owns it); field-wise
    // comparison is the same contract.
    assert_eq!(job.file_path, want.file_path);
    assert_eq!(job.format, want.format);
    assert_eq!(job.bit_rate, want.bit_rate);
    assert_eq!(job.sample_rate, want.sample_rate);
    assert_eq!(job.bit_depth, want.bit_depth);
    assert_eq!(job.channels, want.channels);
    assert_eq!(job.offset, want.offset);
    assert_eq!(job.command, want.command);

    // `raw` normalizes format to "raw" and bitrate 0 for the estimate
    // (`media_streamer.go:77-82`, `:148-150`).
    let raw = StreamRequest {
        request: Request {
            format: "raw".to_string(),
            bit_rate: 128,
            ..Request::default()
        },
        media_file: MediaFile {
            id: "mf-1".to_string(),
            duration: 300.0,
            ..MediaFile::default()
        },
        ..StreamRequest::default()
    };
    assert_eq!(raw.served_format(), "raw");
    assert_eq!(raw.estimated_content_length(), 0);
    let mp3 = StreamRequest {
        request: Request {
            format: "mp3".to_string(),
            bit_rate: 128,
            ..Request::default()
        },
        ..raw.clone()
    };
    assert_eq!(mp3.served_format(), "mp3");
    // Go: `int(float32(duration) * float32(bitRate) / 8 * 1024)` — verified with
    // a Go program, NOT the exact-arithmetic 4_800_000 an int expression gives:
    // float32(300) * 128 = 38400 exactly, /8 = 4800, *1024 = 4915200 rounds UP
    // to 4915200.00000011920929 (4800 * 1024 is not exactly representable in
    // binary32... it is, but the multiply chain is not) and the int conversion
    // truncates to 4_915_200.
    assert_eq!(mp3.estimated_content_length(), 4_915_200);
    assert_eq!(
        mp3.stream_name(),
        "mf-1.mp3",
        "Go's `Stream.Name()` is `Title + \".\" + format` (`media_streamer.go:147`); \
         the port has no title column, so it carries the id in that slot"
    );
    let wav = StreamRequest {
        request: Request {
            format: "wav".to_string(),
            bit_rate: 128,
            ..Request::default()
        },
        ..mp3.clone()
    };
    assert_eq!(wav.stream_name(), "mf-1.wav");
    let unnamed = StreamRequest {
        media_file: MediaFile {
            id: String::new(),
            ..mp3.media_file.clone()
        },
        ..mp3.clone()
    };
    assert_eq!(unnamed.stream_name(), "stream.mp3");
}

#[test]
fn error_arms_carry_go_status_headers_and_messages() {
    assert_eq!(StreamError::TooManyTranscodes.status_code(), 429);
    assert_eq!(StreamError::TooManyTranscodes.retry_after(), Some(5));
    assert_eq!(
        StreamError::TooManyTranscodes.client_message(),
        Some(TOO_MANY_TRANSCODES_MESSAGE)
    );
    assert_eq!(
        TOO_MANY_TRANSCODES_MESSAGE,
        "too many concurrent transcodes, please retry shortly"
    );
    assert_eq!(RETRY_AFTER_SECONDS, 5);
    // sendError maps a non-ErrNotFound error to ErrorGeneric (`api.go:297-311`).
    assert_eq!(
        StreamError::TooManyTranscodes.subsonic_error_code(),
        SUBSONIC_ERROR_GENERIC
    );
    assert_eq!(StreamError::NotFound.status_code(), 404);
    assert_eq!(StreamError::NotFound.client_message(), Some(NOT_FOUND));
    assert_eq!(StreamError::NotFound.subsonic_error_code(), 70);
    assert_eq!(StreamError::NotFound.retry_after(), None);

    // Go's abort arm writes no body: `panic(http.ErrAbortHandler)`.
    let aborted = StreamError::Aborted {
        written: 4096,
        message: "client went away".to_string(),
    };
    assert!(aborted.aborts_response());
    assert_eq!(aborted.client_message(), None);
    assert_eq!(aborted.status_code(), 500);
    assert!(!StreamError::ServeStart {
        message: "boom".to_string()
    }
    .aborts_response());

    // The 429 response Go's two handlers build (`api.go:319-323`,
    // `handle_streams.go:66-69`).
    let response = too_many_transcodes_response();
    assert_eq!(response.status, 429);
    assert_eq!(header(&response, "Retry-After"), Some("5"));
    assert_eq!(
        header(&response, "Content-Type"),
        Some(ERROR_CONTENT_TYPE),
        "http.Error sets text/plain; charset=utf-8 (server.go:2388)"
    );
    assert_eq!(
        header(&response, HEADER_CONTENT_TYPE_OPTIONS),
        Some(NOSNIFF),
        "the Subsonic handler set nosniff before Serve, so it is already on the writer"
    );
    assert_eq!(
        header(&response, "Content-Length"),
        None,
        "http.Error sets none"
    );
    assert_eq!(response.written, 0);
    let denied = response.error.expect("429 arm");
    assert!(denied.is_too_many_transcodes());
    assert_eq!(denied.subsonic_error_code(), SUBSONIC_ERROR_GENERIC);
    // Refused before anything launches: no child, no slot held.
    assert_eq!(response.pid, None);
    assert!(!response.child_reaped);
    assert!(!response.cleanup_pending);

    assert_eq!(INVALID_REQUEST, "invalid request");
    assert_eq!(INTERNAL_ERROR, "internal error");
    let io_error = StreamError::Io {
        message: "permission denied".to_string(),
        kind: io::ErrorKind::PermissionDenied,
    };
    assert_eq!(io_error.status_code(), 500);
    assert_eq!(io_error.client_message(), Some(INTERNAL_ERROR));
    assert_eq!(io_error.subsonic_error_code(), SUBSONIC_ERROR_GENERIC);
}

#[test]
fn share_handler_overrides_the_status_for_n_zero() {
    // `server/public/handle_streams.go:87-89`: `if err != nil || n == 0 {
    // http.Error(w, "internal error", 500) }`. Go's Subsonic handler instead
    // reports only `err` and leaves the status alone (`stream.go:52`).
    let mut response = too_many_transcodes_response();
    response.status = 200;
    response.written = 0;
    response.error = None;
    assert_eq!(response.share_handler_status(), 500);

    response.written = 1;
    assert_eq!(response.share_handler_status(), 200);

    response.error = Some(StreamError::NotFound);
    assert_eq!(response.share_handler_status(), 500);

    let empty = EmptyOutput {
        written: 0,
        log_message: empty_output_message(Some("audio/mpeg")),
        status: 500,
        client_message: INTERNAL_ERROR,
    };
    assert_eq!(empty.status, 500);
    assert_eq!(empty.client_message, INTERNAL_ERROR);
}

#[test]
fn headers_set_replaces_and_del_keeps_order() {
    let mut headers = ResponseHeaders::new();
    headers.set("Content-Type", "audio/mpeg");
    headers.set("Content-Length", "10");
    headers.set("Content-Type", "audio/flac");
    assert_eq!(
        headers
            .entries()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec!["Content-Type", "Content-Length"],
        "Set replaces in place and keeps insertion order, like Go's Header().Set"
    );
    headers.del("Content-Length");
    assert_eq!(headers.get("Content-Length"), None);
    assert!(
        headers.get("Nope").is_none(),
        "Get of an absent name is None"
    );
    headers.del("Absent");
    assert_eq!(headers.entries().len(), 1);
    assert!(!headers.is_empty());
    headers.del("Content-Type");
    assert!(headers.is_empty());
}

#[tokio::test]
async fn response_truncated_flag_tracks_the_abort_arm() {
    let mut response = too_many_transcodes_response();
    assert!(!response.truncated());
    response.error = Some(StreamError::Aborted {
        written: 1,
        message: "broken pipe".to_string(),
    });
    assert!(response.truncated(), "Go's panic(http.ErrAbortHandler) arm");
    response.error = Some(StreamError::TruncatedBeforeFirstByte {
        message: "sending transcoded file: eof".to_string(),
    });
    assert!(
        !response.truncated(),
        "0 bytes written keeps the status swappable (media_streamer.go:181-186)"
    );
}

// ---------------------------------------------------------------------------
// direct (seekable) body: Range and conditional semantics end to end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plain_get_serves_the_whole_file_with_go_headers() {
    let path = repo_fixture("test.mp3");
    let md = std::fs::metadata(&path).expect("fixture");
    let modified = http_date(
        md.modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    );
    let req = get_request(&path, media_file(&path, "mp3", 1.026756));

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.written as u64, md.len());
    assert_eq!(sink.len() as u64, md.len());
    assert_eq!(
        header(&response, "Content-Length"),
        Some(md.len().to_string().as_str())
    );
    assert_eq!(header(&response, "Accept-Ranges"), Some("bytes"));
    assert_eq!(header(&response, "Content-Type"), Some("audio/mpeg"));
    assert_eq!(header(&response, "Last-Modified"), Some(modified.as_str()));
    assert_eq!(header(&response, "Content-Range"), None);
    assert_eq!(header(&response, "ETag"), None, "Navidrome sets no ETag");
    assert_eq!(header(&response, HEADER_CONTENT_DURATION), Some("1.026756"));
    assert!(response.seekable, "a raw file is Go's Seekable arm");
    assert_eq!(response.error, None);
    assert!(response.reader_closed);
    assert_eq!(response.pid, None, "a raw stream launches no process");
    assert_eq!(response.share_handler_status(), 200);
}

#[tokio::test]
async fn range_request_answers_206_with_the_exact_window() {
    let path = repo_fixture("test.mp3");
    let size = std::fs::metadata(&path).expect("fixture").len();
    let whole = std::fs::read(&path).expect("fixture");

    let mut req = get_request(&path, media_file(&path, "mp3", 1.026756));
    req.cond.range = Some(format!("bytes={}-{}", size - 1000, size - 1));

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 206);
    assert_eq!(response.written, 1000);
    assert_eq!(sink.len(), 1000);
    assert_eq!(
        header(&response, "Content-Range"),
        Some(format!("bytes {}-{}/{}", size - 1000, size - 1, size).as_str())
    );
    assert_eq!(header(&response, "Content-Length"), Some("1000"));
    assert_eq!(header(&response, "Accept-Ranges"), Some("bytes"));
    // The window really is the tail of the file.
    assert_eq!(sink.as_slice(), &whole[size as usize - 1000..]);
}

#[tokio::test]
async fn unsatisfiable_range_answers_416_with_no_body() {
    let path = repo_fixture("test.mp3");
    let size = std::fs::metadata(&path).expect("fixture").len();

    let mut req = get_request(&path, media_file(&path, "mp3", 1.026756));
    req.cond.range = Some(format!("bytes={}-", size + 1));

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 416);
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    assert_eq!(
        header(&response, "Content-Range"),
        Some(format!("bytes */{size}").as_str())
    );
    assert_eq!(header(&response, "Content-Length"), None);
    assert_eq!(response.error, None, "Go returns no error for 416");
    assert_eq!(
        response.share_handler_status(),
        500,
        "n == 0 makes the share handler answer 500 even though Serve returned nil \
         — Go's behavior reproduced, not fixed"
    );
}

#[tokio::test]
async fn not_modified_answers_304_and_opens_no_file() {
    let path = repo_fixture("test.mp3");
    let md = std::fs::metadata(&path).expect("fixture");
    let modified = http_date(
        md.modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    );

    let mut req = get_request(&path, media_file(&path, "mp3", 1.026756));
    req.cond.if_modified_since = Some(modified.clone());

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 304);
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    assert_eq!(header(&response, "Content-Length"), None);
    assert_eq!(header(&response, "Content-Type"), None);
    assert_eq!(header(&response, "Last-Modified"), Some(modified.as_str()));
}

#[tokio::test]
async fn head_on_a_seekable_body_writes_headers_only() {
    let path = repo_fixture("test.mp3");
    let size = std::fs::metadata(&path).expect("fixture").len();

    let mut req = get_request(&path, media_file(&path, "mp3", 1.026756));
    req.cond.method = "HEAD".to_string();

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    assert_eq!(
        header(&response, "Content-Length"),
        Some(size.to_string().as_str()),
        "headers are still written (`fs.go:429-431` skips only the copy)"
    );
    assert_eq!(response.share_handler_status(), 500, "Go's n == 0 arm");
}

#[tokio::test]
async fn missing_media_file_is_404_not_found() {
    // `server/public/handle_streams.go:47-49` answers `not found`/404, which the
    // Subsonic router renders as ErrorDataNotFound (70).
    let path = repo_fixture("does-not-exist.mp3");
    let req = get_request(&path, MediaFile::default());

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 404);
    assert_eq!(header(&response, "Content-Type"), Some(ERROR_CONTENT_TYPE));
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    let error = response.error.clone().expect("404 arm");
    assert_eq!(error, StreamError::NotFound);
    assert_eq!(error.subsonic_error_code(), 70);
    assert_eq!(response.share_handler_status(), 500, "err != nil ⇒ 500");
}

#[tokio::test]
async fn a_directory_is_not_served() {
    // Go's `fileExists` directory arm (`ffmpeg.go:141-149`) fails before Serve;
    // the share handler's non-ErrNotFound path is a 500, its ErrNotFound a 404.
    let dir = repo_root().join("tests/fixtures");
    let req = get_request(&dir, MediaFile::default());

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.written, 0);
    assert!(
        sink.is_empty(),
        "a directory must never be streamed as a body"
    );
    assert_eq!(
        response.status, 404,
        "the port reports the directory arm as not-found, matching Go's \
         `'%s' is a directory` failure reaching the share handler's error path"
    );
}

#[tokio::test]
async fn estimate_content_length_header_only_for_a_transcode_pipe() {
    let path = repo_fixture("test.mp3");
    let mf = media_file(&path, "mp3", 300.0);
    let size = std::fs::metadata(&path).expect("fixture").len();

    // Raw: Go's seekable arm never uses the estimate (`fs.go:425-427`).
    let mut raw = get_request(&path, mf.clone());
    raw.estimate_content_length = true;
    let plan = handler(TranscodeLimiter::new(0, 0))
        .plan(&raw)
        .await
        .expect("plan");
    assert_eq!(
        plan.headers.get("Content-Length"),
        Some(size.to_string().as_str()),
        "the real size, not the estimate"
    );
    assert_eq!(plan.headers.get("Accept-Ranges"), Some("bytes"));

    // Transcoded pipe (`media_streamer.go:168-172`).
    let mut pipe = transcode_request(&path, mf.clone(), "mp3", 128);
    pipe.estimate_content_length = true;
    let plan = handler(TranscodeLimiter::new(0, 0))
        .plan(&pipe)
        .await
        .expect("plan");
    assert_eq!(
        plan.headers.get("Content-Length"),
        Some("4915200"),
        "the float32 estimate Go computes (`media_streamer.go:149`), pinned in \
         `content_type_lookup_and_small_helpers`"
    );
    assert_eq!(plan.headers.get("Accept-Ranges"), Some("none"));
    assert_eq!(plan.headers.get("Content-Type"), Some("audio/mpeg"));
    assert_eq!(plan.kind, BodyKind::Transcoded);
    assert!(!plan.kind.is_seekable());
    assert_eq!(plan.plan.status, 200);

    // Without the parameter, the pipe sends no Content-Length at all.
    pipe.estimate_content_length = false;
    let plan = handler(TranscodeLimiter::new(0, 0))
        .plan(&pipe)
        .await
        .expect("plan");
    assert_eq!(plan.headers.get("Content-Length"), None);

    // HEAD on a pipe: no Content-Length either (`plan_streaming`'s head arm).
    pipe.cond.method = "HEAD".to_string();
    pipe.estimate_content_length = true;
    let plan = handler(TranscodeLimiter::new(0, 0))
        .plan(&pipe)
        .await
        .expect("plan");
    assert_eq!(plan.headers.get("Content-Length"), None);
    assert!(plan.no_body());
}

#[tokio::test]
async fn content_encoding_omits_content_length_on_a_plain_200() {
    // `fs.go:426`: serveContent skips Content-Length when Content-Encoding is set.
    let path = repo_fixture("test.mp3");
    // A bare GET with no Range: `serveContent` reaches the plain-200 tail, where
    // Content-Encoding suppresses Content-Length. (With a Range the plan is a
    // 206, which always declares its length — asserted in `range.rs`.)
    let mut req = get_request(&path, media_file(&path, "mp3", 1.0));
    req.cond.range = None;
    req.content_encoding = Some("gzip".to_string());

    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(
        header(&response, "Content-Length"),
        None,
        "fs.go:426 — a plain 200 with Content-Encoding sends no length"
    );
    assert_eq!(header(&response, "Content-Encoding"), Some("gzip"));
    assert_eq!(response.written as u64, sink.len() as u64);
}

// ---------------------------------------------------------------------------
// 429 rate-limit arm: refused before anything launches
// ---------------------------------------------------------------------------

/// Hold one slot of `limiter` for `alice` without launching anything: the handler's
/// 429 arm is decided by the permit, before the launch, so the occupant does not
/// need to be a real transcode. The slot is released when the returned permit is
/// dropped, exactly as a confirmed reap would release it.
///
/// A process-based occupant would make the refusal assertions depend on ffmpeg:
/// a launch failure inside `open_transcode_gated` returns a `Body(Process)` error
/// instead of admitting the slot, and the "cap fired" claim below would be false.
fn hold_one_slot(
    limiter: &TranscodeLimiter,
    username: &str,
) -> nd_stream::limiter::TranscodePermit {
    limiter
        .acquire(username)
        .expect("the first slot is available before the cap is hit")
}

#[tokio::test]
async fn saturated_cap_answers_429_and_launches_nothing() {
    let limiter = TranscodeLimiter::new(1, 0);
    let held = hold_one_slot(&limiter, "alice");

    let path = repo_fixture("test.mp3");
    let req = transcode_request(&path, media_file(&path, "mp3", 1.0), "mp3", 128);

    let mut handler = handler(limiter.clone());
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 429);
    assert_eq!(header(&response, "Retry-After"), Some("5"));
    assert_eq!(header(&response, "Content-Type"), Some(ERROR_CONTENT_TYPE));
    assert_eq!(response.error, Some(StreamError::TooManyTranscodes));
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    assert_eq!(response.pid, None, "the refusal happens before the launch");
    assert!(!response.cleanup_pending);

    // The occupant still holds the slot: a direct acquire is refused too, so the
    // 429 above came from the cap rather than a launch failure. Dropping the permit
    // is what a confirmed reap amounts to, and only then is it grantable.
    assert!(
        limiter.acquire("bob").is_err(),
        "the held permit occupies the only slot"
    );
    drop(held);
    assert!(
        limiter.acquire("bob").is_ok(),
        "dropping the permit frees the slot"
    );
}

#[tokio::test]
async fn raising_the_cap_turns_the_429_into_a_served_transcode() {
    // Non-vacuity control for the test above: the same request against a cap that
    // admits it is served, so a 429 assertion proves the cap fired rather than a
    // launch failure, and a "served" assertion proves the limiter was consulted.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "cap-control", 60.0) {
        Some(path) => path,
        None => return,
    };

    let limiter = TranscodeLimiter::new(2, 0);
    let held = hold_one_slot(&limiter, "alice");
    // The refusal control needs its **own** occupied cap: a fresh limiter with
    // nothing held admits the request, which would make the 429 assertion vacuous.
    let strict = TranscodeLimiter::new(1, 0);
    let held_strict = hold_one_slot(&strict, "alice");

    let mut req = transcode_request(&source, media_file(&source, "mp3", 60.0), "mp3", 128);
    req.command = Some(endless_mp3_command(bin.as_str()));
    let mut handler = handler(limiter.clone());
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200, "cap 2: the second lane is admitted");
    assert_eq!(response.error, None);
    assert!(!sink.is_empty(), "the admitted transcode produced bytes");
    let pid = response.pid.expect("a transcode launched a child");
    assert!(
        !pid_alive(pid),
        "pid {pid} survived a serve that reached EOF — the handler must reap it"
    );

    // The identical request against a cap of 1 that is already occupied is
    // refused instead.
    let mut refused = self::handler(strict.clone());
    let (_tx2, rx2) = stays_connected();
    let mut sink2 = Vec::new();
    let denied = refused.serve(&req, &mut sink2, rx2).await;
    assert_eq!(denied.status, 429);
    assert_eq!(denied.pid, None);

    drop(held);
    drop(held_strict);
}

// ---------------------------------------------------------------------------
// transcode body: headers, empty output, HEAD, launch failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcode_streams_mp3_bytes_with_go_headers() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "mp3-serve", 60.0) {
        Some(path) => path,
        None => return,
    };

    let req = transcode_request(&source, media_file(&source, "mp3", 2.0), "mp3", 128);
    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(header(&response, "Accept-Ranges"), Some("none"));
    assert_eq!(header(&response, "Content-Type"), Some("audio/mpeg"));
    assert_eq!(
        header(&response, HEADER_CONTENT_DURATION),
        Some("2"),
        "float32 shortest decimal, as Go writes it"
    );
    assert_eq!(
        header(&response, HEADER_CONTENT_TYPE_OPTIONS),
        Some(NOSNIFF)
    );
    assert_eq!(header(&response, "Content-Length"), None, "estimate off");
    assert!(response.written > 0, "ffmpeg produced no bytes");
    assert_eq!(response.written as usize, sink.len());
    assert_eq!(response.error, None);
    assert!(!response.disconnected);
    assert!(!response.seekable);
    assert!(response.empty_output.is_none());
    assert!(response.child_reaped, "the handler reaps before returning");
    assert!(response.reader_closed);
    assert!(!response.cleanup_pending);
    let pid = response.pid.expect("transcode child pid");
    assert!(
        !pid_alive(pid),
        "serve returned after a confirmed reap, so pid {pid} must be gone"
    );
    assert_eq!(response.share_handler_status(), 200);
}

#[tokio::test]
async fn head_on_a_transcode_launches_drains_and_reports_zero_bytes() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "head-transcode", 60.0) {
        Some(path) => path,
        None => return,
    };

    let mut req = transcode_request(&source, media_file(&source, "mp3", 2.0), "mp3", 128);
    req.cond.method = "HEAD".to_string();

    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = DiscardSink;
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.written, 0,
        "Go's HEAD arm returns 0, nil (`media_streamer.go:174-177`)"
    );
    assert_eq!(response.share_handler_status(), 500, "n == 0 ⇒ 500");
    assert!(
        response.child_reaped,
        "the drained child is reaped, not left running"
    );
    let pid = response
        .pid
        .expect("HEAD still launches ffmpeg: NewStream runs first");
    assert!(!pid_alive(pid));
    assert_eq!(header(&response, "Accept-Ranges"), Some("none"));
    assert!(!response.cleanup_pending);
}

#[tokio::test]
async fn empty_output_is_reported_not_treated_as_a_stream_error() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "empty-output", 60.0) {
        Some(path) => path,
        None => return,
    };

    // An encode to the `null` muxer: ffmpeg runs to completion, exits 0, and
    // writes nothing to stdout. That is Go's `c == 0` arm (`media_streamer.go:190-193`) —
    // logged, not an error, but the share handler still answers 500 for `n == 0`.
    // A command that *fails* is the other arm: `ffCmd.wait` closes the pipe with
    // the exit error (`core/ffmpeg/ffmpeg.go:387-393`), so `io.Copy` errors and
    // the caller answers 500 (`failing_command_fails_at_serve_with_a_500_before_any_byte`).
    let mut req = transcode_request(&source, media_file(&source, "mp3", 1.0), "mp3", 128);
    req.command = Some(format!(
        "{bin} -hide_banner -loglevel error -i %s -map 0:a -f null -"
    ));

    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    let empty = response
        .empty_output
        .as_ref()
        .expect("zero-byte ffmpeg output must be reported as empty output");
    assert_eq!(empty.written, 0);
    assert_eq!(empty.status, 500);
    assert_eq!(empty.client_message, INTERNAL_ERROR);
    assert!(empty.log_message.contains("(format=audio/mpeg)"));
    assert_eq!(response.written, 0);
    assert_eq!(
        response.error, None,
        "Go does not treat empty output as an error"
    );
    assert_eq!(response.share_handler_status(), 500);
    assert!(response.child_reaped, "even an empty run leaves no child");
    assert!(!response.cleanup_pending);
}

#[tokio::test]
async fn failing_command_fails_at_serve_with_a_500_before_any_byte() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "bad-command", 60.0) {
        Some(path) => path,
        None => return,
    };

    // A codec ffmpeg cannot resolve: `NewStream` succeeds (the process launches,
    // `media_streamer.go:234-277`), then `ffCmd.wait` closes the stdout pipe with
    // the exit error (`core/ffmpeg/ffmpeg.go:384-393`) so `io.Copy` fails at 0
    // bytes. Go's Serve deletes Content-Length and returns
    // `sending transcoded file: …` (`media_streamer.go:181-185`), which the
    // callers turn into a 500 (`handle_streams.go:72`, `stream.go:52` → sendError).
    // It is a serve-time failure, not a launch failure.
    let mut req = transcode_request(&source, media_file(&source, "mp3", 1.0), "mp3", 128);
    req.command = Some(format!(
        "{bin} -hide_banner -loglevel error -i %s -c:a no-such-codec -f mp3 -"
    ));

    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 500);
    assert_eq!(header(&response, "Content-Type"), Some(ERROR_CONTENT_TYPE));
    assert_eq!(response.written, 0);
    let error = response.error.expect("serve-time failure at 0 bytes");
    assert!(
        matches!(error, StreamError::TruncatedBeforeFirstByte { .. }),
        "expected TruncatedBeforeFirstByte, got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("exited with non-zero status code: 8"),
        "Go's ffCmd.wait message reaches the caller: {error}"
    );
    assert_eq!(error.client_message(), Some(INTERNAL_ERROR));
    assert!(!response.cleanup_pending);
    if let Some(pid) = response.pid {
        assert!(!pid_alive(pid), "a failed launch leaves no surviving child");
    }
}

#[tokio::test]
async fn unknown_format_with_no_command_refuses_before_spawning() {
    // Go's `LookupTranscodeCommand` returning "" (`media_streamer.go:228-231`)
    // means no ffmpeg launches at all, so this arm cannot leak a process.
    let path = repo_fixture("test.mp3");
    let mut req = transcode_request(&path, media_file(&path, "mp3", 1.0), "totally-unknown", 128);
    req.command = Some(String::new());

    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 500);
    assert_eq!(response.pid, None, "nothing was spawned");
    assert_eq!(response.written, 0);
    assert!(!response.cleanup_pending);
}

#[tokio::test]
async fn per_user_cap_refuses_the_second_request_from_the_same_user() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "per-user", 900.0) {
        Some(path) => path,
        None => return,
    };

    let limiter = TranscodeLimiter::new(0, 1);
    let mut req = transcode_request(&source, media_file(&source, "mp3", 900.0), "mp3", 128);
    req.command = Some(endless_mp3_command(bin.as_str()));

    let mut first = handler(limiter.clone());
    let (tx1, rx1) = stays_connected();
    let mut sink1 = ChannelSink::default();
    let mut client1 = sink1.take_receiver().expect("receiver");
    let serve1 = first.serve(&req, &mut sink1, rx1);
    tokio::pin!(serve1);

    // Hold the slot open until bytes are flowing, then hang up.
    let mut seen = 0usize;
    while seen < 64 * 1024 {
        tokio::select! {
            chunk = client1.recv() => match chunk {
                Some(chunk) => seen += chunk.len(),
                None => break,
            },
            done = &mut serve1 => {
                let _ = done;
                break;
            }
        }
    }
    assert!(
        seen >= 64 * 1024,
        "first lane sent {seen} bytes: it must still hold its slot when the second         request arrives"
    );
    drop(client1);
    // Closing the socket *and* signalling the hang-up: `HangUp` is a oneshot
    // receiver, so the cancel arm only runs once the sender goes away (Go's
    // `r.Context()` being cancelled, `media_streamer.go:136`). Dropping the
    // receiver alone would only make the write fail, which is a different arm.
    drop(tx1);
    let mut second = handler(limiter.clone());
    let (_tx2, rx2) = stays_connected();
    let mut sink2 = Vec::new();
    let denied = second.serve(&req, &mut sink2, rx2).await;
    assert_eq!(denied.status, 429, "per-user cap of 1 while the first runs");
    assert_eq!(denied.pid, None);

    let r1 = serve1.await;
    assert!(r1.disconnected);
    assert!(r1.child_reaped);
    assert!(!r1.cleanup_pending);
    let pid = r1.pid.expect("first child");
    assert!(!pid_alive(pid));
    assert!(limiter.acquire("bob").is_ok(), "slot returned to the pool");
}

// ---------------------------------------------------------------------------
// THE acceptance criterion: a client that hangs up leaves no surviving child
// ---------------------------------------------------------------------------

/// Serve a real transcode, watch bytes arrive, hang up mid-body, and assert the
/// handler path left **zero** surviving processes.
///
/// What would fail if the kill path were removed:
///   - `GatedTranscode::serve` moves the disconnect future into the write loop
///     (`transcode_body::serve_stream`), polls it `biased` against both the read
///     and the write, and on a disconnect calls `child.cancel()` — which
///     `killpg`s the process **group**, then waits for the reap. Remove that
///     cancel and the child keeps encoding into a closed pipe: `written` stays
///     low, `child_reaped` is false, and `pid_alive(pid)` remains true for the
///     seconds of remaining encode work, so `assert_no_survivors` below fails.
///   - If the handler skipped its own teardown instead, `cleanup_pending` /
///     `handed_off_to_tracker` would be set and the same pid assertion fails.
///   - `Drop` is deliberately **not** what is proven: the assertions run on the
///     `StreamResponse` after `serve` returned, while the gate is still owned
///     inside the served future's scope, so a `Drop`-only kill cannot satisfy
///     them.
async fn disconnect_mid_body(lifetime_secs: f64) -> StreamResponse {
    let bin = ffmpeg_bin().expect("ffmpeg required for the lifecycle lane");
    let name = format!("disconnect-{}", lifetime_secs as u64);
    let source = make_source(bin.as_str(), name.as_str(), lifetime_secs)
        .expect("ffmpeg must encode the fixture (needs libmp3lame)");

    let limiter = TranscodeLimiter::new(4, 2);
    let mut req = transcode_request(
        &source,
        media_file(&source, "mp3", lifetime_secs),
        "mp3",
        128,
    );
    req.command = Some(endless_mp3_command(bin.as_str()));

    let mut handler = handler(limiter);
    let (tx, rx) = stays_connected();
    let mut sink = ChannelSink::default();
    let mut client = sink.take_receiver().expect("receiver");

    // Serve and the client run concurrently in one task (the seam's BoxFuture is
    // not `Send`, so `tokio::spawn` cannot carry a serve future over a cached
    // handler; this lane does not need to).
    let serve = handler.serve(&req, &mut sink, rx);
    tokio::pin!(serve);

    // The encode must really be in progress: bytes have to reach the client
    // before it hangs up, otherwise this is a disconnect-before-first-byte test.
    let mut seen = 0usize;
    while seen < 64 * 1024 {
        tokio::select! {
            chunk = client.recv() => match chunk {
                Some(chunk) => seen += chunk.len(),
                // The handler finished on its own: not a mid-body disconnect, so
                // the assertion below fails rather than passing quietly.
                None => break,
            },
            done = &mut serve => {
                let _ = done;
                break;
            }
        }
    }
    assert!(
        seen >= 64 * 1024,
        "client received {seen} bytes before hanging up; the encode finished too         fast for a mid-body hang-up"
    );
    drop(client); // socket closed mid-body
    drop(tx); // and the hang-up signal itself, which is what `HangUp` polls

    serve.await
}

#[tokio::test]
async fn client_disconnect_mid_body_leaves_zero_surviving_children() {
    let response = disconnect_mid_body(900.0).await;

    assert!(
        response.disconnected,
        "the write loop saw the client go away"
    );
    assert!(
        response.written > 0 && response.written < 9_000_000,
        "mid-body: {} bytes went on the wire, far short of the full encode",
        response.written
    );
    assert_eq!(
        response.error, None,
        "a cancellation is not a stream failure: Go reports an io.Copy error only \
         for a real failure (media_streamer.go:181-188)"
    );
    assert!(response.child_reaped, "the kill was confirmed, not assumed");
    assert!(
        !response.cleanup_pending,
        "no unresolved teardown: cleanup_error={:?}",
        response.cleanup_error
    );
    assert!(!response.handed_off_to_tracker);
    assert!(response.reader_closed);

    let pid = response.pid.expect("a real ffmpeg child was launched");
    assert!(
        !pid_alive(pid),
        "ACCEPTANCE: pid {pid} survived the handler — the kill path is broken. \
         Removing the cancel-on-disconnect in GatedTranscode::serve leaves this \
         child encoding into a closed pipe and fails right here."
    );
    assert_eq!(
        survivors(&[pid]),
        Vec::<u32>::new(),
        "zero surviving children"
    );
}

#[tokio::test]
async fn no_children_accumulate_across_repeated_disconnects() {
    // The leak class this lane exists to prevent: N requests, N hang-ups, zero
    // survivors, and the pool never saturates. With the kill path removed this
    // leaves N live ffmpeg processes and eventually refuses the cap, so both the
    // liveness list and the final acquire fail.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "repeat-disconnect", 300.0) {
        Some(path) => path,
        None => return,
    };

    let limiter = TranscodeLimiter::new(2, 2);
    let mut pids = Vec::new();
    for round in 0..4 {
        let mut req = transcode_request(&source, media_file(&source, "mp3", 300.0), "mp3", 128);
        req.command = Some(endless_mp3_command(bin.as_str()));

        let mut handler = handler(limiter.clone());
        let (tx, rx) = stays_connected();
        let mut sink = ChannelSink::default();
        let mut client = sink.take_receiver().expect("receiver");
        let serve = handler.serve(&req, &mut sink, rx);
        tokio::pin!(serve);

        let chunk = tokio::select! {
            chunk = client.recv() => chunk,
            done = &mut serve => {
                let _ = done;
                panic!("round {round}: the handler finished before the client could hang up")
            }
        }
        .expect("round produces bytes before the handler finished");
        assert!(!chunk.is_empty(), "round produced an empty chunk");
        drop(client);
        drop(tx); // the hang-up signal, which is what drives the cancel arm
        let response = serve.await;
        assert!(
            response.disconnected,
            "round {round} must have hung up mid-body"
        );
        assert!(
            response.child_reaped,
            "round {round}: reap not confirmed (cleanup_pending={}, handed_off={}, error={:?})",
            response.cleanup_pending, response.handed_off_to_tracker, response.cleanup_error
        );
        pids.push(response.pid.unwrap_or_else(|| {
            panic!("round {round} launched no child, so the leak cannot be measured")
        }));
    }

    let live = survivors(&pids);
    assert!(
        live.is_empty(),
        "ACCEPTANCE: {live:?} of {} ffmpeg children survived 4 disconnects",
        pids.len()
    );
    assert!(
        limiter.acquire("alice").is_ok(),
        "the pool must be empty again after every lane was reaped"
    );
}

#[tokio::test]
async fn disconnect_during_the_first_write_still_kills_the_group() {
    // Go's arm where ffmpeg produced bytes but the very first flush fails: the
    // 200 is already chosen, so cancellation is the only signal and the child
    // must still die. `DisconnectingSink` fails on the first write, the race
    // `serve_stream` documents.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "disconnect-first-write", 900.0) {
        Some(path) => path,
        None => return,
    };

    let limiter = TranscodeLimiter::new(4, 2);
    let mut req = transcode_request(&source, media_file(&source, "mp3", 900.0), "mp3", 128);
    req.command = Some(endless_mp3_command(bin.as_str()));

    let mut handler = handler(limiter.clone());
    let (_tx, rx) = stays_connected();
    let mut sink = DisconnectingSink::new(0);
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.written, 0);
    let pid = response.pid.expect("ffmpeg launched");
    assert!(
        !pid_alive(pid),
        "pid {pid} survived a write failure at 0 bytes — teardown must kill it"
    );
    assert!(
        response.child_reaped,
        "the kill is confirmed by the teardown"
    );
    assert!(!response.cleanup_pending);
    assert!(
        limiter.acquire("alice").is_ok(),
        "the slot came back after the confirmed reap"
    );
}

#[tokio::test]
async fn dropping_the_serve_future_leaves_no_child() {
    // Go's handlers `defer stream.Close()` even when the request context is
    // already cancelled; the port hands an unreaped child to a detached tracker
    // instead of concluding the process is gone. Assert the tracker really
    // finishes it — by pid, not by trusting Drop.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "drop-future", 900.0) {
        Some(path) => path,
        None => return,
    };

    let limiter = TranscodeLimiter::new(4, 2);
    let mut req = transcode_request(&source, media_file(&source, "mp3", 900.0), "mp3", 128);
    req.command = Some(endless_mp3_command(bin.as_str()));

    // The client stays connected — an unbounded sink accepts every write — so
    // the only teardown trigger here is the future being abandoned. Dropping the
    // receiver instead would drive the disconnect arm, which is the test above.
    let (_tx, rx) = stays_connected();
    let mut sink = ChannelSink::default();
    let client = sink.take_receiver().expect("receiver");

    let baseline = ffmpeg_children();
    let pid = {
        let mut handler = handler(limiter.clone());
        let serve = handler.serve(&req, &mut sink, rx);
        tokio::pin!(serve);
        // Poll in slices so the child can be observed in the process table while
        // the encode is in flight: a single 300 ms poll can complete (and be
        // reaped) before the scan runs, which would make "no child appeared" a
        // false failure rather than a leak check.
        let mut found = None;
        for _ in 0..20 {
            tokio::select! {
                _ = serve.as_mut() => break,
                () = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
            let new = ffmpeg_children()
                .into_iter()
                .find(|pid| !baseline.contains(pid));
            if let Some(pid) = new {
                found = Some(pid);
                break;
            }
        }
        // `serve` and `handler` are dropped at the end of this block.
        found.expect("an ffmpeg child launched and was observed mid-encode")
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while pid_alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        !pid_alive(pid),
        "ACCEPTANCE: pid {pid} outlived the dropped serve future — the handoff \
         tracker neither killed nor reaped it"
    );
    drop(client);
}

// ---------------------------------------------------------------------------
// cache seam: what the transcoding-file cache lane plugs into
// ---------------------------------------------------------------------------

/// Stand-in for a **complete** cache entry: a seekable file. Go's
/// `cache.FileCacheItem` with a `Seeker` makes `Stream.Seekable()` true
/// (`media_streamer.go:160-162`), so the handler re-plans it as a Range body.
struct SeekableCacheEntry {
    path: PathBuf,
}

impl CachedTranscode for SeekableCacheEntry {
    fn seekable(&self) -> bool {
        true
    }

    fn path(&self) -> Option<&Path> {
        Some(self.path.as_path())
    }

    fn read_chunk<'a>(&'a mut self, _max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>> {
        Box::pin(async { Ok(None) })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// Stand-in for an **in-progress** cache entry: the pipe Go's producer is still
/// filling (`media_streamer.go:226-282`).
struct PipeCacheEntry {
    chunks: Vec<Vec<u8>>,
    next: usize,
    closed: Arc<Mutex<bool>>,
}

impl CachedTranscode for PipeCacheEntry {
    fn read_chunk<'a>(&'a mut self, _max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            if self.next >= self.chunks.len() {
                return Ok(None);
            }
            let chunk = self.chunks[self.next].clone();
            self.next += 1;
            Ok(Some(chunk))
        })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>> {
        let closed = Arc::clone(&self.closed);
        Box::pin(async move {
            *closed.lock().unwrap() = true;
            Ok(())
        })
    }
}

/// A source that answers from a fixed entry, launching nothing.
struct FixedSource<D> {
    entry: Option<Box<dyn CachedTranscode>>,
    _marker: std::marker::PhantomData<D>,
}

impl<D> TranscodeSource<D> for FixedSource<D>
where
    D: Send + 'static,
{
    fn open<'a>(
        &'a mut self,
        _mf: &'a MediaFile,
        _job: &'a TranscodeJob,
        _user: &'a str,
        disconnect: Disconnect<D>,
    ) -> BoxFuture<'a, Result<TranscodeLease<D>, GateError>> {
        let entry = self.entry.take().expect("cache entry served once");
        let signal = match disconnect {
            // A cached body binds nothing to a process, so the signal returns to
            // the caller's pump loop.
            Disconnect::When(signal) => Some(signal),
            Disconnect::Never => None,
        };
        Box::pin(async move {
            Ok(TranscodeLease::Cached {
                body: entry,
                signal,
            })
        })
    }
}

#[tokio::test]
async fn complete_cache_entry_is_served_as_a_seekable_body() {
    let path = repo_fixture("test.mp3");
    let size = std::fs::metadata(&path).expect("fixture").len();

    let mut req = transcode_request(&path, media_file(&path, "mp3", 1.0), "mp3", 128);
    req.cond.range = Some("bytes=0-999".to_string());
    let mut handler = StreamHandler::<HangUp>::new(Box::new(FixedSource {
        entry: Some(Box::new(SeekableCacheEntry { path: path.clone() })),
        _marker: std::marker::PhantomData,
    }));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    // Go's seekable arm hands the cached file to `http.ServeContent`
    // (`media_streamer.go:159-162`), which honors the Range the request carries —
    // so the observable point of this test is that the cached body is seekable and
    // range-addressable, not that it answers a bare 200.
    assert_eq!(
        response.status, 206,
        "the Range is honored like ServeContent's"
    );
    assert!(response.seekable);
    assert_eq!(
        response.written as u64, 1000,
        "exactly the requested window, not the whole file"
    );
    assert_eq!(sink.len() as u64, 1000);
    assert_eq!(header(&response, "Accept-Ranges"), Some("bytes"));
    let expected_range = format!("bytes 0-999/{size}");
    assert_eq!(
        header(&response, "Content-Range"),
        Some(expected_range.as_str()),
        "the window plus total size, as ServeContent writes it (`fs.go:1013-1016`)"
    );
    assert_eq!(header(&response, "Content-Length"), Some("1000"));
    assert_eq!(response.pid, None, "a cache hit launches no process");
    assert_eq!(response.error, None);
}

#[tokio::test]
async fn in_progress_cache_entry_streams_as_a_pipe_and_closes() {
    let path = repo_fixture("test.mp3");
    let closed = Arc::new(Mutex::new(false));
    let mut req = transcode_request(&path, media_file(&path, "mp3", 1.0), "mp3", 128);
    req.estimate_content_length = true;

    let mut handler = StreamHandler::<HangUp>::new(Box::new(FixedSource {
        entry: Some(Box::new(PipeCacheEntry {
            chunks: vec![vec![1u8; 1000], vec![2u8; 500]],
            next: 0,
            closed: Arc::clone(&closed),
        })),
        _marker: std::marker::PhantomData,
    }));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.written, 1500);
    assert_eq!(sink.len(), 1500);
    assert!(!response.seekable);
    assert_eq!(header(&response, "Accept-Ranges"), Some("none"));
    assert_eq!(header(&response, "Content-Length"), Some("16384"));
    assert_eq!(response.pid, None);
    assert!(
        *closed.lock().unwrap(),
        "the handler always closes the body"
    );
}

#[tokio::test]
async fn cached_pipe_honours_the_disconnect_without_a_process() {
    let path = repo_fixture("test.mp3");
    let closed = Arc::new(Mutex::new(false));
    let mut handler = StreamHandler::<HangUp>::new(Box::new(FixedSource {
        entry: Some(Box::new(PipeCacheEntry {
            chunks: vec![vec![7u8; 4096]],
            next: 0,
            closed: Arc::clone(&closed),
        })),
        _marker: std::marker::PhantomData,
    }));
    let req = transcode_request(&path, media_file(&path, "mp3", 1.0), "mp3", 128);
    let (tx, rx) = stays_connected();
    drop(tx); // client gone before the first chunk
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert!(response.disconnected);
    assert_eq!(response.written, 0);
    assert_eq!(response.pid, None);
    assert!(*closed.lock().unwrap());
}

#[tokio::test]
async fn a_source_that_refuses_with_cancellation_required_is_500() {
    // Go forces the request context onto the process whenever the cap is enabled
    // (`media_streamer.go:243-260`); a source that declares otherwise is refused
    // rather than served uncancellable.
    struct Refusing<D>(std::marker::PhantomData<D>);

    impl<D> TranscodeSource<D> for Refusing<D>
    where
        D: Send + 'static,
    {
        fn open<'a>(
            &'a mut self,
            _mf: &'a MediaFile,
            _job: &'a TranscodeJob,
            _user: &'a str,
            _disconnect: Disconnect<D>,
        ) -> BoxFuture<'a, Result<TranscodeLease<D>, GateError>> {
            Box::pin(async { Err(GateError::CancellationRequired) })
        }
    }

    let path = repo_fixture("test.mp3");
    let req = transcode_request(&path, media_file(&path, "mp3", 1.0), "mp3", 128);
    let mut handler = StreamHandler::<HangUp>::new(Box::new(Refusing(std::marker::PhantomData)));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    assert_eq!(response.status, 500);
    assert_eq!(
        response.error,
        Some(StreamError::CancellationRequired),
        "uncancellable-with-cap is refused, not silently served"
    );
    assert_eq!(response.pid, None);
    assert_eq!(response.share_handler_status(), 500);
}

// ---------------------------------------------------------------------------
// teardown reporting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_state_reports_reader_and_reap_separately() {
    let path = repo_fixture("test.mp3");
    let req = get_request(&path, media_file(&path, "mp3", 1.0));
    let mut handler = handler(TranscodeLimiter::new(0, 0));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    let state: CloseState = response.close_state();
    assert!(state.reader_closed, "the handler always closes the reader");
    assert!(!state.has_child, "a raw stream has no child to reap");
    assert!(
        !state.child_reaped,
        "nothing was reaped because nothing ran"
    );
    assert!(!state.cleanup_pending);
    assert!(state.cleanup_error.is_none());
    assert!(!state.handed_off_to_tracker);
}

#[tokio::test]
async fn a_served_transcode_reports_its_pid_and_reap() {
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(bin.as_str(), "close-state", 60.0) {
        Some(path) => path,
        None => return,
    };

    let req = transcode_request(&source, media_file(&source, "mp3", 1.0), "mp3", 128);
    let mut handler = handler(TranscodeLimiter::new(2, 1));
    let (_tx, rx) = stays_connected();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, rx).await;

    let state = response.close_state();
    assert!(state.reader_closed);
    assert!(state.has_child, "a transcode launched a process");
    assert!(
        state.child_reaped,
        "EOF reached, so the handler reaped it: cleanup_error={:?}",
        state.cleanup_error
    );
    assert!(!state.cleanup_pending);
    assert!(!state.handed_off_to_tracker);
}
