//! `core/ffmpeg/ffmpeg.go` argument construction — byte-exact port of
//! `fixCmd`/`createFFmpegCommand`/`isDefaultCommand`/`buildDynamicArgs`/
//! `buildTemplateArgs`/`injectDynamicAudioFlags`/`injectBeforeOutput` and the
//! codec/output-format tables. Go PascalCase names cited per symbol.
//!
//! Divergence: Go reads the resolved ffmpeg binary from the package-global
//! `ffmpegCmd()`; the port threads the resolved `cmd_path` explicitly into
//! every builder so resolution lives once in `ffmpeg_cmd.rs`.

use nd_conf::consts::DEFAULT_TRANSCODINGS;

/// `formatCodecMap` (`ffmpeg.go:421-427`): target format → ffmpeg codec flag.
static FORMAT_CODEC_MAP: &[(&str, &str)] = &[
    ("mp3", "libmp3lame"),
    ("opus", "libopus"),
    ("aac", "aac"),
    ("flac", "flac"),
];

/// `formatOutputMap` (`ffmpeg.go:429-435`): target format → ffmpeg output
/// format flag (-f). Note aac outputs adts (raw ADTS stream, not mp4).
static FORMAT_OUTPUT_MAP: &[(&str, &str)] = &[
    ("mp3", "mp3"),
    ("opus", "opus"),
    ("aac", "adts"),
    ("flac", "flac"),
];

/// `isLosslessOutputFormat` table (`ffmpeg.go:526-533`).
static LOSSLESS_OUTPUTS: &[&str] = &["flac", "alac", "wav", "aiff"];

/// Go map lookup with an ok-miss.
pub fn lookup(fmt: &str, table: &[(&str, &str)]) -> Option<String> {
    for (k, v) in table {
        if *k == fmt {
            return Some(v.to_string());
        }
    }

    None
}

/// Go `strings.Fields`: whitespace-run split (extra spaces collapse).
pub fn split_fields(s: &str) -> Vec<String> {
    s.split(" ")
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// Replace only the first occurrence (Go `strings.Replace(s, from, to, 1)`).
pub fn replace_first(s: String, from: &str, to: &str) -> String {
    match s.find(from) {
        None => s,
        Some(at) => s[..at].to_string() + to + &s[(at + from.len())..],
    }
}

/// Go `filepath.Dir` separator position (last '/'), -1 when absent.
pub fn last_sep(s: &str) -> i64 {
    let mut at: i64 = -1;
    for i in 0..s.len() {
        if s[i..(i + 1)] == *"/" {
            at = i as i64;
        }
    }

    at
}

/// `TranscodeOptions` (`ffmpeg.go:26-36`): all parameters for a transcoding
/// operation. Go's `int` fields are fixed to i64 (the workspace integer type);
/// `duration` keeps Go's float32.
pub struct TranscodeOptions {
    /// Custom ffmpeg command template (`Command`).
    pub command: String,
    /// Target format identifier (`Format`).
    pub format: String,
    /// Source media path (`FilePath`).
    pub file_path: String,
    /// Max bitrate, kbps; 0 = unset (`BitRate`).
    pub bit_rate: i64,
    /// Target sample rate, Hz; 0 = keep source (`SampleRate`).
    pub sample_rate: i64,
    /// Target channel count; 0 = keep source (`Channels`).
    pub channels: i64,
    /// Target bit depth; 0 = unset (`BitDepth`).
    pub bit_depth: i64,
    /// Playback offset in seconds (`Offset`).
    pub offset: i64,
    /// Remaining duration in seconds (`Duration`), used by the FLAC patcher.
    pub duration: f32,
}

/// `bitDepthToSampleFmt` (`ffmpeg.go:536-544`).
pub fn bit_depth_to_sample_fmt(bit_depth: i64) -> String {
    match bit_depth {
        16 => "s16".to_string(),
        _ => "s32".to_string(),
    }
}

/// `isLosslessOutputFormat` (`ffmpeg.go:526-533`).
pub fn is_lossless_output_format(format: &str) -> bool {
    LOSSLESS_OUTPUTS
        .iter()
        .any(|name| *name == format.to_lowercase())
}

/// `injectBeforeOutput` (`ffmpeg.go:515-522`): insert flag+value before the
/// trailing "-" (stdout marker); append when there is no trailing dash.
pub fn inject_before_output(args: Vec<String>, flag: &str, value: &str) -> Vec<String> {
    if !args.is_empty() && args[args.len() - 1] == "-" {
        let mut out = args[..(args.len() - 1)].to_vec();
        out.push(flag.to_string());
        out.push(value.to_string());
        out.push("-".to_string());
        return out;
    }

    let mut out = args;
    out.push(flag.to_string());
    out.push(value.to_string());
    out
}

/// `injectDynamicAudioFlags` (`ffmpeg.go:501-513`).
pub fn inject_dynamic_audio_flags(args: Vec<String>, opts: &TranscodeOptions) -> Vec<String> {
    let mut out = args;
    if opts.sample_rate > 0 {
        let value = opts.sample_rate.to_string();
        out = inject_before_output(out, "-ar", &value);
    }
    if opts.channels > 0 {
        let value = opts.channels.to_string();
        out = inject_before_output(out, "-ac", &value);
    }
    if opts.bit_depth >= 16 && is_lossless_output_format(&opts.format) {
        let value = bit_depth_to_sample_fmt(opts.bit_depth);
        out = inject_before_output(out, "-sample_fmt", &value);
    }

    out
}

/// `ffprobePath` (`ffmpeg.go:411-416`): derive the ffprobe binary path from the
/// resolved ffmpeg path — `filepath.Join(dir, base.replace("ffmpeg",
/// "ffprobe", 1))`. Go's `filepath.Join` drops empty components, so a bare
/// base yields the bare ffprobe name (never a doubled '/').
pub fn ffprobe_path(ffmpeg_cmd: &str) -> String {
    let sep = last_sep(ffmpeg_cmd);
    let base = ffmpeg_cmd[((sep + 1) as usize)..].to_string();
    let nb = replace_first(base, "ffmpeg", "ffprobe");
    if sep < 0 {
        return nb;
    }

    let dir = ffmpeg_cmd[..(sep as usize)].to_string();
    dir + "/" + &nb
}

/// `fixCmd` (`ffmpeg.go:379-392`): split the command template and substitute
/// the resolved ffmpeg/ffprobe binary paths for the bare tool tokens.
pub fn fix_cmd(cmd: &str, cmd_path: &str) -> Vec<String> {
    let probe = ffprobe_path(cmd_path);
    let out = split_fields(cmd)
        .iter()
        .map(|s| {
            if *s == "ffmpeg" || *s == "ffmpeg.exe" {
                cmd_path.to_string()
            } else if *s == "ffprobe" || *s == "ffprobe.exe" {
                probe.clone()
            } else {
                s.clone()
            }
        })
        .collect();
    out
}

/// Go's backwards scan for the LAST existing "-i" token (`slices.Backward`).
pub fn last_input_index(args: &[String]) -> usize {
    let mut at = args.len();
    let mut i = args.len();
    while i > 0 {
        i -= 1;
        if args[i] == "-i" {
            at = i;
            break;
        }
    }

    at
}

/// `createFFmpegCommand` (`ffmpeg.go:394-410`). `cmd_path` is the resolved
/// ffmpeg binary Go reads from `ffmpegCmd()`'s package global; the port threads
/// it explicitly through `fixCmd`.
pub fn create_ffmpeg_command(
    cmd: &str,
    path: &str,
    max_bit_rate: i64,
    offset: i64,
    cmd_path: &str,
) -> Vec<String> {
    let mut args: Vec<String> = [].to_vec();
    for token in fix_cmd(cmd, cmd_path) {
        let mut s = token;
        if s.contains("%s") {
            if offset > 0 && !cmd.contains("%t") {
                // Pre-input seeking: ffmpeg seeks at the demuxer level (fast)
                // instead of decoding all frames up to the offset (slow).
                let insert_at = last_input_index(&args);
                let mut out = args[..insert_at].to_vec();
                out.push("-ss".to_string());
                out.push(offset.to_string());
                out.extend(args[insert_at..].to_vec());
                args = out;
            }
            args.push(s.replace("%s", path));
        } else {
            s = s.replace("%t", &offset.to_string());
            s = s.replace("%b", &max_bit_rate.to_string());
            args.push(s);
        }
    }

    args
}

/// `isDefaultCommand` (`ffmpeg.go:446-448`): the command must equal the known
/// default for this format. Go builds a `format -> command` map from
/// `consts.DefaultTranscodings`; the port scans the 4-row table (an unknown
/// format misses and yields false, matching Go's nil-map read).
pub fn is_default_command(format: &str, command: &str) -> bool {
    for row in DEFAULT_TRANSCODINGS {
        if row.target_format == format {
            return row.command == command;
        }
    }

    false
}

/// `buildDynamicArgs` (`ffmpeg.go:452-488`) — exact argument order.
pub fn build_dynamic_args(opts: TranscodeOptions, cmd_path: &str) -> Vec<String> {
    let mut args: Vec<String> = [cmd_path.to_string()].to_vec();
    if opts.offset > 0 {
        args.push("-ss".to_string());
        args.push(opts.offset.to_string());
    }
    args.push("-i".to_string());
    args.push(opts.file_path.clone());
    args.push("-map".to_string());
    args.push("0:a:0".to_string());

    // Preserve source tags: -map_metadata 0 copies format-level tags (MP3/FLAC);
    // -map_metadata 0:s:a:0 copies tags from the first audio stream (OPUS/OGG).
    args.push("-map_metadata".to_string());
    args.push("0".to_string());
    args.push("-map_metadata".to_string());
    args.push("0:s:a:0".to_string());
    match lookup(&opts.format, FORMAT_CODEC_MAP) {
        None => {}
        Some(codec) => {
            args.push("-c:a".to_string());
            args.push(codec);
        }
    }
    if opts.bit_rate > 0 {
        args.push("-b:a".to_string());
        args.push(opts.bit_rate.to_string() + "k");
    }
    args = inject_dynamic_audio_flags(args, &opts);
    args.push("-v".to_string());
    args.push("0".to_string());
    match lookup(&opts.format, FORMAT_OUTPUT_MAP) {
        None => {}
        Some(out) => {
            args.push("-f".to_string());
            args.push(out);
        }
    }
    args.push("-".to_string());

    args
}

/// `buildTemplateArgs` (`ffmpeg.go:495-499`).
pub fn build_template_args(opts: TranscodeOptions, cmd_path: &str) -> Vec<String> {
    let args = create_ffmpeg_command(
        &opts.command,
        &opts.file_path,
        opts.bit_rate,
        opts.offset,
        cmd_path,
    );
    inject_dynamic_audio_flags(args, &opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arrange-side shorthand: build a `Vec<String>` from literals.
    fn w(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Assert-side shorthand: does `args` contain this token?
    fn has(args: Vec<String>, needle: &str) -> bool {
        args.iter().any(|a| *a == needle)
    }

    /// Arrange-side shorthand: positional `TranscodeOptions` builder.
    fn opts(
        command: &str,
        format: &str,
        file_path: &str,
        bit_rate: i64,
        sample_rate: i64,
        channels: i64,
        bit_depth: i64,
    ) -> TranscodeOptions {
        TranscodeOptions {
            command: command.to_string(),
            format: format.to_string(),
            file_path: file_path.to_string(),
            bit_rate,
            sample_rate,
            channels,
            bit_depth,
            offset: 0,
            duration: 0.0,
        }
    }

    #[test]
    fn create_ffmpeg_command_basic() {
        // Arrange
        let cmd = "ffmpeg -i %s -b:a %bk mp3 -";
        // Act
        let args = create_ffmpeg_command(cmd, "/music library/file.mp3", 123, 0, "ffmpeg");
        // Assert
        assert_eq!(
            args,
            w(&[
                "ffmpeg",
                "-i",
                "/music library/file.mp3",
                "-b:a",
                "123k",
                "mp3",
                "-"
            ])
        );
    }

    #[test]
    fn create_ffmpeg_command_extra_spaces() {
        // Arrange / Act
        let args = create_ffmpeg_command(
            "ffmpeg    -i %s -b:a    %bk      mp3 -",
            "/music library/file.mp3",
            123,
            0,
            "ffmpeg",
        );
        // Assert: fields split on whitespace runs (Go strings.Fields)
        assert_eq!(
            args,
            w(&[
                "ffmpeg",
                "-i",
                "/music library/file.mp3",
                "-b:a",
                "123k",
                "mp3",
                "-"
            ])
        );
    }

    #[test]
    fn create_ffmpeg_command_offset_with_time_token() {
        // Arrange / Act
        let args = create_ffmpeg_command(
            "ffmpeg -ss %t -i %s -b:a %bk mp3 -",
            "/music library/file.mp3",
            123,
            456,
            "ffmpeg",
        );
        // Assert: %t carries the offset, no extra -ss injected
        assert_eq!(
            args,
            w(&[
                "ffmpeg",
                "-ss",
                "456",
                "-i",
                "/music library/file.mp3",
                "-b:a",
                "123k",
                "mp3",
                "-"
            ])
        );
    }

    #[test]
    fn create_ffmpeg_command_offset_without_time_token() {
        // Arrange / Act
        let args = create_ffmpeg_command(
            "ffmpeg -i %s -b:a %bk mp3 -",
            "/music library/file.mp3",
            123,
            456,
            "ffmpeg",
        );
        // Assert: -ss inserted before the LAST existing "-i"
        assert_eq!(
            args,
            w(&[
                "ffmpeg",
                "-ss",
                "456",
                "-i",
                "/music library/file.mp3",
                "-b:a",
                "123k",
                "mp3",
                "-"
            ])
        );
    }

    #[test]
    fn create_ffmpeg_command_absolute_ffmpeg_path() {
        // Arrange / Act
        let args =
            create_ffmpeg_command("ffmpeg -i %s -f mp3 -", "one.mp3", 0, 0, "/usr/bin/ffmpeg");
        // Assert
        assert_eq!(
            args,
            w(&["/usr/bin/ffmpeg", "-i", "one.mp3", "-f", "mp3", "-"])
        );
    }

    #[test]
    fn create_ffmpeg_command_path_with_spaces() {
        // Arrange / Act
        let args = create_ffmpeg_command(
            "ffmpeg -i %s -f mp3 -",
            "one.mp3",
            0,
            0,
            "/usr/bin/with spaces/ffmpeg.exe",
        );
        // Assert: the resolved path stays ONE argument
        assert_eq!(
            args,
            w(&[
                "/usr/bin/with spaces/ffmpeg.exe",
                "-i",
                "one.mp3",
                "-f",
                "mp3",
                "-"
            ])
        );
    }

    #[test]
    fn ffprobe_path_derivations() {
        // Act / Assert
        assert_eq!(ffprobe_path("/usr/bin/ffmpeg"), "/usr/bin/ffprobe");
        assert_eq!(
            ffprobe_path("/usr/bin/with spaces/ffmpeg.exe"),
            "/usr/bin/with spaces/ffprobe.exe"
        );
        assert_eq!(ffprobe_path("ffmpeg"), "ffprobe");
    }

    #[test]
    fn fix_cmd_substitutes_tokens() {
        // Arrange / Act
        let args = fix_cmd("ffprobe -v error -i %s", "/x/ffmpeg");
        // Assert
        assert_eq!(args, w(&["/x/ffprobe", "-v", "error", "-i", "%s"]));
    }

    #[test]
    fn is_default_command_table() {
        // Arrange: the nd-conf defaults
        let mp3 = DEFAULT_TRANSCODINGS[0].command;
        let opus = DEFAULT_TRANSCODINGS[1].command;
        let aac = DEFAULT_TRANSCODINGS[2].command;
        let flac = DEFAULT_TRANSCODINGS[3].command;
        // Act / Assert
        assert!(is_default_command("mp3", mp3));
        assert!(is_default_command("opus", opus));
        assert!(is_default_command("aac", aac));
        assert!(is_default_command("flac", flac));
        assert!(!is_default_command(
            "mp3",
            "ffmpeg -i %s -b:a %bk -custom-flag -f mp3 -"
        ));
        assert!(!is_default_command("wav", "ffmpeg -i %s -f wav -"));
    }

    #[test]
    fn build_dynamic_args_mp3_full() {
        // Arrange
        let o = opts("", "mp3", "/music/file.flac", 256, 48000, 2, 0);
        // Act / Assert
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "256k",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-v",
                "0",
                "-f",
                "mp3",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_flac_no_bitrate() {
        // Arrange
        let o = opts("", "flac", "/music/file.dsf", 0, 48000, 0, 0);
        // Act / Assert: no -b:a
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.dsf",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "flac",
                "-ar",
                "48000",
                "-v",
                "0",
                "-f",
                "flac",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_opus_bitrate_only() {
        // Arrange
        let o = opts("", "opus", "/music/file.flac", 128, 0, 0, 0);
        // Act / Assert
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "libopus",
                "-b:a",
                "128k",
                "-v",
                "0",
                "-f",
                "opus",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_offset() {
        // Arrange
        let o = {
            let mut v = opts("", "mp3", "/music/file.mp3", 192, 0, 0, 0);
            v.offset = 30;
            v
        };
        // Act / Assert: -ss 30 immediately after the binary
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-ss",
                "30",
                "-i",
                "/music/file.mp3",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "192k",
                "-v",
                "0",
                "-f",
                "mp3",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_aac_adts() {
        // Arrange
        let o = opts("", "aac", "/music/file.flac", 256, 0, 0, 0);
        // Act / Assert: -c:a aac but -f adts
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "aac",
                "-b:a",
                "256k",
                "-v",
                "0",
                "-f",
                "adts",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_flac_24bit_s32() {
        // Arrange
        let o = opts("", "flac", "/music/file.dsf", 0, 0, 0, 24);
        // Act / Assert
        assert_eq!(
            build_dynamic_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.dsf",
                "-map",
                "0:a:0",
                "-map_metadata",
                "0",
                "-map_metadata",
                "0:s:a:0",
                "-c:a",
                "flac",
                "-sample_fmt",
                "s32",
                "-v",
                "0",
                "-f",
                "flac",
                "-",
            ])
        );
    }

    #[test]
    fn build_dynamic_args_no_sample_fmt_when_depth_zero() {
        // Arrange
        let o = opts("", "flac", "/music/file.flac", 0, 0, 0, 0);
        // Act
        let args = build_dynamic_args(o, "ffmpeg");
        // Assert
        assert!(!has(args, "-sample_fmt"));
    }

    #[test]
    fn build_dynamic_args_no_sample_fmt_dsd_depth() {
        // Arrange
        let o = opts("", "flac", "/music/file.dsf", 0, 0, 0, 1);
        // Act / Assert
        assert!(!has(build_dynamic_args(o, "ffmpeg"), "-sample_fmt"));
    }

    #[test]
    fn build_dynamic_args_lossy_never_sample_fmt() {
        // Arrange: table (format, bit_rate)
        let table = [("mp3", 256), ("aac", 256), ("opus", 128)];
        for row in table {
            // Arrange
            let o = opts("", row.0, "/music/file.flac", row.1, 0, 0, 16);
            // Act / Assert
            assert!(!has(build_dynamic_args(o, "ffmpeg"), "-sample_fmt"));
        }
    }

    #[test]
    fn build_template_args_injects_ar_ac() {
        // Arrange
        let o = opts(
            "ffmpeg -i %s -b:a %bk -v 0 -f mp3 -",
            "mp3",
            "/music/file.flac",
            192,
            44100,
            2,
            0,
        );
        // Act / Assert
        assert_eq!(
            build_template_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-b:a",
                "192k",
                "-v",
                "0",
                "-f",
                "mp3",
                "-ar",
                "44100",
                "-ac",
                "2",
                "-",
            ])
        );
    }

    #[test]
    fn build_template_args_only_ar() {
        // Arrange
        let o = opts(
            "ffmpeg -i %s -b:a %bk -v 0 -f mp3 -",
            "mp3",
            "/music/file.flac",
            192,
            48000,
            0,
            0,
        );
        // Act / Assert
        assert_eq!(
            build_template_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-b:a",
                "192k",
                "-v",
                "0",
                "-f",
                "mp3",
                "-ar",
                "48000",
                "-",
            ])
        );
    }

    #[test]
    fn build_template_args_nothing_injected() {
        // Arrange
        let o = opts(
            "ffmpeg -i %s -b:a %bk -v 0 -f mp3 -",
            "mp3",
            "/music/file.flac",
            192,
            0,
            0,
            0,
        );
        // Act / Assert
        assert_eq!(
            build_template_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-b:a",
                "192k",
                "-v",
                "0",
                "-f",
                "mp3",
                "-",
            ])
        );
    }

    #[test]
    fn build_template_args_lossless_sample_fmt() {
        // Arrange
        let o = opts(
            "ffmpeg -i %s -v 0 -c:a flac -f flac -",
            "flac",
            "/music/file.dsf",
            0,
            0,
            0,
            24,
        );
        // Act / Assert
        assert_eq!(
            build_template_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.dsf",
                "-v",
                "0",
                "-c:a",
                "flac",
                "-f",
                "flac",
                "-sample_fmt",
                "s32",
                "-",
            ])
        );
    }

    #[test]
    fn build_template_args_lossy_no_sample_fmt() {
        // Arrange
        let o = opts(
            "ffmpeg -i %s -b:a %bk -v 0 -f mp3 -",
            "mp3",
            "/music/file.flac",
            192,
            0,
            0,
            16,
        );
        // Act / Assert
        assert_eq!(
            build_template_args(o, "ffmpeg"),
            w(&[
                "ffmpeg",
                "-i",
                "/music/file.flac",
                "-b:a",
                "192k",
                "-v",
                "0",
                "-f",
                "mp3",
                "-",
            ])
        );
    }

    #[test]
    fn bit_depth_to_sample_fmt_table() {
        // Act / Assert
        assert_eq!(bit_depth_to_sample_fmt(16), "s16");
        assert_eq!(bit_depth_to_sample_fmt(24), "s32");
        assert_eq!(bit_depth_to_sample_fmt(32), "s32");
    }

    #[test]
    fn inject_before_output_both_shapes() {
        // Arrange / Act
        let a = inject_before_output(
            w(&["ffmpeg", "-i", "file.mp3", "-f", "mp3", "-"]),
            "-ar",
            "48000",
        );
        // Assert
        assert_eq!(
            a,
            w(&["ffmpeg", "-i", "file.mp3", "-f", "mp3", "-ar", "48000", "-"])
        );
        // Arrange / Act
        let b = inject_before_output(w(&["ffmpeg", "-i", "file.mp3"]), "-ar", "48000");
        // Assert
        assert_eq!(b, w(&["ffmpeg", "-i", "file.mp3", "-ar", "48000"]));
    }

    #[test]
    fn lossless_output_format_table() {
        // Act / Assert
        assert!(is_lossless_output_format("flac"));
        assert!(is_lossless_output_format("WAV"));
        assert!(!is_lossless_output_format("mp3"));
    }
}
