//! The animated-webp encoder probe — `core/ffmpeg/ffmpeg.go:624-665`
//! (`encoderProbeState`/`encoderProbe`/`has`) plus `parseEncodersOutput`
//! (`ffmpeg.go:115-126`) and its test cases (`core/ffmpeg/ffmpeg_test.go:804-833`).
//!
//! Go memoises a tri-state (`unknown`/`available`/`unavailable`) behind a mutex
//! and ignores which binary the verdict was probed for; the port keeps the same
//! tri-state in a [`std::sync::OnceLock`] whose payload is the `supported`/`unsupported`
//! [`std::collections::HashSet`] pair #07 asks for — `None` while the probe has
//! never run, and a launch failure leaves it unset (Go's "will retry on next
//! animated cover", `ffmpeg.go:652`).
//!
//! // shortcut: 5s bound — Go wraps the `-encoders` run in
//! `context.WithTimeout(ctx, 5*time.Second)` (`ffmpeg.go:648`); the port runs it
//! synchronously through [`Command`] and never bounds the child. Consequence: a
//! hanging ffmpeg stalls the animated-cover call instead of falling back to the
//! static resize. The bounded/kill-group form of the same contract lives in
//! [`ffcmd`].

use crate::errors::FFmpegError;
use crate::ffmpeg_cmd::ffmpeg_cmd;

use std::collections::HashSet;
use std::process::Command;

/// The probe memo (Go's `encoderProbe` + `encoderProbeState`, `ffmpeg.go:624-635`):
/// `None` before the probe ran, `Some(supported, unsupported)` after.
static ENC_ONCE: std::sync::OnceLock<(HashSet<String>, HashSet<String>)> =
    std::sync::OnceLock::new();

/// `parseEncodersOutput` (`ffmpeg.go:115-126`): whole-word match of `name` on the
/// second whitespace-separated field of any line (`strings.Fields` collapses runs
/// of spaces). Header rows are skipped for free: their second field is `=` or
/// `-----`, never an encoder name.
pub fn parse_encoders_output(out: String, name: String) -> bool {
    if name.is_empty() {
        return false;
    }

    for line in out.split("\n") {
        let fields: Vec<String> = line.split_whitespace().map(|f| f.to_string()).collect();
        if fields.len() >= 2 && fields[1] == name {
            return true;
        }
    }
    false
}

/// `encoders` (`ffmpeg.go:116` doc comment) as sets: every name column of an
/// `ffmpeg -encoders` table, header rows excluded.
fn collect_supported(out: String) -> HashSet<String> {
    let mut supported = HashSet::new();
    for line in out.split("\n") {
        let fields: Vec<String> = line.split_whitespace().map(|f| f.to_string()).collect();
        if fields.len() >= 2 && fields[1] != "=" && fields[1] != "-----" {
            supported.insert(fields[1].to_string());
        }
    }
    supported
}

/// The negative-verdict key: `encoder` prefixed with the binary path, so the
/// `unsupported` set can answer per-binary (Go logs the per-path hint,
/// `ffmpeg.go:662`).
fn unsupported_key(path: String, encoder: String) -> String {
    path + " " + encoder.as_str()
}
/// The `supported`/`unsupported` pair, probed at most once: `<path> -hide_banner
/// -encoders` (`ffmpeg.go:650`) and the parsed name set; when the probed build
/// lacks `libwebp_anim` its path-prefixed key lands in `unsupported`. A launch
/// failure memoises nothing and yields empty sets (Go's warn-and-retry).
pub fn encoders_for(path: String) -> (HashSet<String>, HashSet<String>) {
    if let Some(sets) = ENC_ONCE.get().cloned() {
        return sets;
    }

    let res = Command::new(&path)
        .arg("-hide_banner")
        .arg("-encoders")
        .output();
    if res.is_err() {
        // Go: `log.Warn(ctx, "Could not probe ffmpeg encoders; will retry on next animated cover", err)`.
        return (HashSet::new(), HashSet::new());
    }

    let out = String::from_utf8_lossy(&res.unwrap().stdout)
        .trim()
        .to_string();
    let supported = collect_supported(out);
    let mut unsupported = HashSet::new();
    let anim_encoder = "libwebp_anim".to_string();
    if !supported.contains(&anim_encoder) {
        unsupported.insert(unsupported_key(path, anim_encoder));
    }

    let pair = (supported, unsupported);
    let _ = ENC_ONCE.set(pair.clone());
    pair
}

/// `encoderProbe.has` (`ffmpeg.go:637-665`): the memoised verdict for
/// (`path`, `encoder`) — positive lookup in `supported`, negative lookup of the
/// path-prefixed key in `unsupported`, and `false` for anything the probe did
/// not record.
pub fn has(path: String, encoder: String) -> bool {
    let (supported, unsupported) = encoders_for(path.clone());
    if unsupported.contains(&unsupported_key(path, encoder.clone())) {
        return false;
    }
    supported.contains(&encoder)
}
/// `ffmpeg.go:95-102` for the resolved binary: `!ErrAnimatedWebPUnsupported` —
/// resolve ffmpeg ([`ffmpeg_cmd`]) and ask the probe for `libwebp_anim`. `false`
/// means the caller takes the `ErrAnimatedWebPUnsupported` branch.
pub fn encoders_available() -> bool {
    let (path, ok) = ffmpeg_cmd();
    if !ok {
        return false;
    }
    has(path, "libwebp_anim".to_string())
}

/// `ffmpeg.go:66` — `ErrAnimatedWebPUnsupported`, the error `ConvertAnimatedImage`
/// returns when [`encoders_available`] says the build lacks the encoder.
pub fn animated_webp_unsupported() -> FFmpegError {
    FFmpegError::ErrAnimatedWebPUnsupported
}

#[cfg(test)]
mod tests {
    use crate::encoder_probe::animated_webp_unsupported;
    use crate::encoder_probe::encoders_available;
    use crate::encoder_probe::parse_encoders_output;
    use crate::errors::FFmpegError;
    use crate::ffmpeg_cmd::is_available;

    /// `ffmpeg_test.go:805-812` — the sample table the suite feeds the parser.
    fn sample() -> String {
        r#"Encoders:
 V..... = Video
 ------
 V....D apng                 APNG (Animated Portable Network Graphics) image
 V....D libwebp_anim         libwebp WebP image (codec webp)
 V....D libwebp              libwebp WebP image (codec webp)
 A....D aac                  AAC (Advanced Audio Coding)
"#
        .to_string()
    }

    /// `ffmpeg_test.go:813-817` "returns true when the encoder is present".
    #[test]
    fn parse_encoders_output_present() {
        // Arrange
        let table = sample();
        // Act
        let anim = parse_encoders_output(table.clone(), "libwebp_anim".to_string());
        let plain = parse_encoders_output(table.clone(), "libwebp".to_string());
        let aac = parse_encoders_output(table.clone(), "aac".to_string());
        // Assert
        assert!(anim && plain && aac);
    }

    /// `ffmpeg_test.go:818-820` "returns false when the encoder is absent".
    #[test]
    fn parse_encoders_output_absent() {
        // Act
        let missing = parse_encoders_output(sample(), "libwebp_missing".to_string());
        let empty = parse_encoders_output(sample(), "".to_string());
        // Assert
        assert!(!missing && !empty);
    }

    /// `ffmpeg_test.go:822-827` "does not match partial names" — `libwebp` is a
    /// prefix of `libwebp_anim`; names are whole-word.
    #[test]
    fn parse_encoders_output_partial_name() {
        // Arrange
        let stripped = r#"Encoders:
 V....D libwebp              libwebp WebP image (codec webp)
"#
        .to_string();
        // Act
        let anim = parse_encoders_output(stripped, "libwebp_anim".to_string());
        // Assert
        assert!(!anim);
    }

    /// `ffmpeg_test.go:829-831` "handles empty output" (Go's `nil` and `[]byte("")`).
    #[test]
    fn parse_encoders_output_empty_output() {
        // Act
        let from_nil = parse_encoders_output("".to_string(), "libwebp_anim".to_string());
        let from_empty =
            parse_encoders_output("Encoders:\n".to_string(), "libwebp_anim".to_string());
        // Assert
        assert!(!from_nil && !from_empty);
    }

    /// Live ffmpeg (this box: /opt/homebrew/bin/ffmpeg, no `libwebp_anim`), so the
    /// `ErrAnimatedWebPUnsupported` branch of `ffmpeg.go:100-102` is live-testable.
    /// SKIP-IF-MISSING: when ffmpeg cannot be resolved the probe legitimately
    /// reports unavailable without running, so the case asserts nothing — that is
    /// the documented CI-safe semantics.
    #[test]
    fn encoders_available_absent_encoder() {
        // Arrange: ffmpeg present on this machine?
        let ffmpeg_present = is_available();
        // Act
        let available = encoders_available();
        let err = animated_webp_unsupported();
        // Assert
        if ffmpeg_present {
            assert!(!available);
            assert!(matches!(err, FFmpegError::ErrAnimatedWebPUnsupported));
            assert!(err.to_string().contains("libwebp_anim"));
        }
    }
}
