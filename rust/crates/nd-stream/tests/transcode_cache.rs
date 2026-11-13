//! Tests for [`nd_stream::transcode_cache`] — the transcoding-file cache lane.
//!
//! Two layers:
//!   1. **Cache contract**, driven without ffmpeg: `Cached` semantics, fan-out from
//!      offset 0, the completion marker, the failure arms, and the haunter's eviction
//!      rule. Each is pinned to the Go sources (`utils/cache/file_caches.go`,
//!      `spread_fs.go`, `file_haunter.go`) and their tests (`utils/cache/
//!      file_caches_test.go`).
//!   2. **Process lifecycle through the handler**: one ffmpeg per identical key, the
//!      writer's byte count, the first requester's disconnect killing the group while
//!      a late reader cannot, a 429 that is not a cache error, and HEAD parity.
//!
//! Fixture paths derive from `CARGO_MANIFEST_DIR`: scratch lives under
//! `target/transcode-cache-tests/`, never an absolute path. Optional tools (ffmpeg)
//! skip rather than fail, per the house style in `tests/handler.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use nd_stream::handler::{CachedTranscode, StreamHandler, StreamRequest};
use nd_stream::limiter::{TranscodeLimiter, RETRY_AFTER_SECONDS};
use nd_stream::range::CondRequest;
use nd_stream::transcode_body::TranscodeJob;
use nd_stream::transcode_cache::{
    key_mapper, live_pollers, marker_path, stream_cache_key, CacheLease, CacheTranscodeSource,
    Opened, SharedDisconnect, TranscodeCache, CACHE_READ_CHUNK_BYTES,
    DEFAULT_CACHE_CLEAN_UP_INTERVAL, KEY_HASH_HEX_LEN,
};
use nd_stream::types::{MediaFile, Request};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

/// Scratch dir under the crate's own `target/`, never an absolute path outside the
/// checkout.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("transcode-cache-tests")
        .join(name);
    // Start empty: a completed entry left by an earlier run would be adopted by
    // `open_key` and turn "first Get is the writer" into a replay.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("scratch dir under target/");
    dir
}

/// A cache rooted in scratch with the given budget.
fn cache(name: &str, size: &str, max_items: usize) -> Arc<TranscodeCache> {
    Arc::new(
        TranscodeCache::new(
            scratch_dir(name),
            size,
            max_items,
            DEFAULT_CACHE_CLEAN_UP_INTERVAL,
        )
        .expect("cache root under target/"),
    )
}

/// ffmpeg binary, or `None` (skip-if-missing: ffmpeg is on the dev box but is not
/// guaranteed on every CI host).
fn ffmpeg_bin() -> Option<String> {
    let (bin, ok) = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
    if ok {
        Some(bin)
    } else {
        None
    }
}

/// Encode a source of `secs` seconds, so the transcode under test decodes a real
/// file. `None` when this ffmpeg build lacks the encoder.
fn make_source(bin: &str, name: &str, secs: f64) -> Option<PathBuf> {
    let dir = scratch_dir(&format!("fixture-{name}"));
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

/// A template command that keeps producing for the whole encode: no `%b`, so the
/// requested bitrate cannot bound it. Long sources keep the entry observable while
/// it is still in flight.
fn endless_mp3_command(bin: &str) -> String {
    format!("{bin} -hide_banner -loglevel error -i %s -f mp3 -")
}

/// Is `pid` running? A zombie does not count: it terminated, and only a reaped wait
/// clears it. Same probe `tests/handler.rs` uses.
fn pid_alive(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .expect("ps must be available for process lifecycle assertions");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

fn survivors(pids: &[u32]) -> Vec<u32> {
    pids.iter().copied().filter(|pid| pid_alive(*pid)).collect()
}

/// Every live ffmpeg child of this test process. Tests run in parallel in one
/// process, so callers take a **baseline** and then require pids that were not in it.
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

/// ffmpeg processes and poller tasks are process-wide, so every test that inspects
/// children or poller counts holds this lock. Without it, a sibling test's ffmpeg
/// would show up in another test's child set. An async mutex: the guard is
/// deliberately held across the serve awaits.
static PROCESS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Poller tasks live in this process across all parallel tests, so a leak check
/// compares against the count captured when the test started.
fn poller_baseline() -> usize {
    live_pollers()
}

/// Assert that no poller task started by **this** test is still running.
///
/// Contract, and it is load-bearing: the caller must hold [`PROCESS_LOCK`] for
/// the whole serve **and** must have sampled `base` after acquiring it. The
/// counter is process-wide, so a baseline taken while a sibling holds the lock
/// can include that sibling's live poller and then fail forever — a task the
/// sibling owns can never exit from inside this test. Sampling under the lock
/// makes `base` the quiet count.
///
/// This helper deliberately does **not** acquire [`PROCESS_LOCK`] itself:
/// `tokio::sync::Mutex` is not reentrant, so a nested `lock()` here would
/// self-deadlock against the caller's guard and hang the suite rather than
/// report a leak.
///
/// A task that has decided to exit is only removed once the scheduler reaps it,
/// so the settle loop waits for that; a task that never exits never settles and
/// this fails.
async fn assert_no_poller_leak(base: usize, what: &str) {
    for _ in 0..100 {
        if live_pollers() == base {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(live_pollers(), base, "{what}");
}

/// Wait until a disconnect observer's poller task has finished, so a leak assertion
/// is deterministic rather than a race against the runtime's scheduler.
async fn wait_for_poller(observer: &SharedDisconnect) {
    for _ in 0..600 {
        if observer.poller_exited() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the disconnect poller task never exited");
}

/// Wait until the entry for `key` has left the in-flight set, i.e. the drain task
/// finalized it. The producer runs in the background (Go's `go fc.copyAndClose`), so
/// a test must not assume the marker/file state is settled when `serve` returns.
async fn wait_for_entry_gone(arc: &TranscodeCache, key: &str) {
    for _ in 0..600 {
        if arc.in_flight(key).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the entry for key {key:?} never finalized");
}

/// Live ffmpeg children that were not in `baseline`.
fn new_children(baseline: &[u32]) -> Vec<u32> {
    ffmpeg_children()
        .into_iter()
        .filter(|pid| !baseline.contains(pid))
        .collect()
}

/// Media row for a file on disk.
fn media_file(path: &Path, id: &str, suffix: &str, duration: f64) -> MediaFile {
    let size = std::fs::metadata(path).map(|md| md.len()).unwrap_or(0);
    MediaFile {
        id: id.to_string(),
        suffix: suffix.to_string(),
        bit_rate: 128,
        sample_rate: 44_100,
        duration,
        size: size as i64,
        updated_at: 1_700_000_000,
        ..MediaFile::default()
    }
}

/// A transcode request: format + bitrate, what the decider hands the handler.
fn transcode_request(path: &Path, mf: MediaFile, format: &str, bit_rate: i64) -> StreamRequest {
    StreamRequest {
        cond: CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        },
        media_file: mf,
        request: Request {
            format: format.to_string(),
            bit_rate,
            ..Request::default()
        },
        path: path.display().to_string(),
        username: "alice".to_string(),
        ..StreamRequest::default()
    }
}

/// The job the cache keys on for a request, with the command the handler would use.
fn transcode_job(req: &StreamRequest, command: &str) -> TranscodeJob {
    TranscodeJob {
        file_path: req.path.clone(),
        format: req.request.format.clone(),
        bit_rate: req.request.bit_rate,
        sample_rate: req.request.sample_rate,
        bit_depth: req.request.bit_depth,
        channels: req.request.channels,
        offset: req.request.offset,
        command: command.to_string(),
    }
}

/// Handler over the cache-backed source. `D` is [`SharedDisconnect`]: a caller with
/// its own disconnect future turns it into one observer, which is the single task
/// that polls the future (Go's request context inside the stream,
/// `media_streamer.go:66-67`).
fn cache_handler(
    arc: Arc<TranscodeCache>,
    limiter: TranscodeLimiter,
    dedupe: bool,
) -> StreamHandler<SharedDisconnect> {
    StreamHandler::new(Box::new(
        CacheTranscodeSource::new(arc, limiter).with_dedupe(dedupe),
    ))
}

/// A client that never hangs up: the sender is returned so a test can hang up.
fn stays_connected() -> (tokio::sync::oneshot::Sender<()>, HangUp) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    (tx, HangUp(rx))
}

/// The caller's disconnect. A dropped sender means the socket closed, which is what
/// Go's `r.Context()` cancellation is.
struct HangUp(tokio::sync::oneshot::Receiver<()>);

impl std::future::Future for HangUp {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        match std::pin::Pin::new(&mut self.0).poll(cx) {
            std::task::Poll::Ready(_) => std::task::Poll::Ready(()),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl HangUp {
    /// Wrap it in the observer the cache lane and handler share. Spawns exactly one
    /// poller task; [`live_pollers`] counts it.
    fn observer(self) -> SharedDisconnect {
        SharedDisconnect::spawn(self)
    }
}

/// Read a body to completion, collecting bytes; a failure comes back as its text.
async fn drain(body: &mut impl CachedTranscode) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        match body.read_chunk(CACHE_READ_CHUNK_BYTES).await {
            Ok(None) => return Ok(out),
            Ok(Some(bytes)) => out.extend_from_slice(&bytes),
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// Wait until the entry for `key` reports a pid, so a test attaches mid-write with a
/// happens-before instead of a sleep.
async fn wait_for_pid(arc: &TranscodeCache, key: &str) -> u32 {
    for _ in 0..600 {
        if let Some(entry) = arc.in_flight(key) {
            if let Some(pid) = entry.pid() {
                return pid;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the cache entry never launched a producer for key {key:?}");
}

/// Drive a pinned serve future until the cache entry reports its producer pid, so a
/// second request provably attaches **mid-write** rather than after completion.
async fn drive_until_pid<F>(serve: &mut F, arc: &TranscodeCache, key: &str)
where
    // `F` is `Pin<&mut impl Future>` from the caller, which is `Unpin`; the seam's
    // own `BoxFuture` is not `Send`, which is why none of this is spawned.
    F: std::future::Future + std::marker::Unpin,
{
    for _ in 0..600 {
        if let Some(entry) = arc.in_flight(key) {
            if entry.pid().is_some() {
                return;
            }
        }
        tokio::select! {
            biased;
            _ = &mut *serve => return,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
    panic!("the entry for key {key:?} never launched a producer");
}

/// A sink whose bytes live behind an `Arc`, so a test can read them after the serve
/// future (which borrows the sink) is still alive. `Vec<u8>` as a `Sink`
/// (`handler.rs:229`) keeps the borrow until its future is dropped, which would
/// otherwise conflict with the assertions.
#[derive(Clone)]
struct CollectSink(Arc<std::sync::Mutex<Vec<u8>>>);

impl CollectSink {
    fn new() -> (Self, Arc<std::sync::Mutex<Vec<u8>>>) {
        let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        (CollectSink(Arc::clone(&buf)), buf)
    }

    fn bytes(buf: &Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<u8> {
        buf.lock().expect("sink buffer").clone()
    }
}

impl nd_stream::handler::Sink for CollectSink {
    fn write_all<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> nd_stream::archiver::BoxFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0.lock().expect("sink buffer").extend_from_slice(data);
            Ok(())
        })
    }
}

/// Serve one request in the caller's task, returning response plus body bytes.
///
/// The seam's [`nd_stream::archiver::BoxFuture`] is not `Send`, so `tokio::spawn`
/// cannot carry a serve future over a handler (`tests/handler.rs:1375-1378` documents
/// the same constraint). Concurrency in this file is `tokio::select!` / `tokio::join!`
/// inside one task, which is enough to make two requests overlap while an entry is in
/// flight.
async fn serve_one(
    arc: Arc<TranscodeCache>,
    limiter: TranscodeLimiter,
    req: &StreamRequest,
    hang_up: HangUp,
    hang_up_switch: tokio::sync::oneshot::Sender<()>,
    dedupe: bool,
) -> (nd_stream::handler::StreamResponse, Vec<u8>) {
    let mut handler = cache_handler(arc, limiter, dedupe);
    let mut sink = Vec::new();
    let response = handler.serve(req, &mut sink, hang_up.observer()).await;
    // The request is over: dropping the sender resolves `D`, which is what Go's
    // request context does when the handler returns (`media_streamer.go:66-67`) and
    // what lets the disconnect poller task return.
    drop(hang_up_switch);
    (response, sink)
}

// ---------------------------------------------------------------------------
// key + spread layout, pinned against Go's own output
// ---------------------------------------------------------------------------

/// Go oracle: `core/stream/media_streamer.go:59-61` `Key()` plus
/// `utils/cache/spread_fs.go:177-186` `KeyMapper`. Regenerate with
///
/// ```text
/// TZ=UTC go run ./rust/crates/nd-stream/tests/testdata/go_stream_key \
///   "$PWD/rust/crates/nd-stream/tests/data/go_stream_key"
/// ```
fn key_oracle() -> Vec<(MediaFile, TranscodeJob, String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/go_stream_key")
        .join("keymapper.tsv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 10, "oracle line {line:?} must have 10 columns");
            let mf = MediaFile {
                id: cols[0].to_string(),
                updated_at: cols[1].parse().expect("epoch seconds"),
                ..MediaFile::default()
            };
            let job = TranscodeJob {
                bit_rate: cols[2].parse().expect("bitRate"),
                sample_rate: cols[3].parse().expect("sampleRate"),
                bit_depth: cols[4].parse().expect("bitDepth"),
                channels: cols[5].parse().expect("channels"),
                format: cols[6].to_string(),
                offset: cols[7].parse().expect("offset"),
                ..TranscodeJob::default()
            };
            (mf, job, cols[8].to_string(), cols[9].to_string())
        })
        .collect()
}

#[test]
fn cache_key_and_spread_path_match_the_go_oracle() {
    let cases = key_oracle();
    assert!(cases.len() >= 5, "the oracle must cover several jobs");

    for (mf, job, expected_key, expected_hash) in &cases {
        let key = stream_cache_key(mf, job);
        assert_eq!(&key, expected_key, "streamJob.Key() for {:?}", mf.id);

        // Go's layout is `root/xx/xx/<40hex>`; the oracle gives the hash, and the
        // root here is a scratch path, so compare the derived parts.
        let path = key_mapper(Path::new("/cache/transcoding"), &key);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, expected_hash);
        assert_eq!(name.len(), KEY_HASH_HEX_LEN);
        assert_eq!(path.parent().unwrap().file_name().unwrap(), &name[2..4]);
        assert_eq!(
            path.parent()
                .unwrap()
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
            &name[0..2]
        );
    }
}

#[test]
fn key_mapper_is_idempotent_for_an_already_mapped_path() {
    // `spread_fs.go:178-181`: the haunter passes mapped paths, not keys.
    let root = scratch_dir("keymapper-idempotent");
    let mapped = key_mapper(&root, "some-key");
    assert_eq!(key_mapper(&root, &mapped.display().to_string()), mapped);
}

#[test]
fn marker_path_is_the_go_complete_suffix() {
    let path = Path::new("/cache/transcoding/23/d2/23d2");
    assert_eq!(
        marker_path(path).display().to_string(),
        "/cache/transcoding/23/d2/23d2.complete"
    );
}

#[test]
fn an_unparseable_cache_size_falls_back_to_the_go_default() {
    // `file_caches.go:269-273`: an invalid size logs and uses `DefaultCacheSize`.
    let arc = cache("size-fallback", "not-a-size", 0);
    assert!(!arc.disabled());
    assert_eq!(arc.max_size(), 100 * 1024 * 1024);
}

// ---------------------------------------------------------------------------
// Cached semantics, fan-out and failure arms (Go: file_caches_test.go)
// ---------------------------------------------------------------------------

/// A source with no limiter (caps disabled) for cache-only lanes: nothing launches,
/// so no slot is involved.
fn cache_only(arc: Arc<TranscodeCache>) -> CacheTranscodeSource {
    CacheTranscodeSource::new(arc, TranscodeLimiter::new(0, 0))
}

#[tokio::test]
async fn first_get_is_the_writer_and_reports_cached_false() {
    let arc = cache("cached-first", "10MB", 0);
    let key = "job-1";
    let source = cache_only(Arc::clone(&arc));

    let lease = source.get(key).await.expect("first get");
    // Go: `cached := w == nil` (`file_caches.go:169`) — the writer is not "cached".
    assert!(!lease.cached, "the first Get hands back the writer");
    assert!(!lease.item.seekable(), "no final size yet, so no Seeker");
    assert!(lease.item.path().exists());
    assert!(
        !marker_path(lease.item.path()).exists(),
        "a partial entry has no completion marker"
    );
}

#[tokio::test]
async fn a_second_get_mid_write_is_cached_true_but_not_seekable() {
    // Go's spec (`file_caches_test.go:286-308`): a joiner sees `Cached = true` while
    // the entry is in flight, and must NOT be handed a Seeker, because the final size
    // is unknown (`getFinalCachedSize` returns -1).
    let arc = cache("cached-midwrite", "10MB", 0);
    let key = "job-2";
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get(key).await.expect("writer");
    assert!(!writer.cached);

    let entry = arc.in_flight(key).expect("entry is in flight");
    entry.append(b"PARTIAL").await.expect("append");

    let joiner = source.get(key).await.expect("joiner");
    assert!(joiner.cached, "a joiner is not the writer ⇒ Cached = true");
    assert!(!joiner.item.seekable(), "in-flight ⇒ no Seekable arm");
    assert!(!joiner.item.complete());
    assert_eq!(
        joiner.item.final_size(),
        None,
        "no settled size to seek over"
    );
}

#[tokio::test]
async fn a_joiner_reads_every_byte_from_offset_zero() {
    // Go's `CacheReader` follows file growth: a reader that joined after the first
    // chunk still gets the whole stream from 0.
    let arc = cache("fanout", "10MB", 0);
    let key = "job-3";
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get(key).await.expect("writer");
    let entry = arc.in_flight(key).expect("entry");
    entry.append(b"chunk-one:").await.expect("first chunk");

    let joiner = source.get(key).await.expect("joiner");
    entry.append(b"chunk-two:").await.expect("second chunk");
    entry.append(b"chunk-three").await.expect("third chunk");
    arc.finalize(Arc::clone(&entry), Ok(())).await;

    let mut joiner_body = joiner.item;
    let got = drain(&mut joiner_body).await.expect("clean EOF");
    assert_eq!(
        String::from_utf8(got).expect("utf-8 fixture"),
        "chunk-one:chunk-two:chunk-three",
        "the joiner sees everything from 0, not only what came after it joined"
    );

    // The writer's own view reads the same file from 0, as Go's writer-side reader.
    let mut writer_body = writer.item;
    let wrote = drain(&mut writer_body).await.expect("writer EOF");
    assert_eq!(wrote.len(), 31);
}

#[tokio::test]
async fn a_complete_entry_hands_back_a_seekable_cached_lease() {
    let arc = cache("complete", "10MB", 0);
    let key = "job-4";
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get(key).await.expect("writer");
    let entry = arc.in_flight(key).expect("entry");
    entry.append(b"0123456789").await.expect("append");
    drop(writer);
    arc.finalize(Arc::clone(&entry), Ok(())).await;
    assert!(
        marker_path(entry.data_path()).exists(),
        "MarkComplete wrote the marker (`spread_fs.go:148-155`)"
    );

    let lease = source.get(key).await.expect("second get after completion");
    assert!(lease.cached, "steady state is Cached = true");
    assert!(
        lease.item.seekable(),
        "and the final size is known ⇒ Seeker"
    );
    assert_eq!(lease.item.final_size(), Some(10));
    assert_eq!(
        lease.item.path(),
        arc.data_path(key),
        "the handler's seekable arm re-plans this file through `direct::plan_and_open`"
    );

    let mut body = lease.item;
    let got = drain(&mut body).await.expect("clean EOF");
    assert_eq!(got, b"0123456789");
}

#[tokio::test]
async fn a_failed_entry_surfaces_the_cause_not_a_clean_eof() {
    // Go (`file_caches_test.go:277-292`): the reader drains the partial bytes and
    // then fails with "transcoder died". `CloseWithError` is what makes that happen.
    let arc = cache("failed", "10MB", 0);
    let key = "job-5";
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get(key).await.expect("writer");
    let entry = arc.in_flight(key).expect("entry");
    entry.append(b"PARTIAL").await.expect("partial bytes");
    arc.finalize(Arc::clone(&entry), Err("transcoder died".to_string()))
        .await;

    let mut body = writer.item;
    let err = drain(&mut body)
        .await
        .expect_err("the failure must reach the reader");
    assert!(
        err.contains("transcoder died"),
        "the reader must see the producer's cause, got {err:?}"
    );

    // `file_caches_test.go:309-326`: no marker for a failed write, and invalidate
    // removed the data file.
    assert!(
        !marker_path(entry.data_path()).exists(),
        "a failed write must never write a completion marker"
    );
    assert!(
        !entry.data_path().exists(),
        "invalidate removes the truncated entry (`file_caches.go:182-186`)"
    );
}

#[tokio::test]
async fn a_reader_that_joined_mid_write_gets_the_partial_then_the_cause() {
    // `file_caches_test.go:296-308` in Go: the joiner drains "PARTIAL" and then fails
    // with the same cause.
    let arc = cache("joined-fail", "10MB", 0);
    let key = "job-6";
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get(key).await.expect("writer");
    let entry = arc.in_flight(key).expect("entry");
    entry.append(b"PARTIAL").await.expect("partial");
    let joiner = source.get(key).await.expect("joiner");
    assert!(joiner.cached);

    arc.finalize(Arc::clone(&entry), Err("transcoder died".to_string()))
        .await;

    let mut body = joiner.item;
    let err = drain(&mut body)
        .await
        .expect_err("the joiner sees the failure");
    assert!(err.contains("transcoder died"), "got {err:?}");
    assert_eq!(
        body.cursor(),
        7,
        "the joiner drained the bytes written before the failure"
    );
    drop(writer);
}

#[tokio::test]
async fn an_unmarked_data_file_is_a_crash_partial_and_is_discarded() {
    // `spread_fs.go:60-64`: a file with no marker is not adopted.
    let arc = cache("crash-partial", "10MB", 0);
    let key = "orphan";
    let data = arc.data_path(key);
    std::fs::create_dir_all(data.parent().unwrap()).expect("spread dirs");
    std::fs::write(&data, b"truncated-by-crash").expect("partial file");

    let source = cache_only(Arc::clone(&arc));
    let lease = source.get(key).await.expect("re-produce");
    assert!(
        !lease.cached,
        "the caller re-produces instead of serving a partial"
    );
    assert_eq!(std::fs::read(&data).unwrap_or_default().len(), 0);
    drop(lease);
}

#[tokio::test]
async fn a_marker_without_its_data_file_is_invalidated_and_reproduced() {
    // Go's retry arm (`file_caches.go:161-168`): "Cache entry lost its data file.
    // Re-fetching", so the key does not stay broken for the process's life.
    let arc = cache("orphan-marker", "10MB", 0);
    let key = "orphan-marker";
    let data = arc.data_path(key);
    std::fs::create_dir_all(data.parent().unwrap()).expect("spread dirs");
    std::fs::write(marker_path(&data), b"").expect("marker without data");

    let source = cache_only(Arc::clone(&arc));
    let lease = source.get(key).await.expect("re-produce");
    assert!(!lease.cached);
    drop(lease);
}

#[tokio::test]
async fn zero_size_cache_is_disabled_and_open_key_refuses() {
    // `file_caches.go:275-278`: size 0 leaves the cache nil, so there is no entry to
    // open and the caller reads from its reader (`:145-152`).
    let arc = cache("size-zero", "0", 0);
    assert!(arc.disabled());
    assert!(!arc.available());
    let err = arc
        .open_key("anything")
        .await
        .expect_err("a disabled cache creates no entry");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(
        !arc.data_path("anything").parent().unwrap().exists(),
        "a disabled cache builds no spread layout (`file_caches.go:275-278`)"
    );
}

#[tokio::test]
async fn a_disabled_source_launches_uncached_rather_than_caching() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    // The observable Go behavior of a disabled cache: every Get reads straight from
    // the reader, so the seam goes to the gated launch and creates no entry.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "disabled-source", 2.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("disabled-source", "0", 0);
    let mut handler = cache_handler(Arc::clone(&arc), TranscodeLimiter::new(2, 1), true);
    let mf = media_file(&source, "ds-mf", "mp3", 2.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);

    let (tx, rx) = stays_connected();
    let observer = rx.observer();
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, observer.clone()).await;
    drop(tx);
    assert_eq!(response.status, 200);
    assert!(response.written > 0);
    assert!(
        arc.in_flight(&stream_cache_key(
            &mf,
            &transcode_job(&req, &endless_mp3_command(&bin))
        ))
        .is_none(),
        "a disabled cache stores nothing"
    );
    wait_for_poller(&observer).await;
    assert_no_poller_leak(base, "the serve left no poller task behind").await;
}

// ---------------------------------------------------------------------------
// eviction (Go: file_haunter.go)
// ---------------------------------------------------------------------------

/// Write a complete entry directly, as a previous run would have left it.
fn write_complete(arc: &TranscodeCache, key: &str, bytes: &[u8]) -> PathBuf {
    let data = arc.data_path(key);
    std::fs::create_dir_all(data.parent().unwrap()).expect("spread dirs");
    std::fs::write(&data, bytes).expect("data file");
    std::fs::write(marker_path(&data), b"").expect("marker");
    data
}

/// Bump a file's access time by reading it — the effect `file_haunter.go` sorts on.
fn touch_atime(path: &Path) {
    std::fs::read(path).expect("read bumps atime");
}

#[tokio::test]
async fn scrub_removes_the_oldest_by_access_time_until_max_items() {
    // `file_haunter.go:74-101`: sort complete files by atime ascending and remove the
    // oldest until count <= maxItems.
    let arc = cache("scrub-items", "10MB", 2);
    let oldest = write_complete(&arc, "oldest", b"aaaa");
    let middle = write_complete(&arc, "middle", b"bbbb");
    let newest = write_complete(&arc, "newest", b"cccc");
    // Give `middle` and `newest` a strictly later atime than `oldest`.
    tokio::time::sleep(Duration::from_millis(30)).await;
    touch_atime(&newest);
    touch_atime(&middle);

    let report = arc.scrub().await;
    assert_eq!(
        report.reaped,
        vec![oldest.clone()],
        "only the oldest is reaped"
    );
    assert_eq!(report.remaining_items, 2);
    assert_eq!(report.remaining_size, 8);
    assert!(!oldest.exists() && !marker_path(&oldest).exists());
    assert!(middle.exists(), "the newer entries survive");
    assert!(newest.exists());
}

#[tokio::test]
async fn scrub_removes_by_size_when_size_is_the_binding_limit() {
    // Go checks count first, then size (`file_haunter.go:95-107`); with no item
    // budget, only the size loop runs.
    let arc = cache("scrub-size", "6B", 0);
    let oldest = write_complete(&arc, "oldest", b"aaaa");
    let newest = write_complete(&arc, "newest", b"bbbb");
    tokio::time::sleep(Duration::from_millis(30)).await;
    touch_atime(&newest);

    let report = arc.scrub().await;
    assert_eq!(report.reaped, vec![oldest], "the oldest goes first");
    assert_eq!(
        report.remaining_size, 4,
        "8 bytes exceeded the 6-byte budget"
    );
    assert_eq!(report.remaining_items, 1);
    assert!(newest.exists());
}

#[tokio::test]
async fn scrub_removes_empty_files_and_orphan_markers() {
    // `file_haunter.go:53-56` removes zero-length files; `spread_fs.go:105-112`
    // removes markers whose data file is gone.
    let arc = cache("scrub-empty", "10MB", 0);
    let empty = write_complete(&arc, "empty", b"");
    let good = write_complete(&arc, "good", b"xyz");
    let orphan_marker = marker_path(&arc.data_path("gone"));
    std::fs::create_dir_all(orphan_marker.parent().unwrap()).expect("spread dirs");
    std::fs::write(&orphan_marker, b"").expect("orphan marker");

    let report = arc.scrub().await;
    assert!(report.reaped.contains(&empty), "empty file removed");
    assert!(report.reaped.contains(&orphan_marker));
    assert!(good.exists(), "a non-empty file is untouched");
    assert_eq!(report.remaining_items, 1);
}

#[tokio::test]
async fn scrub_spares_in_use_entries() {
    // `file_haunter.go:41-43`: `if e.InUse() { return true }` — never reaped.
    let arc = cache("scrub-inuse", "1B", 0);
    let source = cache_only(Arc::clone(&arc));

    // An in-flight entry with bytes already written.
    let writer = source.get("in-flight").await.expect("writer");
    let entry = arc.in_flight("in-flight").expect("entry");
    entry.append(b"in-flight-bytes").await.expect("append");

    // A complete entry whose lease is alive.
    let held = write_complete(&arc, "held", b"1234567890");
    let lease = arc
        .open_complete(held.clone(), 10)
        .await
        .expect("lease on a complete entry");

    let report = arc.scrub().await;
    assert!(
        report.reaped.is_empty(),
        "nothing in use may be reaped, got {:?}",
        report.reaped
    );
    assert!(
        entry.data_path().exists(),
        "the file being written survives"
    );
    assert!(held.exists(), "the file being read survives");
    // `file_haunter.go:41-43` returns before the count is incremented, so an entry
    // in use is not counted as a remaining item either.
    assert_eq!(report.remaining_items, 0);
    assert_eq!(report.remaining_size, 0);

    drop(lease);
    drop(writer);
}

#[tokio::test]
async fn zero_max_items_means_unlimited() {
    // `consts.DefaultTranscodingCacheMaxItems = 0` (`consts/consts.go:134`) and
    // `file_haunter.go:95`: `if maxItems > 0`.
    let arc = cache("scrub-unlimited", "10MB", 0);
    for i in 0..5 {
        write_complete(&arc, &format!("k{i}"), b"ab");
    }
    let report = arc.scrub().await;
    assert!(report.reaped.is_empty());
    assert_eq!(report.remaining_items, 5);
}

#[tokio::test]
async fn finalize_scrubs_on_entry_completion() {
    // Go's haunter runs on entry close as well as on its period; the port calls
    // `scrub` from `finalize`, so a full cache self-evicts as entries complete.
    let arc = cache("finalize-scrub", "10MB", 1);
    let old = write_complete(&arc, "old", b"aaaa");
    let source = cache_only(Arc::clone(&arc));

    let writer = source.get("fresh").await.expect("writer");
    let entry = arc.in_flight("fresh").expect("entry");
    entry.append(b"bbbb").await.expect("append");
    tokio::time::sleep(Duration::from_millis(30)).await;
    arc.finalize(Arc::clone(&entry), Ok(())).await;

    assert!(
        !old.exists(),
        "the oldest complete file was reaped on completion"
    );
    assert!(
        entry.data_path().exists(),
        "the just-completed entry survives"
    );
    drop(writer);
}

// ---------------------------------------------------------------------------
// single flight, byte counts and cancellation, through the handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_concurrent_identical_requests_launch_exactly_one_ffmpeg() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    // Long enough that both requests overlap while the entry is in flight.
    let source = match make_source(&bin, "single-flight", 300.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("single-flight", "50MB", 0);
    // Per-user cap of 1: the joiner must NOT be refused, because it acquires no slot
    // (Go: the producer acquired one, the joiner took none).
    let limiter = TranscodeLimiter::new(2, 1);

    let mf = media_file(&source, "sf-mf", "mp3", 40.0);
    let command = endless_mp3_command(&bin);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let key = stream_cache_key(&mf, &transcode_job(&req, &command));

    let baseline = ffmpeg_children();
    let (tx_a, rx_a) = stays_connected();
    let (tx_b, rx_b) = stays_connected();

    // Both serves run in this task and are awaited together: the seam's BoxFuture is
    // not `Send`, so `tokio::spawn` cannot carry them (see `tests/handler.rs`). Each
    // sink's bytes live behind an `Arc`, so they stay readable while the serve
    // futures still borrow the sinks.
    let (mut sink_a, buf_a) = CollectSink::new();
    let observer_a = rx_a.observer();
    let observer_b = rx_b.observer();
    let responses = {
        let mut handler_a = cache_handler(Arc::clone(&arc), limiter.clone(), true);
        let serve_a = handler_a.serve(&req, &mut sink_a, observer_a.clone());
        tokio::pin!(serve_a);

        // The writer must have launched before the joiner asks, so the overlap is
        // real rather than two sequential transcodes.
        drive_until_pid(&mut serve_a, &arc, &key).await;
        let pid = wait_for_pid(&arc, &key).await;
        assert!(
            pid_alive(pid),
            "the producer is running when the joiner attaches"
        );
        // The pid comes from the entry, not the response: `TranscodeLease::Cached`
        // reports no pid (`handler.rs:907`), so the entry is the shared truth.
        let entry = arc.in_flight(&key).expect("entry is in flight");
        assert_eq!(entry.pid(), Some(pid), "the entry names its producer");
        assert_eq!(
            new_children(&baseline),
            vec![pid],
            "exactly one ffmpeg beyond the baseline while the entry is in flight"
        );

        let mut handler_b = cache_handler(Arc::clone(&arc), limiter.clone(), true);
        let (mut sink_b, buf_b_inner) = CollectSink::new();
        let serve_b = handler_b.serve(&req, &mut sink_b, observer_b.clone());
        tokio::pin!(serve_b);
        let joined = tokio::join!(serve_a, serve_b);
        (joined, CollectSink::bytes(&buf_b_inner))
    };
    let (first_response, second_response) = responses.0;
    let bytes_b = responses.1;

    assert_eq!(first_response.status, 200, "the writer served the body");
    assert_eq!(
        second_response.status, 200,
        "the joiner was not refused by the per-user cap: it acquired no slot"
    );

    wait_for_entry_gone(&arc, &key).await;
    let extra = new_children(&baseline);
    assert!(
        survivors(&extra).is_empty(),
        "no ffmpeg survives the two serves: {extra:?}"
    );

    // The writer got real bytes, and the joiner got the same stream from offset 0.
    assert!(
        first_response.written > 0,
        "the writer must receive bytes, not an empty 200"
    );
    assert_eq!(
        first_response.written, second_response.written,
        "both readers received the same byte count"
    );
    let bytes_a = CollectSink::bytes(&buf_a);
    assert_eq!(bytes_a.len(), first_response.written as usize);
    assert_eq!(bytes_b.len(), second_response.written as usize);
    assert_eq!(
        bytes_b, bytes_a,
        "byte-for-byte identical streams: the joiner saw everything from offset 0"
    );
    // `TranscodeLease::Cached` reports no pid and no reap (`handler.rs:907`), so the
    // reaped fact is checked where it is owned: the entry's producer and the child set.
    assert!(
        survivors(&new_children(&baseline)).is_empty(),
        "the shared producer was reaped, so the slot is free"
    );
    assert!(
        arc.in_flight(&key).is_none(),
        "the entry completed and left the in-flight set"
    );
    assert!(marker_path(&arc.data_path(&key)).exists());
    // `D` stands for Go's request context: the request is over, so let both poller
    // tasks return before asserting none leaked.
    drop(tx_a);
    drop(tx_b);
    wait_for_poller(&observer_a).await;
    wait_for_poller(&observer_b).await;
    assert_no_poller_leak(base, "both poller tasks exited").await;
}

#[tokio::test]
async fn a_writer_lease_reports_cached_false_and_a_joiner_cached_true() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "cached-semantics-live", 30.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("cached-semantics-live", "50MB", 0);
    let source_obj = CacheTranscodeSource::new(Arc::clone(&arc), TranscodeLimiter::new(2, 2));

    let mf = media_file(&source, "cs-mf", "mp3", 30.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let job = transcode_job(&req, &endless_mp3_command(&bin));
    let key = stream_cache_key(&mf, &job);

    // Open the writer through the cache, then drive the producer through the seam so
    // the entry is live without a full handler serve.
    let opened = arc.open_key(&key).await.expect("open_key");
    let entry = match opened {
        Opened::Writer { entry } => entry,
        other => panic!("expected the writer arm, got {other:?}"),
    };
    assert!(!entry.is_complete());

    let joiner: CacheLease = source_obj.get(&key).await.expect("joiner get");
    assert!(joiner.cached, "a joiner is Cached = true");
    assert!(!joiner.item.seekable(), "still in flight ⇒ not seekable");

    // Tear the entry down without ffmpeg so the lane leaves nothing running.
    arc.finalize(Arc::clone(&entry), Err("test teardown".to_string()))
        .await;
    drop(joiner);
    assert_no_poller_leak(base, "the seam spawned no poller here").await;
}

#[tokio::test]
async fn first_requester_disconnect_kills_the_group_and_leaves_no_survivor() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "disconnect-writer", 300.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("disconnect-writer", "50MB", 0);
    let limiter = TranscodeLimiter::new(1, 1);

    let mf = media_file(&source, "dc-mf", "mp3", 60.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let key = stream_cache_key(&mf, &transcode_job(&req, &endless_mp3_command(&bin)));

    let baseline = ffmpeg_children();
    let (tx, rx) = stays_connected();
    let mut handler = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let mut sink = Vec::new();
    let observer = rx.observer();
    let serve = handler.serve(&req, &mut sink, observer.clone());
    tokio::pin!(serve);

    let mut pid = None;
    for _ in 0..600 {
        if let Some(entry) = arc.in_flight(&key) {
            if let Some(found) = entry.pid() {
                pid = Some(found);
                break;
            }
        }
        tokio::select! {
            biased;
            _ = &mut serve => break,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
    let pid = pid.expect("the first request launched a producer");
    assert!(pid_alive(pid));
    assert_eq!(
        new_children(&baseline),
        vec![pid],
        "the first request launched exactly one process"
    );

    // The first requester hangs up: the group must die and nothing may survive.
    drop(tx);
    let response = serve.await;
    assert!(
        response.disconnected,
        "the handler reports the client went away"
    );
    assert!(!pid_alive(pid), "the process group is killed");
    let extra = new_children(&baseline);
    assert!(
        survivors(&extra).is_empty(),
        "zero surviving children after the disconnect: {extra:?}"
    );

    // A cancelled producer is an incomplete entry: no marker and no adopted partial.
    // Go: the request context dies, `ffCmd.wait` errors the pipe (`ffmpeg.go:384-393`),
    // `copyAndClose` returns the error and `Get` invalidates (`file_caches.go:181-186`).
    // The producer runs in the background (Go's `go fc.copyAndClose`), so its
    // teardown is awaited rather than assumed settled when `serve` returns.
    wait_for_entry_gone(&arc, &key).await;
    let data = arc.data_path(&key);
    assert!(!marker_path(&data).exists(), "no completion marker");
    assert!(!data.exists(), "the truncated entry is invalidated");
    assert!(
        arc.in_flight(&key).is_none(),
        "the entry left the in-flight set"
    );

    // The slot is free again: a *different* key is served, not refused.
    let other_mf = media_file(&source, "dc-other", "mp3", 60.0);
    let other = transcode_request(&source, other_mf, "mp3", 128);
    let (tx2, rx2) = stays_connected();
    let (again, _sink2) =
        serve_one(Arc::clone(&arc), limiter.clone(), &other, rx2, tx2, false).await;
    assert_eq!(
        again.status, 200,
        "the freed slot served the next request (dedupe off, so this is a fresh \
         transcode needing a fresh slot)"
    );
    let pid2 = again
        .pid
        .expect("the second request launched its own process");
    assert_ne!(pid, pid2, "a different key is a different producer");
    assert!(!pid_alive(pid2), "and it was reaped too");
    wait_for_poller(&observer).await;
    assert_no_poller_leak(base, "the disconnect poller task exited").await;
}

#[tokio::test]
async fn a_late_reader_hanging_up_does_not_kill_the_shared_producer() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "reader-disconnect", 300.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("reader-disconnect", "50MB", 0);
    let limiter = TranscodeLimiter::new(2, 2);

    let mf = media_file(&source, "rd-mf", "mp3", 60.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let key = stream_cache_key(&mf, &transcode_job(&req, &endless_mp3_command(&bin)));

    // Writer: stays connected for the whole encode.
    let (tx_writer, rx_writer) = stays_connected();
    let mut handler_w = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let (mut sink_w, buf_w) = CollectSink::new();
    let observer_w = rx_writer.observer();
    let serve_w = handler_w.serve(&req, &mut sink_w, observer_w.clone());
    tokio::pin!(serve_w);
    drive_until_pid(&mut serve_w, &arc, &key).await;
    let pid = wait_for_pid(&arc, &key).await;
    let entry = arc.in_flight(&key).expect("entry is in flight");

    // Reader: hangs up before the producer finishes.
    let (tx_reader, rx_reader) = stays_connected();
    drop(tx_reader);
    let mut handler_r = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let mut sink_r = Vec::new();
    let observer_r = rx_reader.observer();
    let serve_r = handler_r.serve(&req, &mut sink_r, observer_r.clone());
    let (reader_status, reader_disconnected) = {
        let response = serve_r.await;
        (response.status, response.disconnected)
    };
    assert_eq!(reader_status, 200);
    assert!(
        reader_disconnected,
        "the reader's own read stopped when it hung up"
    );
    assert!(
        pid_alive(pid),
        "a reader hanging up must not kill the shared producer"
    );
    assert_eq!(
        entry.pid(),
        Some(pid),
        "the entry still names the same producer"
    );

    let response = serve_w.await;
    assert_eq!(response.status, 200);
    assert!(
        response.written > 0,
        "the producer kept serving the writer after the reader left"
    );
    assert_eq!(response.written as usize, CollectSink::bytes(&buf_w).len());
    assert!(
        !pid_alive(pid),
        "the producer ended when the encode reached EOF"
    );
    drop(tx_writer);
    wait_for_poller(&observer_w).await;
    wait_for_poller(&observer_r).await;
    assert_no_poller_leak(base, "both poller tasks exited").await;
}

#[tokio::test]
async fn a_rejection_is_429_semantics_not_a_cache_error() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "rejection", 3.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("rejection", "10MB", 0);
    let limiter = TranscodeLimiter::new(1, 1);

    // Occupy the only slot, then ask the cache for a transcode: the refusal must be
    // the limiter's, untouched (`media_streamer.go:234-241`).
    let permit = limiter.acquire("alice").expect("first slot");

    let mf = media_file(&source, "rej-mf", "mp3", 3.0);
    let req = transcode_request(&source, mf, "mp3", 128);
    let (tx, rx) = stays_connected();
    let observer = rx.observer();
    let mut handler = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let mut sink = Vec::new();
    let response = handler.serve(&req, &mut sink, observer.clone()).await;
    drop(tx);
    wait_for_poller(&observer).await;

    assert_eq!(response.status, 429, "error was {:?}", response.error);
    assert_eq!(response.written, 0);
    assert!(sink.is_empty());
    assert_eq!(
        response.headers.get("Retry-After"),
        Some(RETRY_AFTER_SECONDS.to_string().as_str()),
        "Go's two handlers set Retry-After on the 429"
    );
    let message = response
        .error
        .as_ref()
        .expect("the rejection is the error")
        .to_string();
    assert!(
        message.contains("too many"),
        "the message is the limiter's, got {message:?}"
    );
    assert!(
        !message.contains("transcoding cache"),
        "a rejection must NOT be wrapped as a cache failure (`media_streamer.go:114-117`)"
    );
    // The entry the rejection opened is gone, so a retry can re-produce it.
    let key = stream_cache_key(
        &req.media_file,
        &transcode_job(&req, &endless_mp3_command(&bin)),
    );
    assert!(
        arc.in_flight(&key).is_none(),
        "the refused entry was dropped"
    );
    assert!(
        !arc.data_path(&key).exists(),
        "the refused entry's file is removed, so a retry re-produces it"
    );

    drop(permit);
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}

#[tokio::test]
async fn head_on_a_cached_transcode_reports_zero_bytes_written() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    // `media_streamer.go:174-177` drains with `io.Discard` and returns 0, which is
    // what makes the share handler answer 500 on `n == 0`
    // (`server/public/handle_streams.go:87-89`). Reproduced, not "fixed".
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "head-cache", 20.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("head-cache", "50MB", 0);
    let mf = media_file(&source, "head-mf", "mp3", 20.0);
    let mut req = transcode_request(&source, mf, "mp3", 128);
    req.cond.method = "HEAD".to_string();

    let (tx, rx) = stays_connected();
    let (response, sink) = serve_one(
        Arc::clone(&arc),
        TranscodeLimiter::new(2, 1),
        &req,
        rx,
        tx,
        true,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.written, 0, "HEAD reports 0 bytes, as Go does");
    assert!(sink.is_empty(), "no body for HEAD");
    assert_eq!(
        response.share_handler_status(),
        500,
        "Go's share handler overwrites the status when n == 0"
    );
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}

#[tokio::test]
async fn a_completed_entry_is_served_seekable_with_range_semantics() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "seekable-replay", 20.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("seekable-replay", "50MB", 0);
    let mf = media_file(&source, "sr-mf", "mp3", 20.0);
    let req = transcode_request(&source, mf, "mp3", 128);

    // First request produces the entry.
    let (tx, rx) = stays_connected();
    let (produced, sink) = serve_one(
        Arc::clone(&arc),
        TranscodeLimiter::new(2, 1),
        &req,
        rx,
        tx,
        true,
    )
    .await;
    assert_eq!(produced.status, 200);
    assert!(produced.written > 0);
    assert!(!produced.seekable, "a fresh entry is served as a pipe");

    // Second request replays it: the handler takes the seekable arm, so a Range
    // request answers 206 with the exact window.
    let mut ranged = req.clone();
    ranged.cond.range = Some("bytes=0-99".to_string());
    let (tx2, rx2) = stays_connected();
    let (replay, sink2) = serve_one(
        Arc::clone(&arc),
        TranscodeLimiter::new(2, 1),
        &ranged,
        rx2,
        tx2,
        true,
    )
    .await;

    assert_eq!(
        replay.status, 206,
        "a complete cached entry is Range-capable"
    );
    assert!(
        replay.seekable,
        "Go's `s.Seekable()` arm (`media_streamer.go:160`)"
    );
    assert_eq!(replay.written, 100);
    assert_eq!(
        sink2.as_slice(),
        &sink[..100],
        "the window is the same bytes"
    );
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}

#[tokio::test]
async fn a_joiner_takes_no_slot_while_an_unrelated_key_is_refused() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    // Proves the slot is neither double-acquired for the shared entry nor released
    // early: with the cap at 1, the identical request is served and a different key
    // is refused.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "cap-shared", 300.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("cap-shared", "50MB", 0);
    let limiter = TranscodeLimiter::new(1, 1);

    let mf = media_file(&source, "cap-mf", "mp3", 30.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let key = stream_cache_key(&mf, &transcode_job(&req, &endless_mp3_command(&bin)));

    let (tx, rx) = stays_connected();
    let mut handler_w = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let (mut sink_w, buf_w) = CollectSink::new();
    let serve_w = handler_w.serve(&req, &mut sink_w, rx.observer());
    tokio::pin!(serve_w);
    drive_until_pid(&mut serve_w, &arc, &key).await;
    assert!(
        pid_alive(wait_for_pid(&arc, &key).await),
        "the producer is still running, so its slot is still held"
    );

    // Different key, same user: refused **first**, because the refusal is returned at
    // `open` while the producer still holds the only slot. A 429 here proves the slot
    // was still held at that instant.
    let other_mf = media_file(&source, "cap-other", "mp3", 300.0);
    let other = transcode_request(&source, other_mf, "mp3", 128);
    let (tx3, rx3) = stays_connected();
    let observer_c = rx3.observer();
    let mut handler_c = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let mut sink_c = Vec::new();
    let refused = handler_c
        .serve(&other, &mut sink_c, observer_c.clone())
        .await;
    assert_eq!(refused.status, 429, "an unrelated key still needs a slot");
    assert_eq!(refused.written, 0);
    drop(tx3);
    wait_for_poller(&observer_c).await;

    // Same key: served by the shared producer, so it acquired no slot — a second
    // acquire would have been refused exactly like the request above.
    let (tx2, rx2) = stays_connected();
    let observer_b = rx2.observer();
    let mut handler_b = cache_handler(Arc::clone(&arc), limiter.clone(), true);
    let mut sink_b = Vec::new();
    let joined = handler_b.serve(&req, &mut sink_b, observer_b.clone()).await;
    assert_eq!(joined.status, 200, "a joiner takes no slot");
    assert!(joined.written > 0, "the joiner received the shared stream");
    drop(tx2);
    wait_for_poller(&observer_b).await;

    let writer = serve_w.await;
    wait_for_entry_gone(&arc, &key).await;
    assert_eq!(writer.status, 200);
    assert_eq!(
        writer.written as usize,
        CollectSink::bytes(&buf_w).len(),
        "the writer received the whole body"
    );
    drop(tx);
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}

// ---------------------------------------------------------------------------
// non-vacuity of the single-flight assertion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_flight_is_not_vacuous_when_dedupe_is_disabled() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    // The same two-request shape as
    // `two_concurrent_identical_requests_launch_exactly_one_ffmpeg`, with the dedupe
    // path switched off: the assertion `first.pid == second.pid` must FAIL here. This
    // is the mirror of `transcode_body::GatedTranscode::release_slot_early`.
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "dedupe-off", 20.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("dedupe-off", "50MB", 0);
    let mf = media_file(&source, "dv-mf", "mp3", 20.0);
    let req = transcode_request(&source, mf.clone(), "mp3", 128);
    let key = stream_cache_key(&mf, &transcode_job(&req, &endless_mp3_command(&bin)));

    let ffmpeg_baseline = ffmpeg_children();
    let (tx1, rx1) = stays_connected();
    let (tx2, rx2) = stays_connected();

    // Both requests run concurrently in this task (`tokio::spawn` cannot carry a
    // serve future over the seam — the BoxFuture is not `Send`).
    let mut handler_a = cache_handler(Arc::clone(&arc), TranscodeLimiter::new(4, 4), false);
    let mut sink_a = Vec::new();
    let observer_a = rx1.observer();
    let serve_a = handler_a.serve(&req, &mut sink_a, observer_a.clone());
    let mut handler_b = cache_handler(Arc::clone(&arc), TranscodeLimiter::new(4, 4), false);
    let mut sink_b = Vec::new();
    let observer_b = rx2.observer();
    let serve_b = handler_b.serve(&req, &mut sink_b, observer_b.clone());
    let (first, second) = tokio::join!(serve_a, serve_b);

    let first_pid = first.pid.expect("pid for the first request");
    let second_pid = second.pid.expect("pid for the second request");
    assert_ne!(
        first_pid, second_pid,
        "with dedupe off the two identical requests launch DIFFERENT processes; if \
         this ever reports equal pids the dedupe switch is not wired and the \
         single-flight test above is vacuous"
    );
    assert!(
        arc.in_flight(&key).is_none(),
        "the disabled path creates no cache entry"
    );
    assert!(!arc.data_path(&key).exists());
    // `D` stands for Go's request context: resolve it and the poller task returns.
    drop(tx1);
    drop(tx2);
    wait_for_poller(&observer_a).await;
    wait_for_poller(&observer_b).await;
    let extra = new_children(&ffmpeg_baseline);
    assert!(
        survivors(&extra).is_empty(),
        "both uncached processes were reaped: {extra:?}"
    );
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}

#[tokio::test]
async fn the_poller_exits_on_teardown_even_when_the_client_never_hangs_up() {
    let _guard = PROCESS_LOCK.lock().await;
    // Requirement 5: the poller task must not outlive the serve. Go's producer ends
    // when the stream reaches EOF, so a client that keeps the socket open cannot pin
    // a task. The exit here comes from the drain task's announcement
    // (`SharedDisconnect::finish`), not from `D`, so the sender is deliberately never
    // dropped.
    let base = poller_baseline();
    let bin = match ffmpeg_bin() {
        Some(bin) => bin,
        None => return,
    };
    let source = match make_source(&bin, "poller-teardown", 20.0) {
        Some(path) => path,
        None => return,
    };
    let arc = cache("poller-teardown", "50MB", 0);

    let (_tx, rx) = stays_connected();
    let observer = rx.observer();
    let mut handler = cache_handler(Arc::clone(&arc), TranscodeLimiter::new(2, 1), true);
    let mut sink = Vec::new();
    let mf = media_file(&source, "pt-mf", "mp3", 20.0);
    let req = transcode_request(&source, mf, "mp3", 128);
    let response = handler.serve(&req, &mut sink, observer.clone()).await;
    assert_eq!(response.status, 200);
    assert!(response.written > 0);

    // `_tx` is still alive, so the only thing that can release the task is the
    // producer's teardown announcement. Whether that announcement has already landed
    // when `serve` returns depends on the drain task's scheduling (Go runs
    // `copyAndClose` in its own goroutine), so only the exit itself is asserted —
    // and the mutation check proves a poller that ignores the announcement hangs.
    wait_for_poller(&observer).await;
    assert_no_poller_leak(base, "the poller exited on the teardown announcement").await;
}

#[tokio::test]
async fn exactly_one_task_polls_a_caller_disconnect_however_many_copies_are_made() {
    let _guard = PROCESS_LOCK.lock().await;
    // Requirement 2's premise: one owner polls `D`. `SharedDisconnect::spawn` is the
    // only constructor that starts a task, and cloning hands out receivers that cannot
    // poll the caller's future — so an observer and its clones account for exactly one
    // poller, and a mid-write reader's clone adds none.
    let base = poller_baseline();
    let (_tx, rx) = stays_connected();
    let observer = rx.observer();
    assert_eq!(
        live_pollers(),
        base + 1,
        "the spawn started exactly one task"
    );
    assert!(
        !observer.poller_exited(),
        "the poller is live while `D` is pending"
    );

    let pollers = base + 1;
    for _ in 0..5 {
        let clone = observer.clone();
        assert_eq!(
            live_pollers(),
            pollers,
            "cloning an observer starts no task, so nothing polls `D` twice"
        );
        // A clone reports the same cancellation state and cannot finish the poller.
        assert!(!clone.is_cancelled());
        clone.finish();
    }
    // `never()` spawns nothing: Go's uncancelled legacy read.
    let uncancelled = SharedDisconnect::never();
    assert!(!uncancelled.is_cancelled());
    assert_eq!(live_pollers(), pollers, "`never` starts no task");

    drop(_tx);
    wait_for_poller(&observer).await;
    assert_no_poller_leak(base, "the one poller exited once `D` resolved").await;
}

#[tokio::test]
async fn the_lane_leaves_no_poller_task_behind() {
    let _guard = PROCESS_LOCK.lock().await;
    let base = poller_baseline();
    // `SharedDisconnect::spawn` is the only task that polls a caller's future, and
    // the drain task announces teardown. The count must be 0 once nothing is being
    // served — checked here and at the end of every lifecycle test above.
    let arc = cache("pollers", "10MB", 0);
    assert_no_poller_leak(base, "no poller task outlived the serve").await;

    // A cache entry with no producer spawns nothing.
    let source = cache_only(Arc::clone(&arc));
    let lease = source.get("no-producer").await.expect("writer");
    let entry = arc.in_flight("no-producer").expect("entry");
    assert_no_poller_leak(base, "a plain cache entry spawns no task").await;
    arc.finalize(Arc::clone(&entry), Ok(())).await;
    drop(lease);
    assert_no_poller_leak(base, "no poller task outlived the serve").await;
}
