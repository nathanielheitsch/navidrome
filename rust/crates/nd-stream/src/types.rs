//! Port of `core/stream/types.go` (199 LOC) plus the two sentinel errors —
//! the shared value types of the transcode decision layer (#14).
//!
//! Go's `TranscodeOptions.SkipProbe` and `Request`/`Details` carry over 1:1;
//! Go's `time.Time` fields become epoch seconds (`i64`) per the crate-wide
//! divergence note in `lib.rs`.

use thiserror::Error;

/// `ErrTokenInvalid` (`types.go:10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid or expired transcode token")]
pub struct ErrTokenInvalid;

/// `ErrTokenStale` (`types.go:11`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("transcode token is stale: media file has changed")]
pub struct ErrTokenStale;

/// `TranscodeOptions` (`types.go:15-19`): controls optional behavior of
/// [`crate::decider::make_decision`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscodeOptions {
    /// `SkipProbe` (`types.go:16-18`): prevents `make_decision` from running
    /// ffprobe on the media file; when true, source stream details are derived
    /// from tag metadata only.
    pub skip_probe: bool,
}

/// `Request` (`types.go:22-29`): the resolved parameters for creating a media
/// stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// `Format` (`types.go:23`).
    pub format: String,
    /// `BitRate` (`types.go:24`) — kbps.
    pub bit_rate: i64,
    /// `SampleRate` (`types.go:25`).
    pub sample_rate: i64,
    /// `BitDepth` (`types.go:26`).
    pub bit_depth: i64,
    /// `Channels` (`types.go:27`).
    pub channels: i64,
    /// `Offset` (`types.go:28`) — seconds.
    pub offset: i64,
}

/// `ClientInfo` (`types.go:33-41`): client playback capabilities. All bitrate
/// values are in kilobits per second (kbps).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClientInfo {
    /// `Name` (`types.go:34`).
    pub name: String,
    /// `Platform` (`types.go:35`).
    pub platform: String,
    /// `MaxAudioBitrate` (`types.go:36`) — kbps; 0 = unlimited.
    pub max_audio_bitrate: i64,
    /// `MaxTranscodingAudioBitrate` (`types.go:37`) — kbps; 0 = unlimited.
    pub max_transcoding_audio_bitrate: i64,
    /// `DirectPlayProfiles` (`types.go:38`).
    pub direct_play_profiles: Vec<DirectPlayProfile>,
    /// `TranscodingProfiles` (`types.go:39`).
    pub transcoding_profiles: Vec<Profile>,
    /// `CodecProfiles` (`types.go:40`).
    pub codec_profiles: Vec<CodecProfile>,
}

/// `DirectPlayProfile` (`types.go:98-103`): a format the client can play
/// directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectPlayProfile {
    /// `Containers` (`types.go:99`).
    pub containers: Vec<String>,
    /// `AudioCodecs` (`types.go:100`).
    pub audio_codecs: Vec<String>,
    /// `Protocols` (`types.go:101`).
    pub protocols: Vec<String>,
    /// `MaxAudioChannels` (`types.go:102`); 0 = unlimited.
    pub max_audio_channels: i64,
}

impl DirectPlayProfile {
    /// `DirectPlayProfile.String` (`types.go:105-115`): `"[containers/codecs]"`
    /// with `*` for an empty container list and no codec segment when codecs
    /// are empty.
    #[must_use]
    pub fn render(&self) -> String {
        let containers = if self.containers.is_empty() {
            "*".to_string()
        } else {
            self.containers.join(",")
        };
        if self.audio_codecs.is_empty() {
            let mut out = String::from("[");
            out.push_str(&containers);
            out.push(']');
            out
        } else {
            let mut out = String::from("[");
            out.push_str(&containers);
            out.push('/');
            out.push_str(&self.audio_codecs.join(","));
            out.push(']');
            out
        }
    }
}

/// `Profile` (`types.go:118-123`): a transcoding target the client supports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// `Container` (`types.go:119`).
    pub container: String,
    /// `AudioCodec` (`types.go:120`).
    pub audio_codec: String,
    /// `Protocol` (`types.go:121`).
    pub protocol: String,
    /// `MaxAudioChannels` (`types.go:122`); 0 = unlimited.
    pub max_audio_channels: i64,
}

/// `CodecProfile` (`types.go:126-130`): codec-specific limitations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodecProfile {
    /// `Type` (`types.go:127`).
    pub kind: String,
    /// `Name` (`types.go:128`).
    pub name: String,
    /// `Limitations` (`types.go:129`).
    pub limitations: Vec<Limitation>,
}

/// `Limitation` (`types.go:133-138`): a specific codec limitation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limitation {
    /// `Name` (`types.go:134`).
    pub name: String,
    /// `Comparison` (`types.go:135`).
    pub comparison: String,
    /// `Values` (`types.go:136`).
    pub values: Vec<String>,
    /// `Required` (`types.go:137`).
    pub required: bool,
}

/// `ProtocolHTTP` (`types.go:142`, OpenSubsonic spec enum).
pub const PROTOCOL_HTTP: &str = "http";
/// `ProtocolHLS` (`types.go:143`, OpenSubsonic spec enum).
pub const PROTOCOL_HLS: &str = "hls";

/// `ComparisonEquals` (`types.go:148`, OpenSubsonic spec enum).
pub const COMPARISON_EQUALS: &str = "Equals";
/// `ComparisonNotEquals` (`types.go:149`, OpenSubsonic spec enum).
pub const COMPARISON_NOT_EQUALS: &str = "NotEquals";
/// `ComparisonLessThanEqual` (`types.go:150`, OpenSubsonic spec enum).
pub const COMPARISON_LESS_THAN_EQUAL: &str = "LessThanEqual";
/// `ComparisonGreaterThanEqual` (`types.go:151`, OpenSubsonic spec enum).
pub const COMPARISON_GREATER_THAN_EQUAL: &str = "GreaterThanEqual";

/// `LimitationAudioChannels` (`types.go:156`, OpenSubsonic spec enum).
pub const LIMITATION_AUDIO_CHANNELS: &str = "audioChannels";
/// `LimitationAudioBitrate` (`types.go:157`, OpenSubsonic spec enum).
pub const LIMITATION_AUDIO_BITRATE: &str = "audioBitrate";
/// `LimitationAudioProfile` (`types.go:158`, OpenSubsonic spec enum).
pub const LIMITATION_AUDIO_PROFILE: &str = "audioProfile";
/// `LimitationAudioSamplerate` (`types.go:159`, OpenSubsonic spec enum).
pub const LIMITATION_AUDIO_SAMPLERATE: &str = "audioSamplerate";
/// `LimitationAudioBitdepth` (`types.go:160`, OpenSubsonic spec enum).
pub const LIMITATION_AUDIO_BITDEPTH: &str = "audioBitdepth";

/// `CodecProfileTypeAudio` (`types.go:165`, OpenSubsonic spec enum).
pub const CODEC_PROFILE_TYPE_AUDIO: &str = "AudioCodec";

/// `TranscodeDecision` (`types.go:170-184`): the internal decision result.
/// All bitrate values are in kilobits per second (kbps).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscodeDecision {
    /// `MediaID` (`types.go:171`).
    pub media_id: String,
    /// `CanDirectPlay` (`types.go:172`).
    pub can_direct_play: bool,
    /// `CanTranscode` (`types.go:173`).
    pub can_transcode: bool,
    /// `TranscodeReasons` (`types.go:174`).
    pub transcode_reasons: Vec<String>,
    /// `ErrorReason` (`types.go:175`).
    pub error_reason: String,
    /// `TargetFormat` (`types.go:176`).
    pub target_format: String,
    /// `TargetBitrate` (`types.go:177`) — kbps.
    pub target_bitrate: i64,
    /// `TargetChannels` (`types.go:178`).
    pub target_channels: i64,
    /// `TargetSampleRate` (`types.go:179`).
    pub target_sample_rate: i64,
    /// `TargetBitDepth` (`types.go:180`).
    pub target_bit_depth: i64,
    /// `SourceStream` (`types.go:181`).
    pub source_stream: StreamDetails,
    /// `SourceUpdatedAt` (`types.go:182`) — epoch seconds (Go `time.Time`,
    /// truncated to seconds at the token boundary).
    pub source_updated_at: i64,
    /// `TranscodeStream` (`types.go:183`) — `None` mirrors Go's nil pointer.
    pub transcode_stream: Option<StreamDetails>,
}

/// `Details` (`types.go:188-199`): audio stream properties. Bitrate is in
/// kilobits per second (kbps).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamDetails {
    /// `Container` (`types.go:189`).
    pub container: String,
    /// `Codec` (`types.go:190`).
    pub codec: String,
    /// `Profile` (`types.go:191`) — audio profile (e.g. "LC", "HE-AACv2"),
    /// populated from ffprobe data.
    pub profile: String,
    /// `Bitrate` (`types.go:192`) — kbps.
    pub bitrate: i64,
    /// `SampleRate` (`types.go:193`).
    pub sample_rate: i64,
    /// `BitDepth` (`types.go:194`).
    pub bit_depth: i64,
    /// `Channels` (`types.go:195`).
    pub channels: i64,
    /// `Duration` (`types.go:196`) — seconds (Go `float32` flattened to
    /// `f64`).
    pub duration: f64,
    /// `Size` (`types.go:197`) — bytes.
    pub size: i64,
    /// `IsLossless` (`types.go:198`).
    pub is_lossless: bool,
}

/// `ClientInfo.CapBitrate` (`types.go:46-60`): lowers the client's declared
/// audio bitrate limits to `max_kbps`, never raising them. A zero limit means
/// "unlimited" and is set to `max_kbps`. Returns true if anything changed.
/// No-op when `max_kbps <= 0`.
pub fn cap_bitrate(ci: &mut ClientInfo, max_kbps: i64) -> bool {
    if max_kbps <= 0 {
        return false;
    }
    let mut changed = false;
    if ci.max_audio_bitrate == 0 || max_kbps < ci.max_audio_bitrate {
        ci.max_audio_bitrate = max_kbps;
        changed = true;
    }
    if ci.max_transcoding_audio_bitrate == 0 || max_kbps < ci.max_transcoding_audio_bitrate {
        ci.max_transcoding_audio_bitrate = max_kbps;
        changed = true;
    }
    changed
}

/// `DecisionOptions` — the port's stand-in for Go's ambient
/// `request.PlayerFrom`/`request.TranscodingFrom` context values
/// (`decider.go:159`, `legacy_client.go:71,107`): explicit, `None` = absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionOptions {
    /// Go's `request.TranscodingFrom(ctx)` (`legacy_client.go:107`): the
    /// server-side forced transcoding (target format + default bitrate).
    pub transcoding: Option<ServerTranscoding>,
    /// Go's `request.PlayerFrom(ctx)` (`legacy_client.go:71,109`): the player
    /// row's `MaxBitRate` (0 = unset).
    pub player_max_bit_rate: i64,
}

/// The minimal shape of `model.Transcoding` the decision layer consumes
/// (`decider.go:157-158`, `legacy_client.go:107`): read-only mirror, no DB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerTranscoding {
    /// `model.Transcoding.TargetFormat`.
    pub target_format: String,
    /// `model.Transcoding.DefaultBitRate` — kbps.
    pub default_bit_rate: i64,
}

/// The minimal shape of `model.MediaFile` the decision layer consumes
/// (`decider.go:43-44,125-149`): the scanned file fields `buildSourceStream`
/// reads. Read-only mirror, no DB.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaFile {
    /// `model.MediaFile.ID`.
    pub id: String,
    /// `model.MediaFile.Suffix` — the container/suffix ("mp3", "flac", ...).
    pub suffix: String,
    /// `model.MediaFile.Codec` — the stored codec from scanning ("" = infer).
    pub codec: String,
    /// `model.MediaFile.BitRate` — kbps.
    pub bit_rate: i64,
    /// `model.MediaFile.SampleRate` — Hz.
    pub sample_rate: i64,
    /// `model.MediaFile.BitDepth` — Go's `*int`; `None` mirrors nil.
    pub bit_depth: Option<i64>,
    /// `model.MediaFile.Channels`.
    pub channels: i64,
    /// `model.MediaFile.Duration` — seconds.
    pub duration: f64,
    /// `model.MediaFile.Size` — bytes.
    pub size: i64,
    /// `model.MediaFile.ProbeData` — the stored ffprobe JSON ("" = none).
    pub probe_data: String,
    /// `model.MediaFile.UpdatedAt` — epoch seconds (Go `time.Time`).
    pub updated_at: i64,
}

impl MediaFile {
    /// `model.MediaFile.AudioCodec` (`model/mediafile.go:261-268`): the stored
    /// codec lowercased, or the suffix/bitdepth inference below.
    #[must_use]
    pub fn audio_codec(&self) -> String {
        if !self.codec.is_empty() {
            return self.codec.to_lowercase();
        }
        self.infer_codec_from_suffix()
    }

    /// `model.MediaFile.inferCodecFromSuffix` (`model/mediafile.go:270-312`):
    /// infer the codec from the file extension when the `Codec` field is empty.
    #[must_use]
    pub fn infer_codec_from_suffix(&self) -> String {
        let suffix = self.suffix.to_lowercase();
        let bit_depth = self.bit_depth.unwrap_or(0);
        match suffix.as_str() {
            "mp3" | "mpga" => "mp3".to_string(),
            "mp2" => "mp2".to_string(),
            "ogg" | "oga" => "vorbis".to_string(),
            "opus" => "opus".to_string(),
            "mpc" => "mpc".to_string(),
            "wma" => "wma".to_string(),
            "flac" => "flac".to_string(),
            "wav" => "pcm".to_string(),
            "aif" | "aiff" | "aifc" => "pcm".to_string(),
            "ape" => "ape".to_string(),
            "wv" | "wvp" => "wv".to_string(),
            "tta" => "tta".to_string(),
            "tak" => "tak".to_string(),
            "shn" => "shn".to_string(),
            "dsf" | "dff" => "dsd".to_string(),
            "m4a" => {
                // AAC if BitDepth==0, ALAC if BitDepth>0
                if bit_depth > 0 {
                    "alac".to_string()
                } else {
                    "aac".to_string()
                }
            }
            "m4b" | "m4p" | "m4r" => "aac".to_string(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_bitrate_is_noop_when_max_kbps_is_zero() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 320,
            max_transcoding_audio_bitrate: 320,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, 0);
        // Assert
        assert!(!changed);
        assert_eq!(ci.max_audio_bitrate, 320);
        assert_eq!(ci.max_transcoding_audio_bitrate, 320);
    }

    #[test]
    fn cap_bitrate_is_noop_when_negative() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 320,
            max_transcoding_audio_bitrate: 320,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, -1);
        // Assert
        assert!(!changed);
        assert_eq!(ci.max_audio_bitrate, 320);
    }

    #[test]
    fn cap_bitrate_sets_both_limits_when_unlimited() {
        // Arrange
        let mut ci = ClientInfo::default();
        // Act
        let changed = cap_bitrate(&mut ci, 256);
        // Assert
        assert!(changed);
        assert_eq!(ci.max_audio_bitrate, 256);
        assert_eq!(ci.max_transcoding_audio_bitrate, 256);
    }

    #[test]
    fn cap_bitrate_lowers_limits_higher_than_max() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 320,
            max_transcoding_audio_bitrate: 500,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, 192);
        // Assert
        assert!(changed);
        assert_eq!(ci.max_audio_bitrate, 192);
        assert_eq!(ci.max_transcoding_audio_bitrate, 192);
    }

    #[test]
    fn cap_bitrate_does_not_raise_lower_limits() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 128,
            max_transcoding_audio_bitrate: 96,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, 320);
        // Assert
        assert!(!changed);
        assert_eq!(ci.max_audio_bitrate, 128);
        assert_eq!(ci.max_transcoding_audio_bitrate, 96);
    }

    #[test]
    fn cap_bitrate_reports_changed_when_only_one_lowered() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 320,
            max_transcoding_audio_bitrate: 128,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, 192);
        // Assert
        assert!(changed);
        assert_eq!(ci.max_audio_bitrate, 192);
        assert_eq!(ci.max_transcoding_audio_bitrate, 128);
    }

    #[test]
    fn cap_bitrate_caps_only_the_zero_limit() {
        // Arrange
        let mut ci = ClientInfo {
            max_audio_bitrate: 128,
            max_transcoding_audio_bitrate: 0,
            ..ClientInfo::default()
        };
        // Act
        let changed = cap_bitrate(&mut ci, 192);
        // Assert
        assert!(changed);
        assert_eq!(ci.max_audio_bitrate, 128);
        assert_eq!(ci.max_transcoding_audio_bitrate, 192);
    }

    #[test]
    fn direct_play_profile_render_matches_go_string() {
        // Arrange/Act/Assert: the `String()` shape (`types.go:105-115`).
        let p = DirectPlayProfile {
            containers: vec!["m4a".into(), "mp4".into()],
            audio_codecs: vec!["aac".into()],
            ..DirectPlayProfile::default()
        };
        assert_eq!(p.render(), "[m4a,mp4/aac]");
        let containers_only = DirectPlayProfile {
            containers: vec!["flac".into()],
            ..DirectPlayProfile::default()
        };
        assert_eq!(containers_only.render(), "[flac]");
        let empty = DirectPlayProfile::default();
        assert_eq!(empty.render(), "[*]");
    }

    #[test]
    fn audio_codec_infers_from_suffix_and_bit_depth() {
        // Arrange
        let alac = MediaFile {
            suffix: "m4a".into(),
            bit_depth: Some(24),
            ..MediaFile::default()
        };
        let aac = MediaFile {
            suffix: "m4a".into(),
            ..MediaFile::default()
        };
        // Act/Assert (`model/mediafile.go:270-312` branch table)
        assert_eq!(alac.audio_codec(), "alac");
        assert_eq!(aac.audio_codec(), "aac");
        let stored = MediaFile {
            codec: "MP3".into(),
            ..MediaFile::default()
        };
        assert_eq!(stored.audio_codec(), "mp3");
        let wav = MediaFile {
            suffix: "wav".into(),
            ..MediaFile::default()
        };
        assert_eq!(wav.audio_codec(), "pcm");
    }
}
