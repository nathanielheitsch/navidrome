//! Probe adapter — the #15 bridge between nd-ffmpeg's ffprobe verbs (#07) and
//! the decision layer's [`ProbeInput`], which is where Go's
//! `ensureProbed`/`IsProbeAvailable` branch lives
//! (`core/stream/decider.go:48-58,431-459`).
//!
//! Go reaches for the probe inside `MakeDecision`: `IsProbeAvailable` first,
//! then `ProbeAudioStream` if the row has no stored `ProbeData`, and a probe
//! failure is the only error `MakeDecision` can return
//! (`decider.go:55`). The port keeps that branch table but moves the launch
//! out of the decision layer — `decider.rs` receives a verdict, so
//! `make_decision` stays infallible (see the divergence note in `lib.rs`) and
//! the fallible part lives here.
//!
//! // divergence: Go's `log.Error` on probe failure is dropped; the reason
//! // string travels to the caller instead, so the HTTP layer decides what a
//! // client sees. `ProbeError::reason` is already path-free
//! // (`nd-ffmpeg/src/errors.rs`: the client-safe `SafeReason()` half).

use nd_ffmpeg::errors::ProbeError;
use nd_ffmpeg::ffmpeg_cmd::is_probe_available;
use nd_ffmpeg::probe::AudioProbeResult;
use nd_ffmpeg::probe_exec::{probe_audio_stream, probe_audio_stream_with};

use crate::decider::ProbeInput;
use crate::types::MediaFile;

/// `IsProbeAvailable` (`decider.go:48-50`, `ffmpeg.go:626-640`): ffprobe can
/// run at all. The port asks nd-ffmpeg, which resolves ffprobe as the sibling
/// of the configured/PATH ffmpeg.
#[must_use]
pub fn probe_available() -> bool {
    is_probe_available()
}

/// `ensureProbed` (`decider.go:431-459`) for an already-authorized path.
///
/// Mirrors Go's decision table:
///   - `SkipProbe` or a row that already carries `ProbeData` ⇒ no probe runs
///     (`decider.go:433-436`), so the verdict is
///     [`ProbeInput::NotAvailable`] and the decision layer falls back to
///     `ProbeData`, then tag metadata (`build_source_stream`,
///     `decider.go:131-149`).
///   - ffprobe unavailable ⇒ [`ProbeInput::NotAvailable`], which is Go's
///     `if !IsProbeAvailable() { return }` arm (`decider.go:440-442`).
///   - ffprobe succeeded ⇒ [`ProbeInput::Probed`].
///   - ffprobe failed ⇒ Go returns the error from `MakeDecision`
///     (`decider.go:451-456`); the port returns `Err(ProbeError)` so the
///     caller can distinguish "cannot probe" from "probed as something
///     unplayable". The decision layer itself is never reached in that case.
///
/// `path` must be an **already-authorized** service-layer path, exactly as in
/// [`crate::direct`] — this module derives nothing and authorizes nothing.
pub async fn ensure_probed(
    mf: &MediaFile,
    path: &str,
    skip_probe: bool,
) -> Result<ProbeInput, ProbeError> {
    if skip_probe || !mf.probe_data.is_empty() {
        // Go: probeData is already set, so ensureProbed returns without
        // launching ffprobe (decider.go:433-437).
        return Ok(ProbeInput::NotAvailable);
    }
    if !probe_available() {
        return Ok(ProbeInput::NotAvailable);
    }
    let result = probe_audio_stream(path.to_string()).await?;
    Ok(ProbeInput::Probed(result))
}

/// `ProbeAudioStream` with an explicit binary — the port's stand-in for Go's
/// suite mutating the `ffmpegPath` package global
/// (`core/ffmpeg/ffmpeg_test.go:37,:67,:72,:764-768`), so a mock binary can
/// drive both the success and failure arms without touching configuration.
pub async fn probe_with_binary(binary: &str, path: &str) -> Result<AudioProbeResult, ProbeError> {
    probe_audio_stream_with(binary.to_string(), path.to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MediaFile;

    /// Fixtures are created in the OS temp dir and addressed through
    /// `std::env::temp_dir()` only — never an absolute developer path, which is
    /// what broke four tests on every other host.
    fn fixture(name: &str, bytes: &[u8]) -> String {
        let mut dir = std::env::temp_dir();
        dir.push(format!("nd-stream-probe-{name}"));
        std::fs::create_dir_all(dir.as_path()).expect("temp fixture dir");
        dir.push(name);
        std::fs::write(dir.as_path(), bytes).expect("write fixture");
        dir.display().to_string()
    }

    #[tokio::test]
    async fn stored_probe_data_skips_the_probe() {
        // Arrange: a row whose ProbeData is set, so Go never launches ffprobe.
        let mf = MediaFile {
            probe_data: "{\"streams\":[]}".to_string(),
            ..MediaFile::default()
        };
        // Act: the path does not even need to exist.
        let input = ensure_probed(&mf, "/no/such/file.flac", false).await;
        // Assert
        assert!(input.is_ok());
        assert!(matches!(input.unwrap(), ProbeInput::NotAvailable));
    }

    #[tokio::test]
    async fn skip_probe_flag_skips_the_probe() {
        let mf = MediaFile::default();
        let input = ensure_probed(&mf, "/no/such/file.flac", true)
            .await
            .unwrap();
        assert!(matches!(input, ProbeInput::NotAvailable));
    }

    #[tokio::test]
    async fn mock_binary_produces_a_probe_error_not_a_panic() {
        // Arrange: /bin/echo prints its argv, which is never valid ffprobe
        // JSON, so the parse arm fires (Go: ffmpeg.go:167).
        let path = fixture("echo-probe.mp3", b"id3 not really audio");
        // Act
        let res = probe_with_binary("/bin/echo", path.as_str()).await;
        // Assert: the error is the client-safe reason, path-free.
        let err = res.unwrap_err();
        assert!(!err.not_found, "the media file exists");
        assert!(
            !err.reason.contains(path.as_str()),
            "reason must stay path-free, got {err:?}"
        );
    }

    #[tokio::test]
    async fn missing_file_is_reported_as_not_found() {
        // Go: ffmpeg_test.go:623-631 — the stat arm short-circuits the launch.
        let mf = MediaFile::default();
        let res = ensure_probed(&mf, "/no/such/dir/really-missing.flac", false).await;
        match res {
            Err(err) => assert!(err.not_found),
            // On a box without ffprobe the availability arm answers first,
            // which is also a legitimate outcome here.
            Ok(input) => assert!(matches!(input, ProbeInput::NotAvailable)),
        }
    }

    #[tokio::test]
    async fn live_probe_yields_a_probed_input() {
        // Skip-if-missing, house style. Build a real WAV in the temp dir with
        // ffmpeg when it is available, then probe it through the adapter.
        let (bin, ok) = nd_ffmpeg::ffmpeg_cmd::ffmpeg_cmd();
        if !ok || !probe_available() {
            return;
        }
        let out = fixture("live-probe-src", b"");
        let made = std::process::Command::new(&bin)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.5",
                "-y",
                out.as_str(),
            ])
            .status();
        if made.is_err() || !made.unwrap().success() {
            return; // lavfi unavailable in this build: skip
        }

        let mf = MediaFile::default();
        let input = ensure_probed(&mf, out.as_str(), false).await.unwrap();
        match input {
            ProbeInput::Probed(r) => {
                assert!(!r.codec.is_empty(), "codec must be parsed");
                assert!(r.sample_rate > 0);
            }
            ProbeInput::NotAvailable => panic!("ffprobe resolved but no verdict came back"),
        }
    }
}
