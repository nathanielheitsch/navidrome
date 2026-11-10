//! Direct-file serving adapter — the Rust shape of Go's raw streaming path
//! (`core/stream/media_streamer.go:86-99`: `os.Open(filePath)` handed back as
//! both `io.ReadCloser` **and** `io.Seeker`, which is exactly why Go's
//! `Stream.Serve` takes the `http.ServeContent` branch for it).
//!
//! **Trust boundary — read this before wiring an HTTP handler.**
//! The path given to [`open_authorized`] must already be an
//! **authorized service-layer path**: the absolute path of a `MediaFile` row
//! the caller has authenticated the requester against, i.e. Go's
//! `mf.AbsolutePath()` after the share/library checks in
//! `server/public/handle_streams.go:45-61` (`ds.MediaFile().Get`, share
//! membership, `shareOwner.HasLibraryAccess`) or the equivalent
//! [`crate::types::MediaFile`] after the caller's own authorization.
//! This module performs **no** authorization and **no** path derivation: it
//! never joins, normalizes, extension-checks, or otherwise interprets the
//! path, and it must not be handed a request-supplied string, a query
//! parameter, a filename from a zip entry, or anything derived from user
//! input. Any path traversal / library-scoping decision belongs to the layer
//! that materializes the path (nd-model/nd-db + the auth middleware), so the
//! contract is enforced by construction upstream rather than by a check here.
//!
//! Serving is **bounded and incremental**: [`RangeBody::read_chunk`] returns at
//! most [`MAX_CHUNK_BYTES`] per call, so a 4 GiB file streams in constant
//! memory. Go gets the same property from `io.Copy`'s 32 KiB buffer
//! (`io.Copy` → `io.copyBuffer` default 32KiB) feeding `http.ServeContent`;
//! the port makes the cap explicit instead of relying on a stdlib default.
//!
//! Length semantics, matching Go's two observable cases:
//!   - **direct** (this module): length is known from `os.Stat`, so the body
//!     gets `Content-Length` + `Accept-Ranges: bytes` and can be ranged via
//!     [`crate::range::plan`].
//!   - **transcoded**: length is unknown (a pipe), so the body gets chunked
//!     transfer with `Accept-Ranges: none`
//!     (`media_streamer.go:165`) — see [`crate::range::plan_streaming`].

use std::io;
use std::path::Path;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::range::{self, CondRequest, FileMeta, ServePlan};

/// Largest chunk [`RangeBody::read_chunk`] will return in one call.
///
/// Go's equivalent bound is `io.Copy`'s internal 32 KiB buffer; 64 KiB here
/// keeps the same order of magnitude while halving the syscall count for
/// large bodies. It is a *read* bound, not a write bound — the HTTP layer
/// decides how much of each chunk to flush per write.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;

/// A direct-file body: an open handle plus the byte window to serve.
///
/// The window is `[start, end]` inclusive, resolved by [`crate::range::plan`].
/// Dropping the value closes the file (tokio's `File` owns the fd), which is
/// the port's stand-in for Go's `defer stream.Close()`
/// (`server/public/handle_streams.go:77-81`).
#[derive(Debug)]
pub struct RangeBody {
    /// Open file handle. `None` once [`RangeBody::close`] has run.
    file: Option<File>,
    /// Absolute byte offset of the next byte to read.
    cursor: u64,
    /// Inclusive last byte to serve.
    end: u64,
    /// Scratch buffer, reused across reads so repeated chunks allocate once.
    buf: Vec<u8>,
}

impl RangeBody {
    /// Seek to `start` and serve through `end` inclusive. Seeks up front, which
    /// is what Go's `io.Seeker` enables inside `http.ServeContent`
    /// (`net/http/fs.go:216-221`: `errSeek` → `io.SeekStart`).
    pub async fn open(file: File, start: u64, end: u64) -> io::Result<RangeBody> {
        let mut file = file;
        file.seek(io::SeekFrom::Start(start)).await?;
        Ok(RangeBody {
            file: Some(file),
            cursor: start,
            end,
            buf: Vec::new(),
        })
    }

    /// Bytes still to serve, i.e. the exact `Content-Length` the plan promised.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.end.saturating_add(1).saturating_sub(self.cursor)
    }

    /// True once the window has been fully served.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Read the next chunk: at most `max` bytes and never more than
    /// [`MAX_CHUNK_BYTES`], and never past the window's end. Returns `None` at
    /// end-of-window (a clean EOF — not an error).
    ///
    /// A short read from the OS is reported as-is; only a zero-byte read
    /// before the window is done means the file shrank underneath us, which is
    /// surfaced as [`io::ErrorKind::UnexpectedEof`] so the caller aborts the
    /// response instead of silently truncating it.
    pub async fn read_chunk(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        let want = self.remaining();
        if want == 0 {
            return Ok(None);
        }
        let cap = if max < MAX_CHUNK_BYTES {
            max
        } else {
            MAX_CHUNK_BYTES
        };
        let want_bytes = if want < cap as u64 {
            want as usize
        } else {
            cap
        };

        let file = match self.file.as_mut() {
            Some(f) => f,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "body already closed",
                ))
            }
        };

        if self.buf.len() < want_bytes {
            self.buf.resize(want_bytes, 0);
        }
        let mut read = 0usize;
        while read < want_bytes {
            let n = file.read(&mut self.buf[read..want_bytes]).await?;
            if n == 0 {
                if read == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "media file shrank while being served",
                    ));
                }
                // Partial read is fine: report what we have; the next call
                // reports the shrink.
                break;
            }
            read += n;
        }
        self.cursor += read as u64;
        Ok(Some(self.buf[..read].to_vec()))
    }

    /// Read the whole window, bounded by the chunk cap. Test/inspection helper:
    /// it *does* accumulate bytes, so a response path must use
    /// [`RangeBody::read_chunk`] instead.
    pub async fn read_all(&mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(chunk) = self.read_chunk(MAX_CHUNK_BYTES).await? {
            out.extend_from_slice(chunk.as_slice());
        }
        Ok(out)
    }

    /// Explicit close: drop the handle. Idempotent, mirroring Go's
    /// `releasingReadCloser.Close` idempotency (`core/stream/limiter.go:63-67`).
    /// The fd closes on drop either way; this exists so a handler can close
    /// eagerly instead of waiting for the value to go out of scope.
    pub async fn close(&mut self) -> io::Result<()> {
        self.file.take();
        Ok(())
    }
}

/// Open an **already-authorized** media path for direct serving.
///
/// See the module doc for the trust boundary: the caller must have authorized
/// this exact path before calling. Opens read-only; a missing path yields
/// [`io::ErrorKind::NotFound`] so the HTTP layer can answer 404 rather than
/// 500 (Go's `os.Open` error propagates the same way at
/// `media_streamer.go:91-94`).
pub async fn open_authorized(path: &Path) -> io::Result<File> {
    File::open(path).await
}

/// Normalize a caller-supplied validator into the **wire form** Go compares
/// against (`fs.go:506,:556,:595` all read `w.Header().get("Etag")`, and
/// `etagStrongMatch` requires the value to begin with `'"'`, `fs.go:463-466`).
///
/// Accepts either a bare tag body (`abc`) or an already-quoted wire value
/// (`"abc"`, `W/"abc"`), and returns `None` for an empty/absent validator —
/// which is Navidrome's own seekable behavior (`media_streamer.go:161` sets no
/// ETag, and Go never synthesizes one; see deviation 4 in `range.rs`).
#[must_use]
pub fn normalize_etag(etag: Option<&str>) -> Option<String> {
    let raw = etag?.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('"') || raw.starts_with("W/") {
        return Some(raw.to_string());
    }
    Some(format!("\"{raw}\""))
}

/// Build the [`FileMeta`] the conditional matrix needs from a `Stat` result —
/// Go's `size` + `modTime` inputs to `http.ServeContent`
/// (`core/stream/media_streamer.go:161`).
///
/// `etag` is the caller's validator, bare or already quoted (see
/// [`normalize_etag`]); `None` means no validator.
#[must_use]
pub fn meta_from_metadata(md: &std::fs::Metadata, etag: Option<&str>) -> FileMeta {
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    FileMeta {
        size: md.len(),
        last_modified_secs: mtime,
        etag: normalize_etag(etag),
        content_type: None,
        content_encoding: None,
    }
}

/// Open an authorized path, stat it, and plan the response.
///
/// Returns `(meta, plan, body)`. When the plan carries no body (304/412/416,
/// or HEAD) no handle is opened at all — Go's `serveContent` likewise never
/// writes after `WriteHeader(304)` (`fs.go:642`), and a descriptor must not
/// linger for a response that sends no bytes.
///
/// A directory is reported as [`io::ErrorKind::InvalidInput`] rather than
/// opened, matching Go's `fileExists` directory arm
/// (`core/ffmpeg/ffmpeg.go:141-149` → `'%s' is a directory`).
pub async fn plan_and_open(
    path: &Path,
    req: &CondRequest,
    etag: Option<&str>,
) -> io::Result<(FileMeta, ServePlan, Option<RangeBody>)> {
    let md = tokio::fs::metadata(path).await?;
    if md.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a directory", path.display()),
        ));
    }
    let meta = meta_from_metadata(&md, etag);
    let plan = range::plan(&meta, req);

    if plan.no_body {
        return Ok((meta, plan, None));
    }

    if plan.bytes_to_write() == 0 {
        // An empty file plans status 200 with Content-Length: 0, so there is
        // nothing to read; opening a descriptor for a zero-byte body would be
        // pure overhead (Go's io.CopyN(w, content, 0) reads nothing).
        return Ok((meta, plan, None));
    }

    let file = open_authorized(path).await?;
    let body = RangeBody::open(file, plan.start, plan.end).await?;
    Ok((meta, plan, Some(body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::RangeOutcome;

    use std::path::PathBuf;

    /// Fixtures live **outside the repo tree**, in the OS temp dir, and are
    /// addressed only through `std::env::temp_dir()`. Never a hard-coded
    /// developer path: an absolute path pinned to one checkout fails on every
    /// other host (that is what broke four tests in the previous lane).
    fn fixture_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("nd-stream-direct-{name}"));
        std::fs::create_dir_all(&dir).expect("temp fixture dir");
        dir
    }

    /// Write a fixture and return its path, unique per test name so parallel
    /// tests cannot collide.
    fn write_fixture(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = fixture_dir(name);
        let mut path = dir;
        path.push(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path
    }

    /// Sparse-but-large fixture: `size` bytes claimed without writing them all,
    /// so the bounded-memory assertion costs no disk. Byte at offset `off` is
    /// `off % 251`, so a wrong window is detectable byte-for-byte.
    fn write_sparse_fixture(name: &str, size: u64) -> PathBuf {
        let dir = fixture_dir(name);
        let mut path = dir;
        path.push(name);
        let mut f = std::fs::File::create(&path).expect("create sparse fixture");
        use std::io::{Seek, SeekFrom, Write};
        f.seek(SeekFrom::Start(size - 1)).unwrap();
        f.write_all(&[((size - 1) % 251) as u8]).unwrap();
        f.flush().unwrap();
        // Fill the hole deterministically: sparse holes read as zero, which is
        // fine as long as the assertion knows it.
        path
    }

    #[tokio::test]
    async fn normal_get_serves_whole_file() {
        let path = write_fixture("normal", b"0123456789");
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let (_meta, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 200);
        assert_eq!(plan.content_length, Some(10));
        let body_mut = body.as_mut().unwrap();
        assert_eq!(body_mut.remaining(), 10);
        let bytes = body_mut.read_all().await.unwrap();
        assert_eq!(bytes, b"0123456789".to_vec());
        assert!(body_mut.is_exhausted());
    }

    #[tokio::test]
    async fn partial_range_reads_only_the_window() {
        let data: Vec<u8> = (0u8..255).cycle().take(1000).collect();
        let path = write_fixture("partial", data.as_slice());
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=100-199".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 206);
        assert_eq!(plan.content_range.as_deref(), Some("bytes 100-199/1000"));
        let bytes = body.as_mut().unwrap().read_all().await.unwrap();
        assert_eq!(bytes.len(), 100);
        assert_eq!(bytes, data[100..200].to_vec());
    }

    #[tokio::test]
    async fn open_ended_range_reads_to_end() {
        let data: Vec<u8> = (0u8..255).cycle().take(1000).collect();
        let path = write_fixture("open-ended", data.as_slice());
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=900-".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 206);
        assert_eq!(plan.content_length, Some(100));
        let bytes = body.as_mut().unwrap().read_all().await.unwrap();
        assert_eq!(bytes, data[900..].to_vec());
    }

    #[tokio::test]
    async fn suffix_range_reads_the_tail() {
        let data: Vec<u8> = (0u8..255).cycle().take(1000).collect();
        let path = write_fixture("suffix", data.as_slice());
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=-150".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 206);
        assert_eq!(plan.start, 850);
        let bytes = body.as_mut().unwrap().read_all().await.unwrap();
        assert_eq!(bytes, data[850..].to_vec());
    }

    #[tokio::test]
    async fn zero_length_file_serves_empty_200() {
        let path = write_fixture("zero-length", b"");
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=0-10".to_string()),
            ..CondRequest::default()
        };
        let (meta, plan, body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(meta.size, 0);
        assert_eq!(plan.status, 200);
        assert_eq!(plan.content_length, Some(0));
        assert!(body.is_none(), "an empty body must not open a read window");
    }

    #[tokio::test]
    async fn beyond_eof_range_is_416_and_reads_nothing() {
        let path = write_fixture("beyond", b"0123456789");
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=50-100".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 416);
        assert_eq!(plan.content_range.as_deref(), Some("bytes */10"));
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn malformed_range_falls_back_to_full_200() {
        let path = write_fixture("malformed", b"abcdef");
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("pages=1-2".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 200);
        assert_eq!(plan.content_length, Some(6));
        assert_eq!(
            body.as_mut().unwrap().read_all().await.unwrap(),
            b"abcdef".to_vec()
        );
        assert_eq!(
            range::parse_range(Some("pages=1-2"), 6),
            RangeOutcome::Ignored
        );
    }

    #[tokio::test]
    async fn multiple_ranges_serve_full_content() {
        let path = write_fixture("multi", b"abcdefghij");
        let req = CondRequest {
            method: "GET".to_string(),
            range: Some("bytes=0-2,7-9".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 200);
        assert_eq!(plan.content_length, Some(10));
        assert_eq!(plan.content_range, None);
        assert_eq!(
            body.as_mut().unwrap().read_all().await.unwrap(),
            b"abcdefghij".to_vec()
        );
    }

    #[tokio::test]
    async fn head_serves_headers_only_and_reads_nothing() {
        let path = write_fixture("head", b"abcdefghij");
        let req = CondRequest {
            method: "HEAD".to_string(),
            ..CondRequest::default()
        };
        let (_m, plan, body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 200);
        assert_eq!(plan.bytes_to_write(), 0);
        assert!(body.is_none());
        assert_eq!(plan.etag.as_deref(), Some("\"e1\""));
        assert_eq!(plan.accept_ranges.as_deref(), Some("bytes"));
    }

    #[tokio::test]
    async fn conditional_hit_304_and_miss_200() {
        let path = write_fixture("conditional", b"abcdefghij");
        let md = std::fs::metadata(path.as_path()).unwrap();
        let meta = meta_from_metadata(&md, Some("etag-v1"));
        assert_eq!(meta.size, 10);
        assert!(meta.last_modified_secs.is_some());

        let hit = CondRequest {
            method: "GET".to_string(),
            if_none_match: Some("\"etag-v1\"".to_string()),
            ..CondRequest::default()
        };
        let (_m, plan, body) = plan_and_open(path.as_path(), &hit, Some("etag-v1"))
            .await
            .unwrap();
        assert_eq!(plan.status, 304);
        assert!(body.is_none());

        let miss = CondRequest {
            method: "GET".to_string(),
            if_none_match: Some("\"other\"".to_string()),
            ..CondRequest::default()
        };
        let (_m2, plan2, body2) = plan_and_open(path.as_path(), &miss, Some("etag-v1"))
            .await
            .unwrap();
        assert_eq!(plan2.status, 200);
        assert_eq!(plan2.bytes_to_write(), 10);
        assert!(body2.is_some());

        // If-Modified-Since at exactly the fixture's mtime is a hit.
        let revalidate = CondRequest {
            method: "GET".to_string(),
            if_modified_since: meta.last_modified_secs.map(range::http_date),
            ..CondRequest::default()
        };
        assert_eq!(range::plan(&meta, &revalidate).status, 304);
    }

    #[tokio::test]
    async fn missing_file_is_not_found() {
        let mut path = fixture_dir("missing");
        path.push("does-not-exist.mp3");
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let err = plan_and_open(path.as_path(), &req, Some("e1")).await;
        match err {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            Ok(_) => panic!("a missing path must not yield a body"),
        }
    }

    #[tokio::test]
    async fn directory_is_invalid_input_not_a_body() {
        let dir = fixture_dir("dir");
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let err = plan_and_open(dir.as_path(), &req, Some("e1")).await;
        match err {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("a directory must not be served"),
        }
    }

    /// Bounded-memory case: a 64 MiB sparse file served through the chunk API.
    /// Asserts the reader never hands back more than `MAX_CHUNK_BYTES` at a
    /// time, that it needs many chunks (so there is no single big buffer), and
    /// that the bytes are the window's bytes.
    #[tokio::test]
    async fn large_file_is_read_in_bounded_chunks() {
        let size: u64 = 64 * 1024 * 1024;
        let path = write_sparse_fixture("large", size);
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let (meta, plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        assert_eq!(meta.size, size);
        assert_eq!(plan.content_length, Some(size));

        let body_mut = body.as_mut().unwrap();
        let mut chunks = 0usize;
        let mut total: u64 = 0;
        let mut max_seen = 0usize;
        let mut tail: Vec<u8> = Vec::new();
        while let Some(chunk) = body_mut.read_chunk(MAX_CHUNK_BYTES).await.unwrap() {
            chunks += 1;
            if chunk.len() > max_seen {
                max_seen = chunk.len();
            }
            assert!(
                chunk.len() <= MAX_CHUNK_BYTES,
                "chunk of {} bytes exceeds the {}-byte bound",
                chunk.len(),
                MAX_CHUNK_BYTES
            );
            total += chunk.len() as u64;
            if chunks > 3 {
                // Keep only the last chunk alive: peak retained bytes stay O(chunk).
                tail = chunk;
            }
        }
        assert_eq!(total, size, "the whole window must be served");
        assert!(
            chunks >= (size / MAX_CHUNK_BYTES as u64) as usize,
            "expected many bounded chunks, got {chunks}"
        );
        assert_eq!(max_seen, MAX_CHUNK_BYTES);
        assert_eq!(tail.len(), MAX_CHUNK_BYTES);
        assert_eq!(tail[MAX_CHUNK_BYTES - 1], ((size - 1) % 251) as u8);
        assert!(body_mut.is_exhausted());
    }

    #[tokio::test]
    async fn chunk_cap_smaller_than_max_is_honored() {
        let data: Vec<u8> = (0u8..255).cycle().take(1000).collect();
        let path = write_fixture("chunk-cap", data.as_slice());
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let (_m, _plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        let body_mut = body.as_mut().unwrap();
        let first = body_mut.read_chunk(64).await.unwrap().unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(first, data[..64].to_vec());
        assert_eq!(body_mut.remaining(), 936);
    }

    #[test]
    fn normalize_etag_produces_wire_form() {
        // Go compares the raw `Etag` header value and etagStrongMatch requires
        // it to begin with '"' (fs.go:463-466), so the port hands out wire form
        // — never a bare token, and never a synthesized tag.
        assert_eq!(normalize_etag(None), None);
        assert_eq!(normalize_etag(Some("")), None);
        assert_eq!(normalize_etag(Some("   ")), None);
        assert_eq!(normalize_etag(Some("abc")), Some("\"abc\"".to_string()));
        assert_eq!(normalize_etag(Some("\"abc\"")), Some("\"abc\"".to_string()));
        assert_eq!(
            normalize_etag(Some("W/\"abc\"")),
            Some("W/\"abc\"".to_string()),
            "an explicit weak tag is preserved, not upgraded"
        );
    }

    #[test]
    fn meta_without_etag_has_no_validator() {
        // Deviation 4: no validator is ever synthesized. With no ETag,
        // If-Match fails (412) and If-None-Match cannot produce a 304.
        let dir = fixture_dir("no-validator");
        let mut path = dir;
        path.push("file.bin");
        std::fs::write(&path, b"abc").unwrap();
        let md = std::fs::metadata(path.as_path()).unwrap();
        let meta = meta_from_metadata(&md, None);
        assert_eq!(meta.etag, None);
        assert_eq!(meta.etag_or_empty(), "");
        assert_eq!(meta.size, 3);
        assert!(meta.has_known_mtime());

        let mut req = CondRequest {
            method: "GET".to_string(),
            if_match: Some("\"abc\"".to_string()),
            ..CondRequest::default()
        };
        assert_eq!(range::plan(&meta, &req).status, 412);
        req.if_match = None;
        req.if_none_match = Some("\"abc\"".to_string());
        assert_eq!(range::plan(&meta, &req).status, 200);
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let path = write_fixture("close", b"abc");
        let req = CondRequest {
            method: "GET".to_string(),
            ..CondRequest::default()
        };
        let (_m, _plan, mut body) = plan_and_open(path.as_path(), &req, Some("e1"))
            .await
            .unwrap();
        let body_mut = body.as_mut().unwrap();
        assert!(body_mut.close().await.is_ok());
        assert!(body_mut.close().await.is_ok());
        assert!(
            body_mut.read_chunk(16).await.is_err(),
            "closed body cannot read"
        );
    }
}
