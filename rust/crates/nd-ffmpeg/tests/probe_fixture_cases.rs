//! Remaining fixture cases for the probe path — the `tests/fixtures/symlink*`
//! chains and the `tests/fixtures/invalid-files/` container left over from #07
//! (`docs/rust-port/next-work.md:43`). The Go-side pins these mirror:
//! `scanner/walk_dir_tree_test.go:453-470` classifies
//! `symlink_chain/level3.mp3` (chain to a real audio file) and
//! `symlink_chain/evil3.mp3` (chain to `index.html`), while
//! `adapters/gotaglib/gotaglib_test.go:265` feeds
//! `invalid-files/test-invalid-frame.mp3` to a parser.
//!
//! Invariant pinned here (already encoded in the crate, asserted — never
//! changed): unparsable or missing containers fail as LAUNCH errors, i.e. a
//! `ProbeError` whose `not_found` flag comes from the file's stat
//! classification (`stat_token`, Go's `fileExists`, `ffmpeg.go:141-149`)
//! through `file_access_reason` (`fileAccessReason`, `ffmpeg.go:200-209`)
//! — NEVER as parse errors (`parseProbeOutput`'s "parsing ffprobe output:"
//! only follows a *successful* ffprobe run). Live-ffprobe cases SKIP-IF-MISSING
//! on `is_probe_available` (Go's suite assumes ffprobe on PATH; CI without
//! ffmpeg asserts nothing).

use nd_ffmpeg::ffmpeg_cmd::is_probe_available;
use nd_ffmpeg::probe_exec::probe_audio_stream;
use nd_ffmpeg::probe_exec::probe_audio_stream_with;
use nd_ffmpeg::probe_exec::stat_token;

use std::path::PathBuf;

/// The worktree root: first ancestor of the crate manifest dir holding `go.mod`
/// (house fixture-root rule, same as `nd-lyrics/src/lib.rs`).
fn repo_root() -> PathBuf {
    let manifest: PathBuf = PathBuf::new().join(env!("CARGO_MANIFEST_DIR"));
    let found = manifest.ancestors().find(|p| p.join("go.mod").is_file());
    found.unwrap().to_path_buf()
}

/// Absolute path (as String) of a repo-root `tests/fixtures/` entry.
fn fixture(name: String) -> String {
    let p: PathBuf = repo_root().join("tests/fixtures").join(&name);
    p.to_string_lossy().into_owned()
}

/// SKIP-IF-MISSING: a fixture-backed case asserts nothing when the checkout
/// lacks the fixture — the port never fabricates a stand-in.
fn fixture_missing(name: String) -> bool {
    let p: PathBuf = PathBuf::new().join(fixture(name.to_string()));
    !(p.is_file() || p.is_dir() || p.is_symlink())
}

/// The stand-in ffprobe that never touches the media file: Go's suite mutates
/// the `ffmpegPath` package global (`core/ffmpeg/ffmpeg_test.go:37`) to drive
/// the stat/launch branches without a real ffprobe; the port passes it
/// explicitly through [`nd_ffmpeg.probe_exec.probe_audio_stream_with`].
const STUB_BINARY: &str = "/bin/echo";

/// `tests/fixtures/symlink` -> `index.html`: the stat succeeds (symlink to a
/// regular file), so ffprobe runs and rejects the HTML as media (exit 1).
/// Launch error, never parse error.
#[tokio::test]
async fn probe_symlink_to_nonaudio_container_is_launch_error() {
    if fixture_missing("symlink".to_string()) || !is_probe_available() {
        return; // skip-if-missing: fixture absent or no ffprobe resolved
    }

    // Arrange / Act
    let media = fixture("symlink".to_string());
    let res = probe_audio_stream(media.to_string()).await;
    // Assert: launch failure (ffprobe "Invalid data found when processing
    // input"), never a parse error; the file exists so NotFound stays false.
    let err = res.unwrap_err();
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
    assert!(err.reason.contains("Invalid data found"));
    assert_eq!(stat_token(media.to_string()), "".to_string());
}

/// `symlink_chain/evil3.mp3 -> evil2.mp3 -> evil1.mp3 -> ../index.html`: the
/// multi-hop chain Go's classifier walks down to a non-audio target
/// (`walk_dir_tree_test.go:460`).
#[tokio::test]
async fn probe_symlink_chain_to_html_is_launch_error() {
    if fixture_missing("symlink_chain/evil3.mp3".to_string()) || !is_probe_available() {
        return; // skip-if-missing
    }

    // Arrange / Act
    let media = fixture("symlink_chain/evil3.mp3".to_string());
    let res = probe_audio_stream(media.to_string()).await;
    // Assert: same launch-error shape as the direct symlink case.
    let err = res.unwrap_err();
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
    assert!(err.reason.contains("Invalid data found"));
}

/// `symlink_chain/level3.mp3 -> level2.mp3 -> level1.mp3 -> ../test.mp3`: the
/// chain lands on a real MP3, so `ProbeAudioStream` must parse normally
/// (pins verified against ffprobe on this box).
#[tokio::test]
async fn probe_symlink_chain_to_real_audio_probes_normally() {
    if fixture_missing("symlink_chain/level3.mp3".to_string()) || !is_probe_available() {
        return; // skip-if-missing
    }

    // Arrange / Act
    let media = fixture("symlink_chain/level3.mp3".to_string());
    let res = probe_audio_stream(media.to_string()).await;
    let r = res.unwrap();
    // Assert
    assert_eq!(r.codec, "mp3".to_string());
    assert_eq!(r.sample_rate, 44100);
    assert_eq!(r.channels, 2);
    assert_eq!(r.bit_rate, 192); // stream bit_rate 192000 bps -> kbps
    assert_eq!(r.bit_depth, 0);  // lossy codec reports no depth
}

/// `symlink2dir` -> `empty_folder`: statted through the symlink (Go's
/// `fileExists` follows links) the path is a directory, so
/// `ProbeAudioStream` answers from the directory branch (`ffmpeg.go:143`)
/// without launching.
#[tokio::test]
async fn probe_symlink_to_directory_is_inaccessible_not_not_found() {
    if fixture_missing("symlink2dir".to_string()) {
        return; // skip-if-missing
    }

    // Arrange / Act: the stub binary proves the launch never happens.
    let media = fixture("symlink2dir".to_string());
    let res =
        probe_audio_stream_with(STUB_BINARY.to_string(), media.to_string()).await;
    // Assert: ProbeError carrying the file-access reason, NotFound false (the
    // path is there, it is just not a media file).
    let err = res.unwrap_err();
    assert_eq!(stat_token(media.to_string()), "inaccessible".to_string());
    assert_eq!(err.reason, "file not accessible".to_string());
    assert!(!err.not_found);
    assert!(!err.reason.contains("parsing ffprobe output"));
}

/// The missing-container half of the invariant: a path under `invalid-files/`
/// that does not exist must fail with `ProbeError` carrying NotFound, derived
/// from the stat classification. No ffprobe and no fixture needed — CI-safe.
#[tokio::test]
async fn probe_missing_container_under_invalid_files_is_not_found() {
    // Arrange / Act
    let media =
        fixture("invalid-files/this-container-does-not-exist.flac".to_string());
    let res =
        probe_audio_stream_with(STUB_BINARY.to_string(), media.to_string()).await;
    // Assert
    let err = res.unwrap_err();
    assert!(err.not_found);
    assert_eq!(err.reason, "file not found".to_string());
    assert_eq!(stat_token(media.to_string()), "not_found".to_string());
}

/// `invalid-files/test-invalid-frame.mp3` is "invalid" for Go's *tag* parser
/// (`adapters/gotaglib/gotaglib_test.go:265`) but ffprobe still decodes it as
/// MP3 — the probe must parse it, proving the parse-error path stays reserved
/// for bad ffprobe stdout.
#[tokio::test]
async fn probe_invalid_frame_container_parses() {
    if fixture_missing("invalid-files/test-invalid-frame.mp3".to_string()) {
        return; // skip-if-missing
    }
    if !is_probe_available() {
        return; // skip-if-missing: no ffprobe resolved
    }

    // Arrange / Act
    let media = fixture("invalid-files/test-invalid-frame.mp3".to_string());
    let res = probe_audio_stream(media.to_string()).await;
    let r = res.unwrap();
    // Assert
    assert_eq!(r.codec, "mp3".to_string());
    assert_eq!(r.sample_rate, 44100);
    assert_eq!(r.channels, 2);
    assert_eq!(r.bit_rate, 192);
}

/// Sweep: every unparsable/missing container from the two fixture areas must
/// fail as a launch/stat error — the parse-error string may never appear, and
/// NotFound is true only when the file's stat classified it as `not_found`.
#[tokio::test]
async fn probe_unparsable_fixtures_never_report_parse_errors() {
    if !is_probe_available() {
        return; // skip-if-missing: ProbeAudioStream resolves the binary first
    }

    // Arrange: the real fixtures only — the deliberately-missing container is
    // covered by its own case above, never counted through this skip guard.
    let names: Vec<String> = vec![
        "symlink".to_string(),
        "symlink2dir".to_string(),
        "symlink_chain/evil3.mp3".to_string(),
    ];

    let mut checked = 0;
    for name in names.iter() {
        if fixture_missing(name.to_string()) {
            continue; // skip-if-missing per fixture
        }
        let media = fixture(name.to_string());
        // Act
        let res = probe_audio_stream(media.to_string()).await;
        // Assert: fail, but only as launch/stat errors.
        let err = res.unwrap_err();
        assert!(!err.reason.contains("parsing ffprobe output"));
        assert_eq!(err.not_found, stat_token(media.to_string()) == "not_found");
        checked += 1;
    }
    assert_eq!(checked, 3);
}
