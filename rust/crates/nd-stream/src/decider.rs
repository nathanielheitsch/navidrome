//! Port of `core/stream/decider.go` (459 LOC): the transcode decision core —
//! `MakeDecision`, `buildSourceStream`, `checkDirectPlayProfile`,
//! `computeTranscodedStream`, `computeBitrate`, `applyCodecLimitations`, plus
//! the format/bitrate/command lookup tables.
//!
//! Divergences from Go (frozen in `lib.rs`):
//!   - Go's `ds.Transcoding().FindByFormat` DB lookup becomes the decider's
//!     read-only `transcoding_rows` table (`decider.go:315,330`) — the decision
//!     layer stays pure and side-effect free.
//!   - Go's probe verbs (`IsProbeAvailable`/`ensureProbed`/ffprobe launch,
//!     `decider.go:48-58,431-459`) live in nd-ffmpeg (#07); this layer keeps the
//!     identical branch table but receives the probe verdict as [`ProbeInput`].
//!     Consequently `make_decision` is infallible here: Go's only error arms are
//!     the probe/JSON-marshal failures (`decider.go:55`), and Go's malformed
//!     `ProbeData` parse error is discarded at `decider.go:132` — the port
//!     discards it too (see [`parse_probe_data`]).
//!   - Go's `log.Trace/Debug/Warn/Info` calls are omitted; the branch structure
//!     they annotate is preserved.

use serde_json;

use crate::aliases::{contains_ignore_case, matches_codec, matches_container};
use crate::codec::{
    codec_fixed_output_sample_rate, codec_max_channels, codec_max_sample_rate, is_lossless_format,
    normalize_probe_codec, normalize_source_sample_rate, target_bit_depth,
};
use crate::limitations::{apply_limitation, check_limitations, AdjustResult};
use crate::types::{
    ClientInfo, DirectPlayProfile, MediaFile, Profile, StreamDetails, TranscodeDecision,
    TranscodeOptions, CODEC_PROFILE_TYPE_AUDIO, PROTOCOL_HTTP,
};
use nd_conf::consts::DEFAULT_TRANSCODINGS;
use nd_ffmpeg::probe::AudioProbeResult;

/// `fallbackBitrate` (`decider.go:18`): the bitrate used when neither the
/// transcoding table nor the built-in defaults know the format (kbps).
pub const FALLBACK_BITRATE: i64 = 256;

/// The probe verdict for [`make_decision`], mirroring Go's states at
/// `decider.go:47-58` plus `buildSourceStream:131-149`.
#[derive(Debug, Clone)]
pub enum ProbeInput {
    /// ffprobe did not run (unavailable, `SkipProbe`, or `ProbeData` already
    /// set so `ensureProbed` short-circuits): fall back to `mf.ProbeData`, then
    /// tag metadata.
    NotAvailable,
    /// ffprobe ran and produced this `ffmpeg.AudioProbeResult`
    /// (`decider.go:53,135-149`).
    Probed(AudioProbeResult),
}

/// A transcoding row (`model.Transcoding` as consumed here: `TargetFormat`,
/// `DefaultBitRate`, `Command`). Read-only mirror of the DB row whose
/// storage/marshal shape lives in nd-db's `transcoding_repo.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcoding {
    /// `model.Transcoding.TargetFormat`.
    pub target_format: String,
    /// `model.Transcoding.DefaultBitRate` — kbps.
    pub default_bit_rate: i64,
    /// `model.Transcoding.Command` — the ffmpeg command template.
    pub command: String,
}

/// `NewTranscodeDecider` (`decider.go:29-39`): the service struct. Go wires the
/// `model.DataStore` and `ffmpeg.FFmpeg` interfaces in; the port carries only
/// the read-only transcoding table the two lookup functions consult, and the
/// `FFmpeg` half becomes an explicit [`ProbeInput`] argument at the verb.
#[derive(Debug, Clone)]
pub struct TranscodeDecider {
    /// The transcoding rows `lookupDefaultBitrate`/`LookupTranscodeCommand`
    /// consult before the built-in defaults (`decider.go:315,330`).
    pub transcoding_rows: Vec<Transcoding>,
}

/// `NewTranscodeDecider` (`decider.go:29-34`): construct the decider from its
/// transcoding table.
pub fn new_transcode_decider(transcoding_rows: Vec<Transcoding>) -> TranscodeDecider {
    TranscodeDecider { transcoding_rows }
}

impl TranscodeDecider {
    /// `MakeDecision` (`decider.go:41-121`): decide direct play vs transcode
    /// and which target parameters to use.
    pub fn make_decision(
        self: &TranscodeDecider,
        mf: &MediaFile,
        client_info: ClientInfo,
        opts: TranscodeOptions,
        probe_input: ProbeInput,
    ) -> TranscodeDecision {
        let mut decision = TranscodeDecision {
            media_id: mf.id.clone(),
            source_updated_at: mf.updated_at,
            ..TranscodeDecision::default()
        };

        // `decider.go:47-58`: probe only when not skipped.
        let run_probe = !opts.skip_probe && matches!(probe_input, ProbeInput::Probed(_));
        let probe: AudioProbeResult = match probe_input {
            ProbeInput::Probed(result) => result,
            ProbeInput::NotAvailable => AudioProbeResult::default(),
        };

        // `buildSourceStream:123-153`
        decision.source_stream = build_source_stream(mf, probe, run_probe);
        let src = &decision.source_stream;

        // Global bitrate constraint first (`decider.go:69-85`).
        if client_info.max_audio_bitrate > 0 && src.bitrate > client_info.max_audio_bitrate {
            decision
                .transcode_reasons
                .push("audio bitrate not supported".to_string());
        } else {
            for profile in &client_info.direct_play_profiles {
                match self.check_direct_play_profile(src, profile, &client_info) {
                    None => {
                        decision.can_direct_play = true;
                        decision.transcode_reasons.clear();
                        break;
                    }
                    Some(reason) => decision.transcode_reasons.push(reason),
                }
            }
        }

        // Direct play wins (`decider.go:88-91`).
        if decision.can_direct_play {
            return decision;
        }

        // Try transcoding profiles in preference order (`decider.go:94-105`).
        for profile in &client_info.transcoding_profiles {
            let computed = self.compute_transcoded_stream(src, profile, &client_info);
            if computed.is_none() {
                continue;
            }
            let (ts, target_format) = computed.unwrap();
            decision.can_transcode = true;
            decision.target_format = target_format.clone();
            decision.target_bitrate = ts.bitrate;
            decision.target_channels = ts.channels;
            decision.target_sample_rate = ts.sample_rate;
            decision.target_bit_depth = ts.bit_depth;
            decision.transcode_stream = Some(ts);
            break;
        }

        // Neither path fits (`decider.go:114-118`).
        if !decision.can_direct_play && !decision.can_transcode {
            decision.error_reason = "no compatible playback profile found".to_string();
        }
        decision
    }

    /// `checkDirectPlayProfile` (`decider.go:205-236`): `None` if the profile
    /// matches (direct play OK), or the typed reason string if it does not.
    pub fn check_direct_play_profile(
        self: &TranscodeDecider,
        src: &StreamDetails,
        profile: &DirectPlayProfile,
        client_info: &ClientInfo,
    ) -> Option<String> {
        // Protocol (`decider.go:207-209`): only http for now.
        if !profile.protocols.is_empty() && !contains_ignore_case(&profile.protocols, PROTOCOL_HTTP)
        {
            return Some("protocol not supported".to_string());
        }

        // Container (`decider.go:211-214`).
        if !profile.containers.is_empty() && !matches_container(&src.container, &profile.containers)
        {
            return Some(format!(
                "container '{}' not supported by profile {}",
                src.container,
                profile.render()
            ));
        }

        // Codec (`decider.go:216-219`): also accepts the pcm<->wav bridge.
        if !profile.audio_codecs.is_empty()
            && !matches_codec(&src.codec, &profile.audio_codecs)
            && !matches_pcm_wav_bridge(src, profile)
        {
            return Some(format!(
                "audio codec '{}' not supported by profile {}",
                src.codec,
                profile.render()
            ));
        }

        // Channels (`decider.go:221-224`).
        if profile.max_audio_channels > 0 && src.channels > profile.max_audio_channels {
            return Some(format!(
                "audio channels {} not supported by profile {} (max {})",
                src.channels,
                profile.render(),
                profile.max_audio_channels
            ));
        }

        // Codec-specific limitations (`decider.go:226-233`).
        for codec_profile in &client_info.codec_profiles {
            if codec_profile
                .kind
                .eq_ignore_ascii_case(CODEC_PROFILE_TYPE_AUDIO)
                && matches_codec(&src.codec, std::slice::from_ref(&codec_profile.name))
            {
                if let Some(reason) = check_limitations(src, &codec_profile.limitations) {
                    return Some(reason);
                }
            }
        }

        None
    }

    /// `computeTranscodedStream` (`decider.go:238-309`): attempt a valid
    /// transcoded stream for the profile. Returns the stream details plus the
    /// internal transcoding format (which may differ from the response
    /// container on a codec fallback, e.g. "mp4"→"aac"), or `None` when the
    /// profile cannot produce a valid output.
    pub fn compute_transcoded_stream(
        self: &TranscodeDecider,
        src: &StreamDetails,
        profile: &Profile,
        client_info: &ClientInfo,
    ) -> Option<(StreamDetails, String)> {
        // Protocol (`decider.go:243-247`).
        if !profile.protocol.is_empty() && !profile.protocol.eq_ignore_ascii_case(PROTOCOL_HTTP) {
            return None;
        }

        // Container/target format (`decider.go:249-252`).
        let (response_container, target_format) = resolve_target_format(profile);
        if target_format.is_empty() {
            return None;
        }

        // A transcoding command must exist (`decider.go:254-258`).
        if self.lookup_transcode_command(&target_format).is_empty() {
            return None;
        }

        // Reject lossy→lossless (`decider.go:260-266`).
        let target_is_lossless = is_lossless_format(&target_format);
        if !src.is_lossless && target_is_lossless {
            return None;
        }

        // Seed the target stream (`decider.go:268-278`).
        let mut ts = StreamDetails {
            container: response_container,
            codec: profile.audio_codec.to_lowercase(),
            sample_rate: normalize_source_sample_rate(src.sample_rate, &src.codec),
            channels: src.channels,
            bit_depth: target_bit_depth(src.bit_depth, &src.codec, target_is_lossless),
            is_lossless: target_is_lossless,
            ..StreamDetails::default()
        };
        if ts.codec.is_empty() {
            ts.codec = target_format.clone();
        }

        // Codec-intrinsic rate/channel adjustments before profile limitations
        // (`decider.go:280-289`).
        let fixed_rate = codec_fixed_output_sample_rate(&ts.codec);
        if fixed_rate > 0 {
            ts.sample_rate = fixed_rate;
        }
        let max_rate = codec_max_sample_rate(&ts.codec);
        if max_rate > 0 && ts.sample_rate > max_rate {
            ts.sample_rate = max_rate;
        }
        let max_channels = codec_max_channels(&ts.codec);
        if max_channels > 0 && ts.channels > max_channels {
            ts.channels = max_channels;
        }

        // Target bitrate (`decider.go:292-294`).
        if !self.compute_bitrate(
            src,
            &target_format,
            target_is_lossless,
            client_info,
            &mut ts,
        ) {
            return None;
        }

        // Profile MaxAudioChannels against the already-clamped target
        // channels — never raise the codec hard limit (`decider.go:296-301`).
        if profile.max_audio_channels > 0 && ts.channels > profile.max_audio_channels {
            ts.channels = profile.max_audio_channels;
        }

        // Codec profile limitations on the target codec (`decider.go:303-306`).
        if !self.apply_codec_limitations(
            src.bitrate,
            &target_format,
            target_is_lossless,
            client_info,
            &mut ts,
        ) {
            return None;
        }

        Some((ts, target_format))
    }

    /// `computeBitrate` (`decider.go:371-397`): determine the target bitrate
    /// for the transcoded stream. Returns false if the profile should be
    /// rejected.
    pub fn compute_bitrate(
        self: &TranscodeDecider,
        src: &StreamDetails,
        target_format: &str,
        target_is_lossless: bool,
        client_info: &ClientInfo,
        ts: &mut StreamDetails,
    ) -> bool {
        if src.is_lossless {
            if !target_is_lossless {
                if client_info.max_transcoding_audio_bitrate > 0 {
                    ts.bitrate = client_info.max_transcoding_audio_bitrate;
                } else if client_info.max_audio_bitrate > 0 {
                    ts.bitrate = client_info.max_audio_bitrate;
                } else {
                    ts.bitrate = self.lookup_default_bitrate(target_format);
                }
            } else if client_info.max_audio_bitrate > 0
                && src.bitrate > client_info.max_audio_bitrate
            {
                // Lossless target exceeding the client cap is rejected
                // (`decider.go:382-386`).
                return false;
            }
        } else {
            ts.bitrate = src.bitrate;
        }

        // Final cap (`decider.go:392-395`).
        if client_info.max_audio_bitrate > 0
            && ts.bitrate > 0
            && ts.bitrate > client_info.max_audio_bitrate
        {
            ts.bitrate = client_info.max_audio_bitrate;
        }
        true
    }

    /// `applyCodecLimitations` (`decider.go:401-426`): apply codec profile
    /// limitations to the transcoded stream. Returns false if the profile
    /// should be rejected.
    pub fn apply_codec_limitations(
        self: &TranscodeDecider,
        source_bitrate: i64,
        _target_format: &str,
        target_is_lossless: bool,
        client_info: &ClientInfo,
        ts: &mut StreamDetails,
    ) -> bool {
        for codec_profile in &client_info.codec_profiles {
            if !codec_profile
                .kind
                .eq_ignore_ascii_case(CODEC_PROFILE_TYPE_AUDIO)
            {
                continue;
            }
            if !matches_codec(&ts.codec, std::slice::from_ref(&codec_profile.name)) {
                continue;
            }
            for lim in &codec_profile.limitations {
                let result = apply_limitation(source_bitrate, lim, ts);
                // Cannot adjust bitrate for a lossless target
                // (`decider.go:412-416`).
                if lim.name.eq_ignore_ascii_case("audioBitrate")
                    && target_is_lossless
                    && result == AdjustResult::Adjusted
                {
                    return false;
                }
                if result == AdjustResult::CannotFit {
                    return false;
                }
            }
        }
        true
    }

    /// `lookupDefaultBitrate` (`decider.go:311-324`): the default bitrate for a
    /// format — the table first (user-customized), then the built-in defaults,
    /// then [`FALLBACK_BITRATE`].
    pub fn lookup_default_bitrate(self: &TranscodeDecider, format: &str) -> i64 {
        if let Some(row) = self.find_by_format(format) {
            if row.default_bit_rate > 0 {
                return row.default_bit_rate;
            }
        }
        for dt in DEFAULT_TRANSCODINGS {
            if dt.target_format == format && dt.default_bit_rate > 0 {
                return dt.default_bit_rate;
            }
        }
        FALLBACK_BITRATE
    }

    /// `LookupTranscodeCommand` (`decider.go:329-341`): the ffmpeg command for
    /// a format — table first, then the built-in defaults; `""` when the
    /// format is unknown.
    pub fn lookup_transcode_command(self: &TranscodeDecider, format: &str) -> String {
        if let Some(row) = self.find_by_format(format) {
            if !row.command.is_empty() {
                return row.command;
            }
        }
        for dt in DEFAULT_TRANSCODINGS {
            if dt.target_format == format {
                return dt.command.to_string();
            }
        }
        String::new()
    }

    /// `model.TranscodingRepository.FindByFormat`
    /// (`model/transcoding_repository.go:38-41`, called at `decider.go:315,330`):
    /// the first row whose `target_format` matches.
    #[must_use]
    pub fn find_by_format(self: &TranscodeDecider, format: &str) -> Option<Transcoding> {
        self.transcoding_rows
            .iter()
            .find(|row| row.target_format == format)
            .cloned()
    }
}

/// `buildSourceStream` (`decider.go:123-153`): the source stream details,
/// preferring probe data for the authoritative values.
pub fn build_source_stream(
    mf: &MediaFile,
    probe: AudioProbeResult,
    run_probe: bool,
) -> StreamDetails {
    // `decider.go:124-128`
    let mut sd = StreamDetails {
        container: mf.suffix.clone(),
        duration: mf.duration,
        size: mf.size,
        ..StreamDetails::default()
    };

    // `decider.go:131-133`: use the pre-parsed probe result, else parse the
    // stored probe data.
    let probe_used: AudioProbeResult = if run_probe {
        probe
    } else {
        parse_probe_data(&mf.probe_data).unwrap_or_default()
    };

    // `decider.go:136-149`
    let has_probe = run_probe || !mf.probe_data.is_empty();
    if has_probe {
        sd.codec = normalize_probe_codec(&probe_used.codec);
        sd.profile = probe_used.profile;
        sd.bitrate = probe_used.bit_rate;
        sd.sample_rate = probe_used.sample_rate;
        sd.bit_depth = probe_used.bit_depth;
        sd.channels = probe_used.channels;
    } else {
        sd.codec = mf.audio_codec();
        sd.bitrate = mf.bit_rate;
        sd.sample_rate = mf.sample_rate;
        sd.bit_depth = mf.bit_depth.unwrap_or(0);
        sd.channels = mf.channels;
    }
    sd.is_lossless = is_lossless_format(&sd.codec);
    sd
}

/// `parseProbeData` (`decider.go:181-190`): parse stored ffprobe JSON. Go
/// returns `(nil, nil)` for empty data and `(nil, err)` for malformed JSON;
/// the only call site (`decider.go:132`) discards the error, so the port
/// collapses both to `None`.
#[must_use]
pub fn parse_probe_data(data: &str) -> Option<AudioProbeResult> {
    if data.is_empty() {
        return None;
    }
    serde_json::from_str::<AudioProbeResult>(data).ok()
}

/// `matchesPCMWAVBridge` (`decider.go:192-201`): bridge Navidrome's internal
/// "pcm" codec name with the "wav" codec name browsers advertise for
/// `audio/wav`, scoped to WAV containers so AIFF (which also normalizes to
/// codec "pcm") cannot false-match a codec-only ["wav"] profile.
#[must_use]
pub fn matches_pcm_wav_bridge(src: &StreamDetails, profile: &DirectPlayProfile) -> bool {
    src.codec.eq_ignore_ascii_case("pcm")
        && src.container.eq_ignore_ascii_case("wav")
        && contains_ignore_case(&profile.audio_codecs, "wav")
}

/// `resolveTargetFormat` (`decider.go:346-367`): the response container and
/// internal target format from a profile's Container/AudioCodec — the codec is
/// preferred when provided (container "mp4" + codec "aac" → format "aac").
pub fn resolve_target_format(profile: &Profile) -> (String, String) {
    let response_container = profile.container.to_lowercase();
    let target_format = profile.audio_codec.to_lowercase();
    let mut out_container = response_container.clone();
    let mut out_format = response_container.clone();
    if !target_format.is_empty() {
        out_format = target_format;
    }
    if out_format.is_empty() {
        return (String::new(), String::new());
    }
    if out_container.is_empty() {
        out_container = out_format.clone();
    }
    (out_container, out_format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe_json::probe_json;
    use crate::types::*;

    #[test]
    fn resolve_target_format_prefers_codec() {
        // Arrange/Act/Assert (`decider.go:343-367`)
        let p = Profile {
            container: "mp4".into(),
            audio_codec: "aac".into(),
            protocol: PROTOCOL_HTTP.to_string(),
            max_audio_channels: 0,
        };
        assert_eq!(
            resolve_target_format(&p),
            ("mp4".to_string(), "aac".to_string())
        );
    }

    #[test]
    fn resolve_target_format_falls_back_to_container() {
        // Act/Assert
        let p = Profile {
            container: "mp3".into(),
            audio_codec: "".into(),
            protocol: PROTOCOL_HTTP.to_string(),
            max_audio_channels: 0,
        };
        assert_eq!(
            resolve_target_format(&p),
            ("mp3".to_string(), "mp3".to_string())
        );
    }

    #[test]
    fn resolve_target_format_uses_codec_when_container_absent() {
        // Act/Assert (`decider.go:361-364`)
        let p = Profile {
            container: "".into(),
            audio_codec: "Opus".into(),
            protocol: "".into(),
            max_audio_channels: 0,
        };
        assert_eq!(
            resolve_target_format(&p),
            ("opus".to_string(), "opus".to_string())
        );
    }

    #[test]
    fn resolve_target_format_empty_when_neither() {
        // Act/Assert (`decider.go:356-359`)
        let p = Profile::default();
        assert_eq!(resolve_target_format(&p), (String::new(), String::new()));
    }

    #[test]
    fn lookup_default_bitrate_prefers_table_over_defaults() {
        // Arrange (the Go oracle's mock transcoding repo table,
        // `tests/mock_transcoding_repo.go:17-32`)
        let dec = new_transcode_decider(mock_transcoding_rows());
        // Act/Assert (decider_test.go "uses default bitrate when client doesn't
        // specify": 160 from the mock table, not consts' 192)
        assert_eq!(dec.lookup_default_bitrate("mp3"), 160);
        assert_eq!(dec.lookup_default_bitrate("opus"), 96);
        assert_eq!(dec.lookup_default_bitrate("aac"), 256);
    }

    #[test]
    fn lookup_default_bitrate_falls_back_for_unknown_format() {
        // Act/Assert (decider_test.go "falls back to 256 for unknown format")
        let dec = new_transcode_decider(mock_transcoding_rows());
        assert_eq!(dec.lookup_default_bitrate("xyz"), FALLBACK_BITRATE);
    }

    #[test]
    fn lookup_default_bitrate_uses_builtin_when_table_row_has_zero() {
        // Arrange: `decider.go:315-317` requires `defaultBitRate > 0`, so a
        // table row of 0 falls through to consts.DefaultTranscodings.
        let rows = vec![Transcoding {
            target_format: "opus".into(),
            default_bit_rate: 0,
            command: "ffmpeg".into(),
        }];
        let dec = new_transcode_decider(rows);
        // Act/Assert (consts.go:189 opus default = 128)
        assert_eq!(dec.lookup_default_bitrate("opus"), 128);
    }

    #[test]
    fn lookup_transcode_command_empty_for_unknown_format() {
        // Act/Assert (decider_test.go "rejects format with no transcoding
        // command available")
        let dec = new_transcode_decider(mock_transcoding_rows());
        assert!(dec.lookup_transcode_command("wav").is_empty());
    }

    #[test]
    fn lookup_transcode_command_uses_builtin_when_table_row_command_empty() {
        // Arrange (`decider.go:331-339`)
        let rows = vec![Transcoding {
            target_format: "mp3".into(),
            default_bit_rate: 160,
            command: "".into(),
        }];
        let dec = new_transcode_decider(rows);
        // Act/Assert: consts.go:186 mp3 command
        assert_eq!(
            dec.lookup_transcode_command("mp3"),
            DEFAULT_TRANSCODINGS[0].command.to_string()
        );
    }

    #[test]
    fn matches_pcm_wav_bridge_scoped_to_wav_container() {
        // Arrange/Act/Assert (`decider.go:192-201`)
        let wav = StreamDetails {
            container: "wav".into(),
            codec: "pcm".into(),
            ..StreamDetails::default()
        };
        let profile = DirectPlayProfile {
            containers: vec!["wav".to_string()],
            audio_codecs: vec!["wav".to_string()],
            protocols: vec![PROTOCOL_HTTP.to_string()],
            max_audio_channels: 0,
        };
        assert!(matches_pcm_wav_bridge(&wav, &profile));
        let aiff = StreamDetails {
            container: "aiff".into(),
            codec: "pcm".into(),
            ..StreamDetails::default()
        };
        assert!(!matches_pcm_wav_bridge(&aiff, &profile));
    }

    #[test]
    fn build_source_stream_uses_tag_metadata_without_probe() {
        // Arrange (no probe data, `decider.go:143-149`)
        let mf = MediaFile {
            id: "1".into(),
            suffix: "flac".into(),
            codec: "FLAC".into(),
            bit_rate: 1000,
            sample_rate: 96000,
            bit_depth: Some(24),
            channels: 2,
            duration: 300.5,
            size: 50000000,
            probe_data: "".into(),
            updated_at: 0,
        };
        // Act
        let sd = build_source_stream(&mf, AudioProbeResult::default(), false);
        // Assert (decider_test.go "populates source stream correctly")
        assert_eq!(sd.container, "flac");
        assert_eq!(sd.codec, "flac");
        assert_eq!(sd.bitrate, 1000);
        assert_eq!(sd.sample_rate, 96000);
        assert_eq!(sd.bit_depth, 24);
        assert_eq!(sd.channels, 2);
        assert!(sd.is_lossless);
    }

    #[test]
    fn build_source_stream_prefers_probe_codec_normalization() {
        // Arrange (decider_test.go "uses probe codec name for lossless
        // detection": ffprobe reports "wavpack")
        let mf = MediaFile {
            id: "1".into(),
            suffix: "wv".into(),
            bit_rate: 1000,
            channels: 2,
            sample_rate: 44100,
            bit_depth: Some(16),
            probe_data: probe_json(&AudioProbeResult {
                codec: "wavpack".into(),
                profile: String::new(),
                bit_rate: 1000,
                sample_rate: 44100,
                bit_depth: 16,
                channels: 2,
            }),
            updated_at: 0,
            ..MediaFile::default()
        };
        // Act
        let sd = build_source_stream(&mf, AudioProbeResult::default(), false);
        // Assert
        assert_eq!(sd.codec, "wavpack");
        assert!(sd.is_lossless);
        assert_eq!(sd.bitrate, 1000);
    }

    fn mock_transcoding_rows() -> Vec<Transcoding> {
        // The Go oracle's mock transcoding table (`tests/mock_transcoding_repo.go`).
        let mk = |target: &str, bitrate: i64, command: &str| Transcoding {
            target_format: target.to_string(),
            default_bit_rate: bitrate,
            command: command.to_string(),
        };
        let flac_cmd = "ffmpeg -ss %t -i %s -map 0:a:0 -v 0 -c:a flac -f flac -".to_string();
        vec![
            mk("mp3", 160, ""),
            mk("oga", 128, ""),
            mk("opus", 96, ""),
            mk("flac", 0, flac_cmd.as_str()),
            mk("aac", 256, ""),
        ]
    }
}
