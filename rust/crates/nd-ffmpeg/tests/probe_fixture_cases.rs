//! Real checkout fixtures are mandatory; only external ffprobe is optional.
//! Invalid media must fail at stat/launch, never at JSON parsing.

use nd_ffmpeg::ffmpeg_cmd::is_probe_available;
use nd_ffmpeg::probe_exec::{probe_audio_stream, probe_audio_stream_with, stat_token};

#[path = "support/fixtures.rs"]
mod fixtures;
use fixtures::{fixture, fixture_path};

const STUB_BINARY: &str = "/bin/echo";

#[tokio::test]
async fn probe_symlink_to_nonaudio_container_is_launch_error() {
    let media = fixture("symlink");
    if !is_probe_available() {
        return;
    }
    let err = probe_audio_stream(media.clone()).await.unwrap_err();
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
    assert!(err.reason.contains("Invalid data found"));
    assert_eq!(stat_token(media), "");
}

#[tokio::test]
async fn probe_symlink_chain_to_html_is_launch_error() {
    let media = fixture("symlink_chain/evil3.mp3");
    if !is_probe_available() {
        return;
    }
    let err = probe_audio_stream(media).await.unwrap_err();
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
    assert!(err.reason.contains("Invalid data found"));
}

#[tokio::test]
async fn probe_symlink_chain_to_real_audio_probes_normally() {
    let media = fixture("symlink_chain/level3.mp3");
    if !is_probe_available() {
        return;
    }
    let r = probe_audio_stream(media).await.unwrap();
    assert_eq!(r.codec, "mp3");
    assert_eq!(r.sample_rate, 44100);
    assert_eq!(r.channels, 2);
    assert_eq!(r.bit_rate, 192);
    assert_eq!(r.bit_depth, 0);
}

#[tokio::test]
async fn probe_symlink_to_directory_is_inaccessible_not_not_found() {
    let media = fixture("symlink2dir");
    let err = probe_audio_stream_with(STUB_BINARY.to_string(), media.clone())
        .await
        .unwrap_err();
    assert_eq!(stat_token(media), "inaccessible");
    assert_eq!(err.reason, "file not accessible");
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
}

#[tokio::test]
async fn probe_missing_container_under_invalid_files_is_not_found() {
    let media = fixture_path("invalid-files/this-container-does-not-exist.flac")
        .to_string_lossy()
        .into_owned();
    assert!(!std::path::Path::new(&media).exists());
    let err = probe_audio_stream_with(STUB_BINARY.to_string(), media.clone())
        .await
        .unwrap_err();
    assert!(err.not_found);
    assert_eq!(err.reason, "file not found");
    assert_eq!(stat_token(media), "not_found");
}

#[tokio::test]
async fn probe_invalid_frame_container_parses() {
    let media = fixture("invalid-files/test-invalid-frame.mp3");
    if !is_probe_available() {
        return;
    }
    let r = probe_audio_stream(media).await.unwrap();
    assert_eq!(r.codec, "mp3");
    assert_eq!(r.sample_rate, 44100);
    assert_eq!(r.channels, 2);
    assert_eq!(r.bit_rate, 192);
}

#[tokio::test]
async fn probe_unparsable_fixtures_never_report_parse_errors() {
    let media: Vec<_> = ["symlink", "symlink2dir", "symlink_chain/evil3.mp3"]
        .iter()
        .map(|name| fixture(name))
        .collect();
    assert_eq!(media.len(), 3);
    if !is_probe_available() {
        return;
    }
    let mut checked = 0;
    for path in media {
        let err = probe_audio_stream(path.clone()).await.unwrap_err();
        assert!(!err.reason.contains("parsing ffprobe output"));
        assert!(!err.not_found);
        assert_ne!(stat_token(path), "not_found");
        checked += 1;
    }
    assert_eq!(checked, 3);
}
