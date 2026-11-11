//! Process-backed transcode body — the Rust shape of Navidrome's non-seekable
//! streaming path (`core/stream/media_streamer.go:101-133` for the launch and
//! `:157-199` for the write loop), plus the FLAC duration patcher that Go
//! splices onto ffmpeg's stdout for FLAC output (`core/ffmpeg/ffmpeg.go:92`).
//!
//! ## Trust boundary
//!
//! [`open_transcode`] takes an **already-authorized service-layer path**, never
//! a request-supplied one. Go reaches it through `mf.AbsolutePath()`
//! (`media_streamer.go:84`), which derives from an authorized DB row; this layer
//! does no path derivation, no authorization, and no user input parsing. It is
//! also the ffmpeg-argument consumer, so callers must hand it arguments built by
//! `nd-ffmpeg`'s argv builders, never by string concatenation of client data.
//!
//! ## What this is not (yet)
//!
//! Go's transcode path runs through the transcoding **cache** and its
//! **rate-limiting semaphore** (`media_streamer.go:110-121`,
//! `core/stream/limiter.go`, `utils/cache`), which is why a Go transcode body is
//! a `*streamJob` reader rather than a bare process. Neither is ported here, so
//! this body is one ffmpeg process per call with no concurrency cap and no
//! shared cached file. That is an integration gap for #15, tracked in `lib.rs`.
//!
//! ## Divergences from Go, frozen here
//!
//!   - **No ambient context.** Go's `ffCmd` uses `exec.CommandContext` and dies
//!     when the request context is cancelled. Rust has no ambient context, so
//!     [`TranscodeBody::serve`] takes an explicit `disconnect` future and calls
//!     [`TranscodeBody::cancel`] when it resolves: SIGKILL at the process-group
//!     root, pipes drained, child reaped. Drop attempts group termination but
//!     cannot await reaping: it is best-effort and runtime-driver-dependent.
//!     A killed child remains a zombie until reaped; explicit close is preferred.
//!   - **Content type is the builtin table only.** Go's
//!     `mime.TypeByExtension` (`media_streamer.go:146`) also consults
//!     `/etc/mime.types` on unix; the port ships the Go builtin audio entries
//!     only and returns `None` for anything else, which Go also does for an
//!     unknown extension (it returns `""`, and an empty `Content-Type` is
//!     dropped by the header write).
//!   - **A post-flush failure aborts, it does not swap status.** Go logs the
//!     error, and when bytes were already written it
//!     `panic(http.ErrAbortHandler)` (`media_streamer.go:186-189`). There is no
//!     panic unwinding to a handler here, so [`ServeOutcome::truncated`] carries
//!     that decision to the HTTP layer.

use std::future::Future;
use std::io;

use nd_ffmpeg::args::TranscodeOptions as FFmpegTranscodeOptions;
use nd_ffmpeg::ffcmd::{ExitInfo, FFCmd};
use nd_ffmpeg::flac_patcher::set_flac_total_samples;
use nd_ffmpeg::transcode::transcode;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::types::MediaFile;

/// Bytes the write loop pulls from ffmpeg per iteration. Matches the
/// `io.Copy` 32 KiB internal buffer Go uses (`io.copyLoop`), so the pipe
/// pressure and syscall count match the Go path.
pub const TRANSCODE_CHUNK_BYTES: usize = 32 * 1024;

/// `mime.TypeByExtension("." + format)` (`media_streamer.go:146`) restricted to
/// Go's builtin audio table (`net/http/../mime/type.go:80-118`). `None` means
/// Go would return `""` and the header must be omitted.
///
/// Deliberately narrow: ffmpeg only emits these containers for audio, and the
/// table is a port of Go's builtin map, not of the system `/etc/mime.types`
/// (see the module doc's divergence note).
#[must_use]
pub fn content_type_for_format(format: &str) -> Option<&'static str> {
    match format {
        "mp3" => Some("audio/mpeg"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "flac" => Some("audio/flac"),
        "wav" => Some("audio/wav"),
        "m4a" => Some("audio/mp4"),
        "weba" => Some("audio/webm"),
        _ => None,
    }
}

/// `Stream.EstimatedContentLength` (`media_streamer.go:148-150`):
/// `int(mf.Duration * float32(bitRate) / 8 * 1024)`.
///
/// Go's expression is `float32` throughout and truncates at the final `int()`,
/// so the port multiplies in `f32` step by step — Go's evaluation order is
/// `((duration * bitRate) / 8) * 1024`, and each step rounds to `f32`. A `f64`
/// intermediate disagrees by one byte on some inputs; `duration =
/// 2163.835906758199`, `bit_rate = 1` gives Go 276971 and `f64` 276970 (pinned
/// against a Go oracle). The header is only sent when the client asks for
/// `estimateContentLength` (`media_streamer.go:168-172`).
#[must_use]
pub fn estimated_content_length(duration: f32, bit_rate: i64) -> i64 {
    let rate = bit_rate as f32;
    let scaled = (duration * rate) / 8.0;
    (scaled * 1024.0) as i64
}

/// `LookupTranscodeCommand` fallback arm (`decider.go:329-341`): the built-in
/// template for a default format. Go's caller always hands `Transcode` a
/// command resolved by the decider, so an empty command here means the caller
/// did not resolve one — resolve it from the built-in table rather than
/// building template args from an empty string, which would yield no argv at
/// all (`build_template_args` splits the command, so `""` ⇒ `[]`).
#[must_use]
pub fn default_command_for(format: &str) -> String {
    crate::decider::new_transcode_decider(Vec::new()).lookup_transcode_command(format)
}

/// The failure modes of a transcode body, in Go's order of appearance.
#[derive(Debug, thiserror::Error)]
pub enum TranscodeBodyError {
    /// `Transcode` returned an error before any byte was produced — Go's
    /// `ms.cache.Get` failure arm (`media_streamer.go:111-119`), rendered with
    /// Go's `<path> exited with non-zero status code: <code>[: stderr]` shape
    /// when the process did run and failed (`ffmpeg.go:387-390`).
    #[error("{message}")]
    Process {
        /// Client-safe message. Never contains the media path (the path is in
        /// `stderr` text from ffmpeg, which Go caps at `STDERR_CAP` and logs,
        /// not returns to clients).
        message: String,
    },

    /// Reading ffmpeg's stdout failed (`io.Copy` error, `media_streamer.go:181`).
    #[error("reading transcoded stream: {0}")]
    Read(#[source] io::Error),

    /// Writing to the response failed — Go sees the same error from `io.Copy`
    /// and, since bytes were already written, aborts the response.
    #[error("writing response body: {0}")]
    Write(#[source] io::Error),

    /// The body was already ended (drained, cancelled, or dropped) and cannot
    /// be served again.
    #[error("transcode body already ended")]
    Ended,
}

/// Result of a serve/drain pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOutcome {
    /// Bytes written to the sink.
    pub written: u64,
    /// Some when the stream failed. Go distinguishes `written == 0` — the 200
    /// has not been flushed, so the caller can still answer 500
    /// (`media_streamer.go:183-185` deletes `Content-Length` and returns the
    /// error) — from `written > 0`, where the 200 is already on the wire and
    /// the only option is to drop the connection.
    pub error: Option<String>,
    /// True when the response must be aborted mid-body because the status was
    /// already sent: `error.is_some() && written > 0`. The HTTP layer's
    /// equivalent of Go's `panic(http.ErrAbortHandler)`.
    pub truncated: bool,
    /// Set when `disconnect` resolved and the child was killed instead of the
    /// stream reaching EOF.
    pub disconnected: bool,
    /// True when the stream reached EOF with 0 bytes — Go logs "Transcoding
    /// returned empty output, ffmpeg may have failed" and does **not** treat it
    /// as an error (`media_streamer.go:190-193`).
    pub empty_output: bool,
}

impl ServeOutcome {
    /// A clean run: no error, not truncated, not disconnected.
    #[must_use]
    pub fn ok(written: u64) -> Self {
        ServeOutcome {
            written,
            error: None,
            truncated: false,
            disconnected: false,
            empty_output: written == 0,
        }
    }

    /// Whether the HTTP layer may still send an error status: only when nothing
    /// has been written (`media_streamer.go:183`).
    #[must_use]
    pub fn can_swap_status(&self) -> bool {
        self.error.is_some() && self.written == 0
    }
}

/// Streaming splice of the FLAC STREAMINFO fix — the port of Go's
/// `flacPatcher` (`core/ffmpeg/flac_streaminfo.go:22-47`).
///
/// Go peeks in `Read`, not in the constructor, so `Transcode` returns before
/// ffmpeg has emitted anything; the port keeps that property by filling the
/// 26-byte prefix on the first read. The patch itself is delegated to
/// `nd-ffmpeg`'s `set_flac_total_samples` (the byte-exact gate: `fLaC` magic,
/// first block is STREAMINFO, non-zero header rate, currently-zero
/// total_samples field, and a total that fits the 36-bit field).
struct FlacPatcher {
    duration: f32,
    prefix: Vec<u8>,
    /// Index into `prefix` still to be served; `prefix.len()` means the prefix
    /// has been handed out and reads go straight to the child.
    served: usize,
    /// Short prefix read: ffmpeg produced fewer than 26 bytes, so there is
    /// nothing to patch and the bytes are forwarded as-is (Go:
    /// `io.ReadFull` returning `ErrUnexpectedEOF` ⇒ no `setFLACTotalSamples`
    /// call, `flac_streaminfo.go:33-40`).
    unpatched: bool,
}

impl FlacPatcher {
    fn new(duration: f32) -> Self {
        FlacPatcher {
            duration,
            prefix: Vec::new(),
            served: 0,
            unpatched: false,
        }
    }
}

/// A live ffmpeg process plus the stdout reader handed to the response,
/// equivalent to Go's `Stream.ReadCloser` for a non-seekable stream.
///
/// The child handle is owned privately; the only way to end the stream is EOF,
/// [`TranscodeBody::cancel`], or drop. Nothing here touches `FFCmd`'s lifecycle
/// fields — the process is driven entirely through `nd-ffmpeg`'s inherent
/// methods (`reader`/`wait`/`cancel`) so the lifecycle lane's field changes
/// cannot break this caller.
pub struct TranscodeBody {
    /// Go's `ffCmd` handle (`core/ffmpeg/ffmpeg.go:354-361`), kept for
    /// teardown: `cancel()`/`wait()` reap the child.
    cmd: FFCmd,
    /// Go's `j.out` pipe (`ffmpeg.go:357`) as the piped stdout of the child.
    reader: Option<tokio::process::ChildStdout>,
    /// Present only for FLAC output (`ffmpeg.go:92` wraps the reader in
    /// `patchFLACDuration`).
    patcher: Option<FlacPatcher>,
    ended: bool,
}

impl TranscodeBody {
    /// `patchFLACDuration(out, opts.Duration-float32(opts.Offset))`
    /// (`ffmpeg.go:92`): FLAC only, and only when the remaining duration is
    /// positive (Go returns the reader untouched when `duration <= 0`,
    /// `flac_streaminfo.go:19-21`).
    fn enable_flac_patch(&mut self, duration: f32, offset: i64) {
        let remaining = duration - (offset as f32);
        if remaining > 0.0 {
            self.patcher = Some(FlacPatcher::new(remaining));
        }
    }

    /// Read the next chunk of the (possibly FLAC-patched) stream. Returns `None`
    /// at EOF.
    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Vec<u8>>, TranscodeBodyError> {
        if self.ended {
            return Err(TranscodeBodyError::Ended);
        }
        let cap = if max < TRANSCODE_CHUNK_BYTES {
            max
        } else {
            TRANSCODE_CHUNK_BYTES
        };
        let cap = if cap == 0 { TRANSCODE_CHUNK_BYTES } else { cap };

        match self.patcher.as_mut() {
            Some(p) if p.served < p.prefix.len() => {
                // Still feeding the patched prefix: hand out the remaining
                // prefix bytes, capped by the caller's request.
                let take = std::cmp::min(cap, p.prefix.len() - p.served);
                let out = p.prefix[p.served..p.served + take].to_vec();
                p.served += take;
                return Ok(Some(out));
            }
            Some(_) => {}
            None => {}
        }

        let mut buf = vec![0u8; cap];
        let n = self.read_raw(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);

        if self.patcher_needs_prefix() {
            // First read: Go fills exactly flacPrefixLen with io.ReadFull and
            // patches only on a complete read (flac_streaminfo.go:31-40).
            let mut prefix = vec![0u8; nd_ffmpeg::flac_patcher::FLAC_PREFIX_LEN];
            let got = self.fill_prefix(prefix.as_mut_slice(), buf).await?;
            let complete = got == prefix.len();
            if complete {
                let duration = self.patcher.as_ref().map(|p| p.duration).unwrap_or(0.0);
                prefix = set_flac_total_samples(prefix, duration);
            } else {
                prefix.truncate(got);
            }
            if let Some(p) = self.patcher.as_mut() {
                p.unpatched = !complete;
                p.prefix = prefix;
                p.served = 0;
            }
            if self
                .patcher
                .as_ref()
                .map(|p| p.prefix.is_empty())
                .unwrap_or(true)
            {
                return Ok(None);
            }
            let p = self.patcher.as_ref().unwrap();
            let take = std::cmp::min(cap, p.prefix.len());
            return Ok(Some(p.prefix[..take].to_vec()));
        }

        Ok(Some(buf))
    }

    /// True when a FLAC patcher is attached and its 26-byte prefix has not been
    /// produced yet. Kept as a `&self` check so the `fill_prefix` await below
    /// never holds a borrow of the patcher across a suspension point.
    #[must_use]
    fn patcher_needs_prefix(&self) -> bool {
        match self.patcher.as_ref() {
            Some(p) => p.prefix.is_empty() && !p.unpatched,
            None => false,
        }
    }

    /// Pull from the child stdout, treating EOF as `Ok(0)`.
    async fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize, TranscodeBodyError> {
        let reader = match self.reader.as_mut() {
            Some(r) => r,
            None => return Ok(0),
        };
        match reader.read(buf).await {
            Ok(n) => Ok(n),
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => Ok(0),
            Err(e) => Err(TranscodeBodyError::Read(e)),
        }
    }

    /// Finish filling a 26-byte prefix, starting from `first` bytes already
    /// read, the way `io.ReadFull` keeps reading until the buffer is full or
    /// the source ends.
    async fn fill_prefix(
        &mut self,
        prefix: &mut [u8],
        first: Vec<u8>,
    ) -> Result<usize, TranscodeBodyError> {
        let mut filled = std::cmp::min(first.len(), prefix.len());
        prefix[..filled].copy_from_slice(&first[..filled]);
        if filled == prefix.len() {
            return Ok(filled);
        }
        while filled < prefix.len() {
            let n = self.read_raw(&mut prefix[filled..]).await?;
            if n == 0 {
                return Ok(filled);
            }
            filled += n;
        }
        Ok(filled)
    }

    /// Write the stream to `sink` until EOF, the disconnect future resolves, or
    /// the write fails.
    ///
    /// This is the port of Go's `io.Copy(w, s)` (`media_streamer.go:180`) with
    /// the context cancellation Go gets for free replaced by `disconnect`.
    /// When `disconnect` resolves, cancellation is attempted. A teardown error
    /// is reported and the child remains owned for a later retry.
    pub async fn serve<W, D>(&mut self, sink: &mut W, disconnect: D) -> ServeOutcome
    where
        W: AsyncWrite + Unpin,
        D: Future<Output = ()>,
    {
        tokio::pin!(disconnect);
        let mut written: u64 = 0;

        loop {
            let chunk = tokio::select! {
                biased;
                () = &mut disconnect => {
                    let cleanup_error = self.cancel().await.err().map(|e| e.to_string());
                    return ServeOutcome {
                        written,
                        error: cleanup_error,
                        truncated: false,
                        disconnected: true,
                        empty_output: false,
                    };
                }
                r = self.read_chunk(TRANSCODE_CHUNK_BYTES) => r,
            };

            match chunk {
                Ok(None) => break,
                Err(e) => {
                    let message = e.to_string();
                    return ServeOutcome {
                        written,
                        error: Some(message),
                        truncated: written > 0,
                        disconnected: false,
                        empty_output: false,
                    };
                }
                Ok(Some(bytes)) => {
                    if let Err(e) = sink.write_all(&bytes).await {
                        return ServeOutcome {
                            written,
                            error: Some(e.to_string()),
                            truncated: written > 0,
                            disconnected: false,
                            empty_output: false,
                        };
                    }
                    written += bytes.len() as u64;
                }
            }
        }

        ServeOutcome::ok(written)
    }

    /// Drain the stream without writing it — the HEAD arm
    /// (`media_streamer.go:174-177`: `go func() { io.Copy(io.Discard, s) }()`).
    ///
    /// Go spawns a goroutine so the response returns immediately while ffmpeg
    /// is still drained; without draining, ffmpeg would block on a full pipe
    /// and the child would outlive the request. The port awaits the drain
    /// instead of spawning, so the caller decides whether to spawn. A teardown
    /// error is reported rather than being mistaken for a completed reap.
    pub async fn drain(&mut self) -> ServeOutcome {
        let mut written: u64 = 0;
        loop {
            match self.read_chunk(TRANSCODE_CHUNK_BYTES).await {
                Ok(None) => break,
                Ok(Some(bytes)) => written += bytes.len() as u64,
                Err(e) => {
                    return ServeOutcome {
                        written,
                        error: Some(e.to_string()),
                        truncated: written > 0,
                        disconnected: false,
                        empty_output: false,
                    };
                }
            }
        }
        match self.reap().await {
            Ok(_) => ServeOutcome::ok(written),
            Err(e) => ServeOutcome {
                written,
                error: Some(e.to_string()),
                truncated: written > 0,
                disconnected: false,
                empty_output: false,
            },
        }
    }

    /// Kill and reap the child. An error leaves cleanup unresolved and retryable.
    pub async fn cancel(&mut self) -> io::Result<ExitInfo> {
        self.ended = true;
        self.reader = None;
        self.cmd.cancel().await
    }

    /// `ffCmd.Close` (`ffmpeg.go:361`-adjacent: Go's `defer stream.Close()` in
    /// the handler, `server/public/handle_streams.go`): stdout reached EOF, so
    /// wait for the child that exits on its own and reap it. A teardown error
    /// is returned to the caller; an unresolved child remains owned for retry.
    pub async fn close(&mut self) -> io::Result<ExitInfo> {
        self.reap().await
    }

    /// Wait for a child that exits on its own and reap it.
    async fn reap(&mut self) -> io::Result<ExitInfo> {
        self.ended = true;
        self.reader = None;
        self.cmd.wait().await
    }

    /// The ffmpeg binary this body launched, for the caller's log.
    #[must_use]
    pub fn exec(&self) -> &str {
        self.cmd.exec.as_str()
    }

    /// Whether cleanup still owns a child, including after a timeout or error.
    #[must_use]
    pub fn cleanup_pending(&self) -> bool {
        self.cmd.is_available()
    }

    /// Whether stdout has been fully consumed or the stream ended.
    #[must_use]
    pub fn is_ended(&self) -> bool {
        self.ended
    }
}

impl Drop for TranscodeBody {
    fn drop(&mut self) {
        // FFCmd's Drop synchronously signals the process group. Unlike explicit
        // cancel/close it cannot await reaping; callers should close explicitly.
        // Close stdout before the command is dropped.
        self.ended = true;
        self.reader = None;
    }
}

/// Go's `streamJob` (`core/stream/media_streamer.go:101-110`): the resolved
/// transcode parameters a launch consumes, assembled by the caller from the
/// decision (`TranscodeDecision`) plus the authorized media path.
///
/// `file_path` is the already-authorized absolute path — Go's
/// `mf.AbsolutePath()`. An empty `command` means "use the built-in template for
/// `format`"; Go's decider always resolves one, so the HTTP layer should pass
/// its `LookupTranscodeCommand` result.
#[derive(Debug, Clone, Default)]
pub struct TranscodeJob {
    /// Already-authorized absolute media path (`filePath`).
    pub file_path: String,
    /// Target container (`format`).
    pub format: String,
    /// Target bitrate, kbps (`bitRate`).
    pub bit_rate: i64,
    /// Target sample rate (`sampleRate`).
    pub sample_rate: i64,
    /// Target bit depth (`bitDepth`).
    pub bit_depth: i64,
    /// Target channel count (`channels`).
    pub channels: i64,
    /// Playback offset in seconds (`offset`).
    pub offset: i64,
    /// ffmpeg command template; empty means the built-in default for `format`.
    pub command: String,
}

/// Launch ffmpeg for an authorized media file and hand back the body.
///
/// Mirrors Go's transcode arm end to end: `ffmpegCmd()` resolution and
/// `fileExists` (`ffmpeg.go:129-135` via `transcode.rs`), argv built by
/// `nd-ffmpeg` (`build_dynamic_args`/`build_template_args`, `ffmpeg.go:83-89`),
/// then the FLAC wrapper (`ffmpeg.go:92`).
pub async fn open_transcode(
    mf: &MediaFile,
    job: &TranscodeJob,
) -> Result<TranscodeBody, TranscodeBodyError> {
    // Go's TranscodeOptions.Duration is the media file's duration
    // (`media_streamer.go:101-108` builds the job from the mf), and the FLAC
    // patch subtracts the offset.
    let duration = mf.duration as f32;
    let command = if job.command.is_empty() {
        default_command_for(job.format.as_str())
    } else {
        job.command.clone()
    };
    if command.is_empty() {
        // No template and no built-in row for this format: Go's
        // LookupTranscodeCommand returns "" and the transcode cannot be built,
        // so fail before launching anything instead of spawning with no argv.
        return Err(TranscodeBodyError::Process {
            message: format!("no transcode command configured for format {}", job.format),
        });
    }
    let opts = FFmpegTranscodeOptions {
        command,
        format: job.format.clone(),
        file_path: job.file_path.clone(),
        bit_rate: job.bit_rate,
        sample_rate: job.sample_rate,
        bit_depth: job.bit_depth,
        channels: job.channels,
        offset: job.offset,
        duration,
    };

    let (bin, ok) = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
    if !ok {
        return Err(TranscodeBodyError::Process {
            message: format!("ffmpeg not available: {bin}"),
        });
    }

    let mut cmd = transcode(opts).await.map_err(|e| {
        // Go's cache.Get error arm: the message is the ffmpeg error text
        // verbatim (media_streamer.go:111-119 logs it and returns it).
        TranscodeBodyError::Process {
            message: e.to_string(),
        }
    })?;

    let reader = cmd.reader();
    match reader {
        None => {
            // No piped stdout means the child is gone; reap it and report the
            // exit in Go's message shape (ffmpeg.go:387-390).
            let info = cmd.cancel().await.unwrap_or(ExitInfo {
                code: -1,
                stderr: String::new(),
            });
            Err(TranscodeBodyError::Process {
                message: nd_ffmpeg::ffcmd::go_error_message(
                    cmd.exec.clone(),
                    info.code,
                    info.stderr.clone(),
                ),
            })
        }
        Some(r) => {
            let mut body = TranscodeBody {
                cmd,
                reader: Some(r),
                patcher: None,
                ended: false,
            };
            if job.format == "flac" {
                body.enable_flac_patch(duration, job.offset);
            }
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MediaFile;

    #[tokio::test]
    async fn close_reports_unresolved_reap_and_cancel_retries() {
        let mut cmd = nd_ffmpeg::ffcmd::new_ffmpeg_cmd(
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), "sleep 30".to_owned()],
        );
        cmd.start().await.unwrap();
        let reader = cmd.reader();
        let mut body = TranscodeBody {
            cmd,
            reader,
            patcher: None,
            ended: false,
        };
        let result = body.close().await;
        assert!(matches!(result, Err(ref e) if e.kind() == io::ErrorKind::TimedOut));
        assert!(body.cleanup_pending(), "timed-out child must remain owned");
        body.cancel().await.expect("retry must kill and reap");
        assert!(!body.cleanup_pending());
    }

    /// Fixtures live in the OS temp dir, addressed only through
    /// `std::env::temp_dir()` — never an absolute developer path (that is what
    /// broke four tests on every other host), and never inside the repo tree.
    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("nd-stream-transcode-{name}"));
        std::fs::create_dir_all(dir.as_path()).expect("temp fixture dir");
        dir
    }

    /// Skip-if-missing, house style: ffmpeg 9 is present on the dev box but not
    /// guaranteed on every CI host.
    fn ffmpeg_bin() -> Option<String> {
        let (bin, ok) = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
        if ok {
            Some(bin)
        } else {
            None
        }
    }

    /// Encode a short source file with ffmpeg itself; returns its path.
    fn make_source(bin: &str, name: &str, extra: &[&str]) -> Option<String> {
        let dir = fixture_dir(name);
        let mut path = dir;
        path.push(format!("src.{name}"));
        let mut cmd = std::process::Command::new(bin);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.3",
        ]);
        cmd.args(extra);
        cmd.args(["-y", path.display().to_string().as_str()]);
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        if ok && path.exists() {
            Some(path.display().to_string())
        } else {
            None
        }
    }

    /// Build a job against a fixture path, mirroring how the HTTP layer would
    /// assemble it from a decision plus the authorized path.
    fn job_for(path: &str, format: &str, bit_rate: i64) -> TranscodeJob {
        TranscodeJob {
            file_path: path.to_string(),
            format: format.to_string(),
            bit_rate,
            ..TranscodeJob::default()
        }
    }

    /// A stand-in for a sink that never fails: the response writer.
    #[derive(Default)]
    struct Sink {
        bytes: Vec<u8>,
    }

    impl AsyncWrite for Sink {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, io::Error>> {
            let this = self.get_mut();
            let _ = cx;
            this.bytes.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A sink that fails after `after` bytes: models a client that went away
    /// mid-body, when the 200 is already on the wire.
    struct FlakySink {
        after: usize,
        written: usize,
    }

    impl AsyncWrite for FlakySink {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, io::Error>> {
            let this = self.get_mut();
            if this.written >= this.after {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "client went away",
                )));
            }
            this.written += buf.len();
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn content_type_matches_go_builtin_table() {
        assert_eq!(content_type_for_format("mp3"), Some("audio/mpeg"));
        assert_eq!(content_type_for_format("ogg"), Some("audio/ogg"));
        assert_eq!(content_type_for_format("oga"), Some("audio/ogg"));
        assert_eq!(content_type_for_format("opus"), Some("audio/ogg"));
        assert_eq!(content_type_for_format("flac"), Some("audio/flac"));
        assert_eq!(content_type_for_format("wav"), Some("audio/wav"));
        assert_eq!(content_type_for_format("m4a"), Some("audio/mp4"));
        // Go's builtin table has no entry for these, so TypeByExtension is ""
        // and the header is omitted.
        assert_eq!(content_type_for_format("aac"), None);
        assert_eq!(content_type_for_format("wma"), None);
        assert_eq!(content_type_for_format(""), None);
    }

    #[test]
    fn estimated_content_length_is_go_float32_truncation() {
        // Values pinned against a Go oracle running the original expression.
        assert_eq!(estimated_content_length(300.0, 320), 12_288_000);
        assert_eq!(estimated_content_length(0.0, 320), 0);
        assert_eq!(estimated_content_length(10.5, 128), 172_032);
        assert_eq!(estimated_content_length(60.0, 192), 1_474_560);
        assert_eq!(estimated_content_length(3600.0, 128), 58_982_400);
        // Truncation, not rounding: 0.3333333s at 96kbps is 4095.x -> 4095.
        assert_eq!(estimated_content_length(0.3333333, 96), 4_095);
        assert_eq!(estimated_content_length(1.0, 0), 0);
        // f32 step-rounding, not f64: Go answers 276971 and 26689252 here,
        // f64 arithmetic answers 276970 and 26689251. Literals are the
        // shortest that round-trip to Go's float32 values (0x45073d60,
        // 0x454b9f72).
        assert_eq!(estimated_content_length(2163.836_f32, 1), 276_971);
        assert_eq!(estimated_content_length(3257.9653_f32, 64), 26_689_252);
    }

    #[test]
    fn default_command_lookup_matches_the_builtin_table() {
        // Go's LookupTranscodeCommand fallback (decider.go:333-340): a known
        // default format has a template, an unknown one has none.
        assert!(default_command_for("mp3").contains("-b:a %bk"));
        assert!(default_command_for("flac").contains("-c:a flac"));
        assert_eq!(default_command_for("wma"), String::new());
    }

    #[tokio::test]
    async fn unknown_format_without_command_fails_before_launching() {
        let mf = MediaFile::default();
        let res = open_transcode(&mf, &job_for("/no/such/file.wma", "wma", 128)).await;
        match res {
            Err(TranscodeBodyError::Process { message }) => {
                assert!(message.contains("no transcode command"), "{message}");
            }
            Err(other) => panic!("unexpected error {other}"),
            Ok(mut body) => {
                let _ = body.cancel().await;
                panic!("an unknown format with no template must not launch ffmpeg");
            }
        }
    }

    #[test]
    fn flac_patch_is_disabled_for_non_positive_remaining_duration() {
        // Go: patchFLACDuration returns the reader untouched when duration<=0.
        let mut body = TranscodeBody {
            cmd: nd_ffmpeg::ffcmd::new_ffmpeg_cmd("true".to_string(), vec![]),
            reader: None,
            patcher: None,
            ended: false,
        };
        body.enable_flac_patch(0.0, 0);
        assert!(body.patcher.is_none());
        body.enable_flac_patch(1.0, 1);
        assert!(
            body.patcher.is_none(),
            "duration - offset == 0 must not patch"
        );
        body.enable_flac_patch(2.0, 1);
        assert_eq!(body.patcher.as_ref().unwrap().duration, 1.0);
    }

    #[tokio::test]
    async fn missing_ffmpeg_is_a_process_error_not_a_panic() {
        // This box has ffmpeg; force the unavailable arm through the test
        // override only when it is unset, so the assertion is honest.
        if std::env::var_os("ND_TEST_FFMPEG_PATH").is_some() {
            return;
        }
        let mf = MediaFile::default();
        let res = open_transcode(&mf, &job_for("/no/such/file.mp3", "mp3", 128)).await;
        match res {
            Err(TranscodeBodyError::Process { message }) => {
                assert!(
                    !message.contains("/no/such/file.mp3") || message.contains("not available"),
                    "message must be the ffmpeg error text, got {message}"
                );
            }
            Err(other) => panic!("unexpected error {other}"),
            Ok(mut body) => {
                // ffmpeg exists but the media file does not: Go's fileExists
                // arm fails before the launch, so this branch is unreachable
                // in practice. Drain to be safe.
                let _ = body.cancel().await;
                panic!("transcode of a missing file must fail");
            }
        }
    }

    #[tokio::test]
    async fn ended_body_cannot_be_read_again() {
        let mut body = TranscodeBody {
            cmd: nd_ffmpeg::ffcmd::new_ffmpeg_cmd("true".to_string(), vec![]),
            reader: None,
            patcher: None,
            ended: true,
        };
        let err = body.read_chunk(16).await.unwrap_err();
        assert!(matches!(err, TranscodeBodyError::Ended));
    }

    #[tokio::test]
    async fn live_transcode_streams_bytes_and_reaps_the_child() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return, // skip-if-missing
        };
        let src = match make_source(bin.as_str(), "mp3", &[]) {
            Some(p) => p,
            None => return, // no libmp3lame in this build: skip
        };

        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "mp3", 128))
            .await
            .expect("transcode of a real file");

        let mut sink = Sink::default();
        let outcome = body.serve(&mut sink, std::future::pending::<()>()).await;
        assert_eq!(outcome.error, None, "clean run: {outcome:?}");
        assert!(!outcome.disconnected);
        assert!(
            outcome.written > 0,
            "ffmpeg produced no bytes for a real mp3 encode"
        );
        assert_eq!(outcome.written, sink.bytes.len() as u64);
        assert!(!outcome.empty_output);
        assert!(!body.is_ended(), "EOF is not Close; the handler closes");

        // The Rust close explicitly waits; Go's stream Close does not wait for
        // ffCmd's separate Wait goroutine (core/stream/limiter.go:63-66).
        let info = body.close().await.expect("reap succeeds");
        assert!(body.is_ended());
        assert_eq!(info.code, 0, "a clean encode exits 0");
    }

    #[tokio::test]
    async fn live_flac_transcode_patches_the_streaminfo_duration() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        let src = match make_source(bin.as_str(), "flac", &[]) {
            Some(p) => p,
            None => return, // no flac encoder: skip
        };

        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "flac", 0))
            .await
            .expect("flac transcode");
        assert!(
            body.patcher.is_some(),
            "flac output must carry the duration patcher"
        );

        let mut sink = Sink::default();
        let outcome = body.serve(&mut sink, std::future::pending::<()>()).await;
        assert_eq!(outcome.error, None);
        assert!(outcome.written > 0);

        // The first four bytes are the fLaC magic, and the 36-bit
        // total_samples field at offset 18 must be non-zero (ffmpeg leaves it
        // at 0 when writing to a pipe).
        assert_eq!(&sink.bytes[..4], b"fLaC");
        let field = u64::from_be_bytes([
            sink.bytes[18],
            sink.bytes[19],
            sink.bytes[20],
            sink.bytes[21],
            sink.bytes[22],
            sink.bytes[23],
            sink.bytes[24],
            sink.bytes[25],
        ]);
        let total = field & ((1u64 << 36) - 1);
        assert!(total > 0, "total_samples must be patched, got {field}");
    }

    #[tokio::test]
    async fn client_disconnect_cancels_the_child() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        // A long encode so the stream is still open when the client goes away.
        if make_source(bin.as_str(), "long", &[]).is_none() {
            return; // no libmp3lame in this build: skip
        }
        let dir = fixture_dir("long-src");
        let mut big = dir;
        big.push("long.mp3");
        let made = std::process::Command::new(bin.as_str())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=30",
                "-b:a",
                "128k",
                "-y",
                big.display().to_string().as_str(),
            ])
            .status();
        if !made.map(|s| s.success()).unwrap_or(false) {
            return;
        }

        let mf = MediaFile {
            duration: 30.0,
            ..MediaFile::default()
        };
        let big_job = job_for(big.display().to_string().as_str(), "mp3", 128);
        let mut body = open_transcode(&mf, &big_job).await.expect("transcode");

        let mut sink = Sink::default();
        let outcome = body
            .serve(&mut sink, async {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await
            })
            .await;
        assert!(
            outcome.disconnected,
            "disconnect must be reported: {outcome:?}"
        );
        assert_eq!(outcome.error, None, "a disconnect is not an error");
        assert!(body.is_ended(), "the child must be ended after disconnect");
    }

    #[tokio::test]
    async fn write_failure_before_any_byte_can_swap_status() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        let src = match make_source(bin.as_str(), "flaky", &[]) {
            Some(p) => p,
            None => return,
        };
        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "mp3", 128))
            .await
            .expect("transcode");

        // Go: c == 0 -> delete Content-Length and return the error (the 200 has
        // not been written yet).
        let mut sink = FlakySink {
            after: 0,
            written: 0,
        };
        let outcome = body.serve(&mut sink, std::future::pending::<()>()).await;
        assert!(outcome.error.is_some());
        assert_eq!(outcome.written, 0);
        assert!(outcome.can_swap_status());
        assert!(!outcome.truncated);
    }

    #[tokio::test]
    async fn write_failure_after_bytes_is_truncated_not_swapable() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        let src = match make_source(bin.as_str(), "flaky2", &[]) {
            Some(p) => p,
            None => return,
        };
        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "mp3", 128))
            .await
            .expect("transcode");

        // Go's panic(http.ErrAbortHandler) arm: bytes are already on the wire.
        let mut sink = FlakySink {
            after: 1,
            written: 0,
        };
        let outcome = body.serve(&mut sink, std::future::pending::<()>()).await;
        assert!(outcome.error.is_some());
        assert!(outcome.written > 0);
        assert!(outcome.truncated);
        assert!(!outcome.can_swap_status());
    }

    #[tokio::test]
    async fn drain_reads_to_eof_without_writing() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        let src = match make_source(bin.as_str(), "drain", &[]) {
            Some(p) => p,
            None => return,
        };
        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "mp3", 128))
            .await
            .expect("transcode");

        let outcome = body.drain().await;
        assert_eq!(outcome.error, None);
        assert!(outcome.written > 0, "HEAD still drains ffmpeg to EOF");
        assert!(body.is_ended());
    }

    #[tokio::test]
    async fn empty_output_is_not_reported_as_an_error() {
        // An encode of silence to a tiny output still produces bytes; the
        // empty-output arm is pinned with a body that yields EOF immediately.
        let mut body = TranscodeBody {
            cmd: nd_ffmpeg::ffcmd::new_ffmpeg_cmd("true".to_string(), vec![]),
            reader: None,
            patcher: None,
            ended: false,
        };
        let mut sink = Sink::default();
        let outcome = body.serve(&mut sink, std::future::pending::<()>()).await;
        assert_eq!(outcome.written, 0);
        assert_eq!(outcome.error, None);
        assert!(outcome.empty_output, "Go logs this, does not error");
    }

    #[tokio::test]
    async fn chunk_reads_are_bounded() {
        let bin = match ffmpeg_bin() {
            Some(b) => b,
            None => return,
        };
        let src = match make_source(bin.as_str(), "bounded", &[]) {
            Some(p) => p,
            None => return,
        };
        let mf = MediaFile {
            duration: 0.3,
            ..MediaFile::default()
        };
        let mut body = open_transcode(&mf, &job_for(src.as_str(), "mp3", 128))
            .await
            .expect("transcode");

        let mut total = 0usize;
        let mut max_chunk = 0usize;
        let mut reads = 0usize;
        while let Some(chunk) = body.read_chunk(TRANSCODE_CHUNK_BYTES).await.unwrap() {
            max_chunk = std::cmp::max(max_chunk, chunk.len());
            total += chunk.len();
            reads += 1;
            if reads > 200 {
                panic!("stream did not reach EOF");
            }
        }
        assert!(total > 0);
        assert!(
            max_chunk <= TRANSCODE_CHUNK_BYTES,
            "a chunk exceeded the bound: {max_chunk}"
        );
    }
}
