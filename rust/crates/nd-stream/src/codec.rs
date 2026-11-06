//! Port of `core/stream/codec.go` (93 LOC): codec-name normalization, lossless
//! detection, and the per-codec sample-rate/channel/bit-depth tables.

/// `normalizeProbeCodec` (`codec.go:8-19`): maps ffprobe `codec_name` values to
/// the simplified internal codec names (matching `inferCodecFromSuffix`
/// output). Most ffprobe names match directly; this handles the exceptions.
#[must_use]
pub fn normalize_probe_codec(codec: &str) -> String {
    let c = codec.to_lowercase();
    // DSD variants: dsd_lsbf_planar, dsd_msbf_planar, dsd_lsbf, dsd_msbf
    if c.starts_with("dsd") {
        return "dsd".to_string();
    }
    // PCM variants: pcm_s16le, pcm_s24le, pcm_s32be, pcm_f32le, etc.
    if c.starts_with("pcm_") {
        return "pcm".to_string();
    }
    c
}

/// `isLosslessFormat` (`codec.go:27-33`): true if the format is a known
/// lossless audio codec/format. Detection is based on codec name only, not bit
/// depth — some lossy codecs (e.g. ADPCM) report non-zero bits_per_sample in
/// ffprobe, so bit depth alone is not a reliable signal.
///
/// Note: nd-ffmpeg has a separate `is_lossless_output_format` that covers only
/// formats ffmpeg can produce as output (a smaller set).
#[must_use]
pub fn is_lossless_format(format: &str) -> bool {
    matches!(
        format.to_lowercase().as_str(),
        "flac"
            | "alac"
            | "wav"
            | "aiff"
            | "ape"
            | "wv"
            | "wavpack"
            | "tta"
            | "tak"
            | "shn"
            | "dsd"
            | "pcm"
    )
}

/// `normalizeSourceSampleRate` (`codec.go:39-44`): adjusts the source sample
/// rate for codecs that store it differently than PCM. Currently handles DSD
/// (÷8): DSD64=2822400→352800, DSD128=5644800→705600, etc. For other codecs,
/// returns the rate unchanged.
#[must_use]
pub fn normalize_source_sample_rate(sample_rate: i64, codec: &str) -> i64 {
    if codec.eq_ignore_ascii_case("dsd") && sample_rate > 0 {
        return sample_rate / 8;
    }
    sample_rate
}

/// `targetBitDepth` (`codec.go:49-57`): the bit depth for a transcoded stream:
/// 0 for lossy targets (they have no PCM bit depth), otherwise the source
/// depth, with DSD adjusted to the 24-bit PCM that ffmpeg produces.
#[must_use]
pub fn target_bit_depth(src_bit_depth: i64, src_codec: &str, target_is_lossless: bool) -> i64 {
    if !target_is_lossless {
        return 0;
    }
    if src_codec.eq_ignore_ascii_case("dsd") && src_bit_depth == 1 {
        return 24;
    }
    src_bit_depth
}

/// `codecFixedOutputSampleRate` (`codec.go:62-68`): the mandatory output sample
/// rate for codecs that always resample regardless of input (e.g. Opus always
/// outputs 48000 Hz). Returns 0 if the codec has no fixed output rate.
#[must_use]
pub fn codec_fixed_output_sample_rate(codec: &str) -> i64 {
    match codec.to_lowercase().as_str() {
        "opus" => 48_000,
        _ => 0,
    }
}

/// `codecMaxSampleRate` (`codec.go:72-80`): the hard maximum output sample rate
/// for a codec. Returns 0 if the codec has no hard limit.
#[must_use]
pub fn codec_max_sample_rate(codec: &str) -> i64 {
    match codec.to_lowercase().as_str() {
        "mp3" => 48_000,
        "aac" => 96_000,
        _ => 0,
    }
}

/// `codecMaxChannels` (`codec.go:85-93`): the hard maximum number of audio
/// channels a codec supports. Returns 0 if the codec has no hard limit (or is
/// unknown), in which case the source/profile constraints applied upstream are
/// authoritative.
#[must_use]
pub fn codec_max_channels(codec: &str) -> i64 {
    match codec.to_lowercase().as_str() {
        "mp3" => 2,
        "opus" => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_lossless_format_true_for_known_lossless() {
        // Act/Assert (`codec_test.go:11-20`)
        assert!(is_lossless_format("flac"));
        assert!(is_lossless_format("alac"));
        assert!(is_lossless_format("pcm"));
        assert!(is_lossless_format("wav"));
        assert!(is_lossless_format("dsd"));
        assert!(is_lossless_format("ape"));
        assert!(is_lossless_format("wv"));
        assert!(is_lossless_format("wavpack")); // ffprobe codec_name for WavPack
    }

    #[test]
    fn is_lossless_format_false_for_lossy_and_unknown() {
        // Act/Assert (`codec_test.go:23-31`)
        assert!(!is_lossless_format("mp3"));
        assert!(!is_lossless_format("aac"));
        assert!(!is_lossless_format("opus"));
        assert!(!is_lossless_format("vorbis"));
        assert!(!is_lossless_format("unknown_codec"));
    }

    #[test]
    fn is_lossless_format_is_case_insensitive() {
        // Act/Assert (`codec_test.go:34-37`)
        assert!(is_lossless_format("FLAC"));
        assert!(is_lossless_format("Alac"));
    }

    #[test]
    fn normalize_probe_codec_passes_common_names_through() {
        // Act/Assert (`codec_test.go:40-48`)
        assert_eq!(normalize_probe_codec("mp3"), "mp3");
        assert_eq!(normalize_probe_codec("aac"), "aac");
        assert_eq!(normalize_probe_codec("flac"), "flac");
        assert_eq!(normalize_probe_codec("opus"), "opus");
        assert_eq!(normalize_probe_codec("vorbis"), "vorbis");
        assert_eq!(normalize_probe_codec("alac"), "alac");
        assert_eq!(normalize_probe_codec("wmav2"), "wmav2");
    }

    #[test]
    fn normalize_probe_codec_normalizes_dsd_variants() {
        // Act/Assert (`codec_test.go:51-56`)
        assert_eq!(normalize_probe_codec("dsd_lsbf_planar"), "dsd");
        assert_eq!(normalize_probe_codec("dsd_msbf_planar"), "dsd");
        assert_eq!(normalize_probe_codec("dsd_lsbf"), "dsd");
        assert_eq!(normalize_probe_codec("dsd_msbf"), "dsd");
    }

    #[test]
    fn normalize_probe_codec_normalizes_pcm_variants() {
        // Act/Assert (`codec_test.go:59-64`)
        assert_eq!(normalize_probe_codec("pcm_s16le"), "pcm");
        assert_eq!(normalize_probe_codec("pcm_s24le"), "pcm");
        assert_eq!(normalize_probe_codec("pcm_s32be"), "pcm");
        assert_eq!(normalize_probe_codec("pcm_f32le"), "pcm");
    }

    #[test]
    fn normalize_probe_codec_lowercases() {
        // Act/Assert (`codec_test.go:67-70`)
        assert_eq!(normalize_probe_codec("MP3"), "mp3");
        assert_eq!(normalize_probe_codec("AAC"), "aac");
        assert_eq!(normalize_probe_codec("DSD_LSBF_PLANAR"), "dsd");
    }

    #[test]
    fn codec_max_channels_table() {
        // Act/Assert (`codec_test.go:73-87`)
        assert_eq!(codec_max_channels("mp3"), 2);
        assert_eq!(codec_max_channels("opus"), 8);
        assert_eq!(codec_max_channels("MP3"), 2);
        assert_eq!(codec_max_channels("Opus"), 8);
        assert_eq!(codec_max_channels("aac"), 0);
        assert_eq!(codec_max_channels("flac"), 0);
        assert_eq!(codec_max_channels("vorbis"), 0);
        assert_eq!(codec_max_channels(""), 0);
    }
}
