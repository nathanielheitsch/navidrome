//! In-process mock of the `FFmpeg` interface — port of `tests/mock_ffmpeg.go`
//! (91 LOC at the repo root's `tests/`, Go package `tests`). Go's
//! `MockFFmpeg` is a test double that satisfies the whole `core/ffmpeg/ffmpeg.go`
//! interface (`IsAvailable`/`IsProbeAvailable`/`Transcode`/`ExtractImage`/
//! `ConvertAnimatedImage`/`ProbeAudioStream`/`CmdPath`/`Version`) *and* the
//! `io.Reader` contract (`Read`/`Close`/`IsClosed`), so the artwork/stream suites
//! can drive a real stream consumer without ever spawning ffmpeg.
//!
//! Go models the reader with `strings.NewReader(data)` behind a `sync.Mutex`
//! plus an `atomic.Bool closed`; the port keeps the same observable state as
//! plain fields (`data` + `offset` + `closed`) because Rust ownership replaces
//! Go's shared-handle-plus-mutex: a verb that hands the reader to a consumer
//! hands over the value, and `close()` mutates it in place.
//!
//! Go's `MockFFmpeg.Error` is an arbitrary `error`; the port pins it to the
//! message of the crate's launch-failure shape [`crate::errors.FFmpegError`]
//! (that enum is not `Clone`, so the mock stores/re-renders rather than
//! re-shares it) and converts to [`crate::errors.ProbeError`] at the probe verb
//! exactly where Go's `ProbeAudioStream` swaps shapes (`ffmpeg.go:150-168`).

use crate::args::TranscodeOptions;
use crate::errors::{FFmpegError, ProbeError};
use crate::probe::AudioProbeResult;

/// Go's `MockFFmpeg` struct (`tests/mock_ffmpeg.go:18-26`).
#[derive(Debug, Clone)]
pub struct MockFFmpeg {
    /// Go's `io.Reader` (`Reader: strings.NewReader(data)`, `:15`): the canned
    /// stdout bytes, plus the read cursor `strings.NewReader` carries.
    pub data: Vec<u8>,
    /// Read cursor; Go's `io.Reader` offset inside the `StringReader`.
    pub offset: usize,
    /// Go's `closed atomic.Bool` (`:21`), flipped by `MockFFmpeg.Close`.
    pub closed: bool,
    /// Go's `Error error` (`:22`): the injected failure every verb returns
    /// before doing any work. Go stores an arbitrary `error`; the port stores
    /// its rendering and re-derives the shape each verb raises —
    /// [`FFmpegError::NotAvailable`] for the launch verbs (the only failure
    /// `transcode.rs`/`ffmpeg_cmd.rs` raise there) and [`ProbeError`] for
    /// `probe_audio_stream`.
    pub error: Option<String>,
    /// Go's `ProbeAudioResult *ffmpeg.AudioProbeResult` (`:23`): the canned
    /// probe verdict `ProbeAudioStream` hands back.
    pub probe_audio_result: AudioProbeResult,
    /// Go's `ProbeAvailable bool` (`:24`), set by `NewMockFFmpeg`.
    pub probe_available: bool,
}

/// `NewMockFFmpeg` (`tests/mock_ffmpeg.go:14-16`): canned stdout, probe available.
pub fn new_mock_ffmpeg(data: String) -> MockFFmpeg {
    MockFFmpeg {
        data: data.as_bytes().to_vec(),
        offset: 0,
        closed: false,
        error: None,
        probe_audio_result: AudioProbeResult::default(),
        probe_available: true,
    }
}

/// Fresh reader over bytes — the port's spelling of Go's
/// `io.NopCloser(bytes.NewReader(data))` (`tests/mock_ffmpeg.go:56`).
fn new_reader(data: Vec<u8>) -> MockFFmpeg {
    MockFFmpeg {
        data,
        offset: 0,
        closed: false,
        error: None,
        probe_audio_result: AudioProbeResult::default(),
        probe_available: true,
    }
}

/// Re-derive the injected failure as the launch-verb error shape
/// (`MockFFmpeg.Error`, `tests/mock_ffmpeg.go:22`).
fn injected_failure(ff: &MockFFmpeg) -> Option<FFmpegError> {
    ff.error.clone().map(|rendered| FFmpegError::NotAvailable(rendered.clone()))
}

/// Go's probe verb returns a [`crate::errors.ProbeError`] where the other verbs
/// raise [`crate::errors.FFmpegError`] (`ffmpeg.go:150-168`), so the injected
/// failure needs re-shaping at that boundary only: the stored rendering becomes
/// Go's `ProbeError.Reason` verbatim, as `ProbeAudioStream` does with `err.Error()`.
fn probe_failure(path: String, rendered: String) -> ProbeError {
    ProbeError {
        path,
        reason: rendered,
        not_found: false,
    }
}

impl MockFFmpeg {
    /// Go's `MockFFmpeg.IsAvailable` (`tests/mock_ffmpeg.go:27-29`): a mock is
    /// always an available binary — the point of the mock is that no lookup runs.
    pub fn is_available(self: &MockFFmpeg) -> bool {
        true
    }

    /// Go's `MockFFmpeg.IsProbeAvailable` (`tests/mock_ffmpeg.go:31-33`).
    pub fn is_probe_available(self: &MockFFmpeg) -> bool {
        self.probe_available
    }

    /// Go's `MockFFmpeg.Transcode` (`tests/mock_ffmpeg.go:35-40`): ignore the
    /// options, fail when an error is injected, else hand back the reader.
    pub fn transcode(self: &MockFFmpeg, _opts: TranscodeOptions) -> Result<MockFFmpeg, FFmpegError> {
        match injected_failure(self) {
            Some(err) => Err(err),
            None => Ok(self.clone()),
        }
    }

    /// Go's `MockFFmpeg.ExtractImage` (`tests/mock_ffmpeg.go:42-47`).
    pub fn extract_image(self: &MockFFmpeg, _path: String) -> Result<MockFFmpeg, FFmpegError> {
        match injected_failure(self) {
            Some(err) => Err(err),
            None => Ok(self.clone()),
        }
    }

    /// Go's `MockFFmpeg.ConvertAnimatedImage` (`tests/mock_ffmpeg.go:49-58`):
    /// drain the stdin `io.Reader` and answer a fresh reader over the same bytes.
    /// The port's stand-in for Go's `bytes.NewReader` argument is another
    /// [`MockFFmpeg`] (its own canned bytes), and Go's `io.ReadAll` read error
    /// has no counterpart — canned bytes always drain to EOF.
    pub fn convert_animated_image(
        self: &MockFFmpeg,
        reader: MockFFmpeg,
        _max_size: i64,
        _quality: i64,
    ) -> Result<MockFFmpeg, FFmpegError> {
        match injected_failure(self) {
            Some(err) => Err(err),
            None => {
                let from = if reader.offset <= reader.data.len() {
                    reader.offset
                } else {
                    reader.data.len()
                };
                Ok(new_reader(reader.data[from..].to_vec()))
            }
        }
    }

    /// Go's `MockFFmpeg.ProbeAudioStream` (`tests/mock_ffmpeg.go:60-65`): the
    /// canned [`crate::probe.AudioProbeResult`], no launch.
    pub fn probe_audio_stream(self: &MockFFmpeg, path: String) -> Result<AudioProbeResult, ProbeError> {
        match self.error {
            None => Ok(self.probe_audio_result.clone()),
            Some(ref rendered) => Err(probe_failure(path.clone(), rendered.clone())),
        }
    }


    /// Go's `MockFFmpeg.CmdPath` (`tests/mock_ffmpeg.go:67-72`): the literal
    /// `"ffmpeg"` Go returns at `:69`.
    pub fn cmd_path(self: &MockFFmpeg) -> Result<String, FFmpegError> {
        match injected_failure(self) {
            Some(err) => Err(err),
            None => Ok("ffmpeg".to_string()),
        }
    }

    /// Go's `MockFFmpeg.Version` (`tests/mock_ffmpeg.go:74-76`): canned banner
    /// token, never the real `<ffmpeg> -version` probe.
    pub fn version(self: &MockFFmpeg) -> String {
        "1.0".to_string()
    }

    /// Go's `MockFFmpeg.Read` (`tests/mock_ffmpeg.go:78-82`): up to `want`
    /// bytes of the canned stdout, advancing the cursor. Go's `(n, err)` pair
    /// reports `io.EOF`; here that is an empty return, so a caller looping
    /// `read` sees Go's `read_to_end` shape.
    pub fn read(self: &mut MockFFmpeg, want: usize) -> Vec<u8> {
        if self.offset >= self.data.len() {
            return Vec::new();
        }
        let stop = self.offset + want;
        let stop = if stop <= self.data.len() { stop } else { self.data.len() };
        let chunk = self.data[self.offset..stop].to_vec();
        self.offset = stop;
        chunk
    }

    /// Go's `MockFFmpeg.Close` (`tests/mock_ffmpeg.go:84-86`): flip the flag.
    /// Go returned `error`; the mock cannot fail, so the port returns nothing.
    pub fn close(self: &mut MockFFmpeg) {
        self.closed = true;
    }

    /// Go's `MockFFmpeg.IsClosed` (`tests/mock_ffmpeg.go:89-91`).
    pub fn is_closed(self: &MockFFmpeg) -> bool {
        self.closed
    }

    /// Test-side convenience for Go's `ff.Error = ...`: take any crate error and
    /// stage its rendering as the injected failure.
    pub fn inject_ffmpeg_error(self: &mut MockFFmpeg, err: FFmpegError) {
        self.error = Some(err.to_string());
    }
}

#[cfg(test)]
mod mock_ffmpeg_tests {
    use super::*;
    use crate::errors::FFmpegError;

    /// Go's `MockFFmpeg.ProbeAudioResult` round trip: the canned verdict is
    /// what `ProbeAudioStream` hands back, and the canned stdout is what
    /// `Read` streams.
    #[test]
    fn canned_probe_result_round_trips() {
        // Arrange: the canned probe payload the fixtures pin (tests/fixtures/test.flac).
        let mut ff = new_mock_ffmpeg("canned-probe-out".to_string());
        ff.probe_audio_result = AudioProbeResult {
            codec: "flac".to_string(),
            profile: "".to_string(),
            bit_rate: 1411,
            sample_rate: 44100,
            bit_depth: 16,
            channels: 2,
        };
        // Assert
        assert!(ff.is_available());
        assert!(ff.is_probe_available());
        assert_eq!(ff.cmd_path().unwrap(), "ffmpeg".to_string());
        assert_eq!(ff.version(), "1.0".to_string());
        let res = ff.probe_audio_stream("/music/test.flac".to_string());
        match res {
            Err(_) => panic!("no error injected: the canned result must come back"),
            Ok(r) => {
                assert_eq!(r.codec, "flac".to_string());
                assert_eq!(r.bit_rate, 1411);
                assert_eq!(r.sample_rate, 44100);
                assert_eq!(r.bit_depth, 16);
                assert_eq!(r.channels, 2);
            }
        }
        // Act: stream the canned bytes the reader would hand a consumer.
        assert_eq!(ff.read(8), b"canned-p".to_vec());
        assert_eq!(ff.read(8), b"robe-out".to_vec());
        assert!(ff.read(8).is_empty());
    }

    /// Go's `MockFFmpeg.Error` short-circuits every verb, and the probe verb
    /// re-shapes it as Go's `ProbeError`.
    #[test]
    fn injected_error_propagates() {
        // Arrange
        let mut ff = new_mock_ffmpeg("payload".to_string());
        ff.inject_ffmpeg_error(FFmpegError::NotAvailable("/not/ffmpeg".to_string()));
        // Assert: availability is unaffected (Go's mock never looks up).
        assert!(ff.is_available());
        assert_eq!(ff.version(), "1.0".to_string());
        let opts = TranscodeOptions {
            command: "".to_string(),
            format: "mp3".to_string(),
            file_path: "/music/test.mp3".to_string(),
            bit_rate: 192,
            sample_rate: 0,
            channels: 0,
            bit_depth: 0,
            offset: 0,
            duration: 0.0,
        };
        let t = ff.transcode(opts);
        match t {
            Ok(_) => panic!("injected error must short-circuit Transcode"),
            Err(err) => assert!(matches!(err, FFmpegError::NotAvailable(_))),
        }
        let p = ff.probe_audio_stream("/music/test.mp3".to_string());
        match p {
            Ok(_) => panic!("injected error must short-circuit ProbeAudioStream"),
            Err(pe) => {
                assert_eq!(pe.path, "/music/test.mp3".to_string());
                assert_eq!(pe.reason, "ffmpeg not available: /not/ffmpeg".to_string());
                assert!(!pe.not_found);
            }
        }
        let img = ff.extract_image("/music/cover.m4a".to_string());
        match img {
            Ok(_) => panic!("injected error must short-circuit ExtractImage"),
            Err(err) => assert!(matches!(err, FFmpegError::NotAvailable(_))),
        }
        let c = ff.convert_animated_image(new_reader("gif".to_string().as_bytes().to_vec()), 250, 80);
        match c {
            Ok(_) => panic!("injected error must short-circuit ConvertAnimatedImage"),
            Err(err) => assert!(matches!(err, FFmpegError::NotAvailable(_))),
        }
    }

    /// Go's `Close`/`IsClosed` pair: the flag is false until `Close`, and a
    /// drained reader keeps serving EOF rather than wrapping around.
    #[test]
    fn close_flips_the_closed_flag() {
        // Arrange
        let mut ff = new_mock_ffmpeg("ab".to_string());
        // Assert
        assert!(!ff.is_closed());
        assert_eq!(ff.read(2), b"ab".to_vec());
        assert!(ff.read(2).is_empty());
        // Act: Go's MockFFmpeg.Close
        ff.close();
        // Assert
        assert!(ff.is_closed());
        assert!(ff.read(2).is_empty());
        // The converted-image reader starts clean, as Go's bytes.NewReader does.
        let src = new_reader("gif-bytes".to_string().as_bytes().to_vec());
        let converted = ff.convert_animated_image(src, 250, 80);
        match converted {
            Err(_) => panic!("no error injected here"),
            Ok(mut r) => assert_eq!(r.read(3), b"gif".to_vec()),
        }
    }
}
