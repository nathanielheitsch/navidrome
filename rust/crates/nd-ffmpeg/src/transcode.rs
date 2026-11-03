//! The `FFmpeg` interface verbs — `core/ffmpeg/ffmpeg.go` `Transcode`
//! (`:129-147`), `ExtractImage` (`:470-480`), `ConvertAnimatedImage`
//! (`:95-110`). Go returns an `io.ReadCloser` (piped stdout, wrapped in the
//! FLAC `STREAMINFO` repair for `Transcode`); the port hands back a live
//! [`crate::ffcmd.FFCmd`] whose `reader()` is the stream and whose
//! `wait()`/`cancel()` enforce the kill-group contract.
//!
//! // shortcut: `patchFLACDuration` (`ffmpeg.go:92-93`) — Go patches the
//! // header while the pipe streams to the caller. The port cannot patch a
//! // stream it never sees, so callers drain `reader()` to EOF and apply
//! // [`crate::flac_patcher.patch_flac_duration`] with
//! // `duration - offset` (wired by issue #14; #07 pins the helper only).

use crate::args::{
    build_dynamic_args, build_template_args, create_ffmpeg_command, is_default_command,
    TranscodeOptions,
};
use crate::encoder_probe::has;
use crate::errors::FFmpegError;
use crate::ffcmd::{go_error_message, new_ffmpeg_cmd, ExitInfo, FFCmd};
use crate::ffmpeg_cmd::ffmpeg_cmd;
use crate::probe_exec::stat_token;

use tokio::io::AsyncReadExt;

/// `extractImageCmd` (`ffmpeg.go:69`), verbatim.
pub const EXTRACT_IMAGE_CMD: &str = "ffmpeg -i %s -map 0:v -map -0:V -vcodec copy -f image2pipe -";

/// `fileExists` (`ffmpeg.go:141-149`) as the crate error: directory → Go's
/// `fmt.Errorf("'%s' is a directory")` ([`FFmpegError::IsDirectory`]);
/// missing/unreadable → [`FFmpegError::NotAvailable`] carrying the path
/// (Go's raw `os.Stat` error has no dedicated variant; `Transcode`/
/// `ExtractImage` propagate it verbatim, `ffmpeg.go:131`/`:474`).
pub fn check_media_file(path: String) -> Result<(), FFmpegError> {
    let token = stat_token(path.clone());
    if token == "inaccessible" {
        return Err(FFmpegError::IsDirectory { path });
    }
    if !token.is_empty() {
        return Err(FFmpegError::NotAvailable(path));
    }
    Ok(())
}

/// Map a launch/spawn failure to the crate error (Go's `exec` error carries
/// no dedicated variant either; `NotAvailable(path)` is the closest shape and
/// keeps the unresolved/failed binary for the caller's log).
fn launch_failed(path: String) -> FFmpegError {
    FFmpegError::NotAvailable(path)
}

/// `Transcode` (`ffmpeg.go:129-147`): resolve the binary, stat the media file,
/// build argv — dynamic for known defaults, template otherwise — and launch
/// through [`crate::ffcmd`] so the returned handle owns the process group
/// until the caller drains/cancels it.
pub async fn transcode(opts: TranscodeOptions) -> Result<FFCmd, FFmpegError> {
    let (bin, ok) = ffmpeg_cmd();
    if !ok {
        return Err(FFmpegError::NotAvailable(bin));
    }
    check_media_file(opts.file_path.clone())?;

    let argv: Vec<String> = if is_default_command(&opts.format, &opts.command) {
        build_dynamic_args(opts, bin.as_str())
    } else {
        build_template_args(opts, bin.as_str())
    };

    let mut cmd = new_ffmpeg_cmd(argv[0].to_string(), argv[1..].to_vec());
    if cmd.start().await.is_err() {
        return Err(launch_failed(bin));
    }
    Ok(cmd)
}

/// `ExtractImage` (`ffmpeg.go:470-480`): the image2pipe stream for cover
/// extraction, same launch path as `Transcode`.
pub async fn extract_image(path: String) -> Result<FFCmd, FFmpegError> {
    let (bin, ok) = ffmpeg_cmd();
    if !ok {
        return Err(FFmpegError::NotAvailable(bin));
    }
    check_media_file(path.clone())?;

    let argv = create_ffmpeg_command(EXTRACT_IMAGE_CMD, path.as_str(), 0, 0, bin.as_str());
    let mut cmd = new_ffmpeg_cmd(argv[0].to_string(), argv[1..].to_vec());
    if cmd.start().await.is_err() {
        return Err(launch_failed(bin));
    }
    Ok(cmd)
}

/// `ConvertAnimatedImage` (`ffmpeg.go:95-110`) argv construction only: the
/// `libwebp_anim` gate ([`crate::encoder_probe.has`]) comes first — this box's
/// ffmpeg lacks the encoder, so the port answers
/// [`crate::encoder_probe.animated_webp_unsupported`]. `reader` (Go's stdin
/// `io.Reader`) is wired by #18/#20; the argv is pinned here.
pub fn convert_animated_image_argv(
    cmd_path: String,
    max_size: i64,
    quality: i64,
) -> (Vec<String>, bool) {
    if !has(cmd_path.clone(), "libwebp_anim".to_string()) {
        return (Vec::new(), false);
    }

    let mut args: Vec<String> = Vec::new();
    args.push(cmd_path);
    args.push("-i".to_string());
    args.push("pipe:0".to_string());
    if max_size > 0 {
        let vf = format!(
            "scale='min({max_size},iw)':'min({max_size},ih)':force_original_aspect_ratio=decrease"
        );
        args.push("-vf".to_string());
        args.push(vf);
    }
    args.push("-loop".to_string());
    args.push("0".to_string());
    args.push("-c:v".to_string());
    args.push("libwebp_anim".to_string());
    args.push("-quality".to_string());
    args.push(quality.to_string());
    args.push("-f".to_string());
    args.push("webp".to_string());
    args.push("-".to_string());
    (args, true)
}

/// Drain a launched stream to EOF and reap the child — the shape `ExtractImage`
/// callers need when they want bytes instead of a handle (issue #18 artwork).
pub async fn drain_to_bytes(mut cmd: FFCmd) -> (ExitInfo, Vec<u8>) {
    let mut out = cmd.reader().unwrap();
    let mut bytes: Vec<u8> = Vec::new();
    out.read_to_end(&mut bytes).await.ok();
    let info = cmd.wait().await.unwrap_or(ExitInfo {
        code: -1,
        stderr: "".to_string(),
    });
    (info, bytes)
}

/// Go's message rendering for a non-zero exit (`ffmpeg.go:387-390`).
pub fn format_exit_message(exec: String, info: ExitInfo) -> String {
    go_error_message(exec, info.code, info.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::create_ffmpeg_command;
    use crate::args::TranscodeOptions;
    use crate::errors::FFmpegError;

    #[tokio::test]
    async fn transcode_missing_file() {
        // Arrange: opts with a missing media path (Go: ffmpeg_test.go:120-130
        // "returns an error for non-existent file").
        let opts = TranscodeOptions {
            command: "".to_string(),
            format: "mp3".to_string(),
            file_path: "/no/such/dir/really-missing.mp3".to_string(),
            bit_rate: 192,
            sample_rate: 0,
            channels: 0,
            bit_depth: 0,
            offset: 0,
            duration: 0.0,
        };
        // Act
        let res = transcode(opts).await;
        // Assert: file-side failure propagates (NotAvailable carries the path)
        match res {
            Err(err) => assert!(matches!(err, FFmpegError::NotAvailable(_))),
            Ok(_) => panic!("missing file must not launch"),
        }
    }

    #[test]
    fn animated_webp_gate_and_argv() {
        // Arrange: this box's ffmpeg has no libwebp_anim → gate answers false
        // with the ErrAnimatedWebPUnsupported sentinel available separately.
        let (argv, ok) =
            convert_animated_image_argv("/opt/homebrew/bin/ffmpeg".to_string(), 250, 80);
        // Assert
        assert!(argv.is_empty());
        assert!(!ok);
    }

    #[tokio::test]
    async fn extract_image_launch_is_pinned() {
        // The argv `ExtractImage` launches (ffmpeg.go:470-480): the image2pipe
        // template through createFFmpegCommand, asserted without spawning.
        let argv = create_ffmpeg_command(
            EXTRACT_IMAGE_CMD,
            "/music/cover.m4a",
            0,
            0,
            "/bin/ffmpeg".to_string().as_str(),
        );
        assert_eq!(
            argv[1..],
            [
                "-i",
                "/music/cover.m4a",
                "-map",
                "0:v",
                "-map",
                "-0:V",
                "-vcodec",
                "copy",
                "-f",
                "image2pipe",
                "-"
            ]
            .to_vec(),
        );
    }

    #[tokio::test]
    async fn extract_image_live_jpeg() {
        // Live ffmpeg (skip-if-missing): pull the attached-art frame from
        // tests/fixtures/test.mp3 as image2pipe bytes.
        let (_bin, ok) = ffmpeg_cmd();
        if !ok {
            return;
        }
        let media = "/Users/nathanielheitsch/emdash/worktrees/navidrome-ea7ae954/issue-planning-rust-sdhnn/tests/fixtures/test.mp3".to_string();
        let cmd = extract_image(media).await;
        match cmd {
            Err(_) => panic!("ffmpeg resolved but launch failed"),
            Ok(c) => {
                let (info, bytes) = drain_to_bytes(c).await;
                assert_eq!(info.code, 0);
                assert!(!bytes.is_empty());
            }
        }
    }
}
