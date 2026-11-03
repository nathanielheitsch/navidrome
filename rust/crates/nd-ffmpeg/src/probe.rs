//! ffprobe JSON DTOs and the probe-output parser — `core/ffmpeg/ffmpeg.go:231-297`
//! (`probeOutput`/`probeFormat`/`probeStream`/`parseProbeOutput`) plus the
//! `ProbeError` message formatters `probeDetail` (`ffmpeg.go:211-218`) and
//! `probeClientReason` (`ffmpeg.go:220-230`).
//!
//! Go's `json.Unmarshal` silently ignores payload keys the struct does not
//! declare, which serde does too; the mirror of that is that every declared
//! field must tolerate a *missing* key, hence `#[serde(default)]`
//! everywhere. The real ffprobe payloads in the suite carry extras Go never
//! reads (`index`, `codec_long_name`, `sample_fmt`, `tags`, `channel_layout`,
//! `width`, `height`) and they are dropped identically.

use serde::{Deserialize, Serialize};

use crate::errors::FFmpegError;

/// The ffprobe envelope (`probeOutput`, `ffmpeg.go:242-245`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeOutput {
    /// `streams` — every stream ffprobe found (audio, video, artwork).
    #[serde(default, rename = "streams")]
    pub streams: Vec<ProbeStream>,
    /// `format` — container-level facts.
    #[serde(default, rename = "format")]
    pub format: ProbeFormat,
}

/// `probeFormat` (`ffmpeg.go:247-249`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeFormat {
    /// `bit_rate` — container bit rate in bits per second, as a string.
    #[serde(default, rename = "bit_rate")]
    pub bit_rate: String,
}

/// `probeStream` (`ffmpeg.go:251-260`). `channels`/`bits_per_sample` are
/// numeric in the payload (Go's `int`), the rest strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeStream {
    /// `codec_name`.
    #[serde(default, rename = "codec_name")]
    pub codec_name: String,
    /// `codec_type` — the gate for "audio".
    #[serde(default, rename = "codec_type")]
    pub codec_type: String,
    /// `profile` — AAC profile ("LC", "HE-AACv2", ...); ffprobe sometimes says
    /// "unknown", which the parser clears.
    #[serde(default, rename = "profile")]
    pub profile: String,
    /// `sample_rate` — a string in Go's DTO, parsed to an int downstream.
    #[serde(default, rename = "sample_rate")]
    pub sample_rate: String,
    /// `bit_rate` — bits per second, string, and absent for FLAC/Opus/ALAC.
    #[serde(default, rename = "bit_rate")]
    pub bit_rate: String,
    /// `channels`.
    #[serde(default, rename = "channels")]
    pub channels: i64,
    /// `bits_per_sample` — numeric; 0 for most compressed codecs.
    #[serde(default, rename = "bits_per_sample")]
    pub bits_per_sample: i64,
    /// `bits_per_raw_sample` — where FLAC/ALAC actually report bit depth.
    #[serde(default, rename = "bits_per_raw_sample")]
    pub bits_per_raw_sample: String,
}

/// What `ProbeAudioStream` hands back (`AudioProbeResult`). Go's wire names are
/// camel-cased, so the port renames `bit_rate`→`bitRate`, `sample_rate`→`sampleRate`,
/// `bit_depth`→`bitDepth` (`profile`/`codec`/`channels` are single words and stay
/// plain).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioProbeResult {
    /// `codec` — the audio stream's codec name.
    #[serde(default, rename = "codec")]
    pub codec: String,
    /// `profile` — cleared when ffprobe reports "unknown".
    #[serde(default, rename = "profile")]
    pub profile: String,
    /// `bitRate` — kbps.
    #[serde(default, rename = "bitRate")]
    pub bit_rate: i64,
    /// `sampleRate` — Hz.
    #[serde(default, rename = "sampleRate")]
    pub sample_rate: i64,
    /// `bitDepth` — bits per sample.
    #[serde(default, rename = "bitDepth")]
    pub bit_depth: i64,
    /// `channels`.
    #[serde(default, rename = "channels")]
    pub channels: i64,
}

/// Go's `strconv.Atoi` with the error discarded, as every call site here does it:
/// junk text ("", "16.0", "N/A") yields 0.
fn atoi_lenient(s: String) -> i64 {
    s.parse::<i64>().unwrap_or(0)
}

/// `parseProbeOutput` (`ffmpeg.go:231-297`), returned as the tuple Go returns
/// (`(result, error)`): on failure the result is a default `AudioProbeResult`
/// and the second slot carries Go's error — [`FFmpegError::ProbeParse`] for a
/// JSON failure ("parsing ffprobe output: {err}") or [`FFmpegError::NoAudioStream`]
/// when no stream has `codec_type == "audio"`.
pub fn parse_probe_output(output: Vec<u8>) -> (AudioProbeResult, Option<FFmpegError>) {
    let probed: ProbeOutput = match serde_json::from_slice(&output) {
        Ok(v) => v,
        Err(err) => {
            return (
                AudioProbeResult::default(),
                Some(FFmpegError::ProbeParse(
                    "parsing ffprobe output: ".to_string() + &err.to_string(),
                )),
            )
        }
    };

    for stream in probed.streams.iter() {
        if stream.codec_type != "audio" {
            continue;
        }

        // Go: bitDepth = bits_per_sample, else the raw-sample field when the
        // first is 0 and that field is present (FLAC/ALAC report there).
        let mut bit_depth = stream.bits_per_sample;
        if bit_depth == 0 && !stream.bits_per_raw_sample.is_empty() {
            bit_depth = atoi_lenient(stream.bits_per_raw_sample.clone());
        }

        let mut result = AudioProbeResult {
            codec: stream.codec_name.clone(),
            channels: stream.channels,
            bit_depth,
            profile: "".to_string(),
            sample_rate: 0,
            bit_rate: 0,
        };

        // Profile: ffprobe's "unknown" (any casing) reads as empty.
        if !stream.profile.is_empty() && !stream.profile.eq_ignore_ascii_case("unknown") {
            result.profile = stream.profile.clone();
        }

        if !stream.sample_rate.is_empty() {
            result.sample_rate = atoi_lenient(stream.sample_rate.clone());
        }

        // Stream-level bit rate is bps; the wire value is kbps and Go's
        // integer division truncates (198314 -> 198, 55999 -> 55).
        if !stream.bit_rate.is_empty() {
            let bps = atoi_lenient(stream.bit_rate.clone());
            result.bit_rate = bps / 1000;
        }

        // FLAC, Opus and ALAC report no stream bit rate, so fall back to the
        // container's — but never over a stream value that was set.
        if result.bit_rate == 0 && !probed.format.bit_rate.is_empty() {
            let bps = atoi_lenient(probed.format.bit_rate);
            result.bit_rate = bps / 1000;
        }

        return (result, None);
    }

    (
        AudioProbeResult::default(),
        Some(FFmpegError::NoAudioStream),
    )
}

/// `probeDetail` (`ffmpeg.go:211-218`): the full diagnostic for logs, which may
/// contain paths — ffprobe's stderr when it has any, otherwise the rendered
/// launch error.
pub fn probe_detail(stderr: Vec<u8>, raw: String) -> String {
    if stderr.is_empty() {
        return raw.clone();
    }

    String::from_utf8_lossy(&stderr).trim().to_string()
}

/// `probeClientReason` (`ffmpeg.go:220-230`): a path-free reason safe for
/// clients. Go substitutes the media path out of ffprobe's stderr with
/// `strings.ReplaceAll`, which replaces every occurrence — `String::replace`
/// does the same.
pub fn probe_client_reason(stderr: Vec<u8>, path: String) -> String {
    if stderr.is_empty() {
        return "could not read file".to_string();
    }

    String::from_utf8_lossy(&stderr)
        .replace(path.as_str(), "the file")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::errors::FFmpegError;
    use crate::probe::parse_probe_output;
    use crate::probe::probe_client_reason;
    use crate::probe::probe_detail;

    #[test]
    fn parse_probe_output_mp3_with_artwork_and_aac_lc() {
        // Arrange: real ffprobe output for an MP3 with an mjpeg artwork stream
        // after the audio stream, and for an AAC-LC m4a.
        let mp3 = r#"{"streams":[{"index":0,"codec_name":"mp3","codec_long_name":"MP3 (MPEG audio layer 3)","codec_type":"audio","sample_fmt":"fltp","sample_rate":"44100","channels":2,"channel_layout":"stereo","bits_per_sample":0,"bit_rate":"198314","tags":{"encoder":"LAME3.99r"}},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline","width":400,"height":400}]}"#;
        let aac = r#"{"streams":[{"index":0,"codec_name":"aac","codec_long_name":"AAC (Advanced Audio Coding)","profile":"LC","codec_type":"audio","sample_fmt":"fltp","sample_rate":"44100","channels":2,"channel_layout":"stereo","bits_per_sample":0,"bit_rate":"279958"},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline"}]}"#;
        // Act
        let (m, err_m) = parse_probe_output(mp3.as_bytes().to_vec());
        let (a, err_a) = parse_probe_output(aac.as_bytes().to_vec());
        // Assert: MP3 has no profile key, lossy codecs carry no bit depth
        assert!(err_m.is_none());
        assert_eq!(m.codec, "mp3".to_string());
        assert_eq!(m.profile, "");
        assert_eq!(m.sample_rate, 44100);
        assert_eq!(m.channels, 2);
        assert_eq!(m.bit_rate, 198);
        assert_eq!(m.bit_depth, 0);
        assert!(err_a.is_none());
        assert_eq!(a.codec, "aac".to_string());
        assert_eq!(a.profile, "LC");
        assert_eq!(a.sample_rate, 44100);
        assert_eq!(a.channels, 2);
        assert_eq!(a.bit_rate, 279);
    }

    #[test]
    fn parse_probe_output_he_aacv2_after_video_stream() {
        // Arrange / Act: LFE-SBRstereo.mp4 — the h264 video stream comes FIRST
        let (r, err) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"h264","codec_type":"video","profile":"Main"},{"index":1,"codec_name":"aac","codec_long_name":"AAC (Advanced Audio Coding)","profile":"HE-AACv2","codec_type":"audio","sample_fmt":"fltp","sample_rate":"48000","channels":2,"channel_layout":"stereo","bits_per_sample":0,"bit_rate":"55999"}]}"#.as_bytes().to_vec());
        // Assert: the video stream is skipped, audio still wins
        assert!(err.is_none());
        assert_eq!(r.codec, "aac".to_string());
        assert_eq!(r.profile, "HE-AACv2");
        assert_eq!(r.sample_rate, 48000);
        assert_eq!(r.channels, 2);
        assert_eq!(r.bit_rate, 55);
    }

    #[test]
    fn parse_probe_output_flac_and_opus_format_fallback() {
        // Arrange: FLAC carries no stream bit_rate (format-level 906900) and
        // reports depth in bits_per_raw_sample; Opus has neither depth nor
        // stream bit_rate.
        let flac = r#"{"streams":[{"index":0,"codec_name":"flac","codec_long_name":"FLAC (Free Lossless Audio Codec)","codec_type":"audio","sample_fmt":"s16","sample_rate":"44100","channels":2,"channel_layout":"stereo","bits_per_sample":0,"bits_per_raw_sample":"16"},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline"}],"format":{"bit_rate":"906900"}}"#;
        let opus = r#"{"streams":[{"index":0,"codec_name":"opus","codec_long_name":"Opus (Opus Interactive Audio Codec)","codec_type":"audio","sample_fmt":"fltp","sample_rate":"48000","channels":2,"channel_layout":"stereo","bits_per_sample":0}],"format":{"bit_rate":"128000"}}"#;
        // Act
        let (f, err_f) = parse_probe_output(flac.as_bytes().to_vec());
        let (o, err_o) = parse_probe_output(opus.as_bytes().to_vec());
        // Assert
        assert!(err_f.is_none());
        assert_eq!(f.codec, "flac".to_string());
        assert_eq!(f.sample_rate, 44100);
        assert_eq!(f.bit_depth, 16);
        assert_eq!(f.bit_rate, 906);
        assert_eq!(f.profile, "");
        assert!(err_o.is_none());
        assert_eq!(o.codec, "opus".to_string());
        assert_eq!(o.sample_rate, 48000);
        assert_eq!(o.channels, 2);
        assert_eq!(o.bit_rate, 128);
        assert_eq!(o.bit_depth, 0);
    }

    #[test]
    fn parse_probe_output_pcm_and_alac() {
        // Arrange / Act: WAV uses bits_per_sample directly; ALAC needs
        // bits_per_raw_sample.
        let (w, err_w) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"pcm_s16le","codec_long_name":"PCM signed 16-bit little-endian","codec_type":"audio","sample_fmt":"s16","sample_rate":"44100","channels":2,"bits_per_sample":16,"bit_rate":"1411200"}]}"#.as_bytes().to_vec());
        let (a, err_a) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"alac","codec_long_name":"ALAC (Apple Lossless Audio Codec)","codec_type":"audio","sample_fmt":"s16p","sample_rate":"44100","channels":2,"channel_layout":"stereo","bits_per_sample":0,"bit_rate":"1011003","bits_per_raw_sample":"16"},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline"}]}"#.as_bytes().to_vec());
        // Assert
        assert!(err_w.is_none());
        assert_eq!(w.codec, "pcm_s16le".to_string());
        assert_eq!(w.sample_rate, 44100);
        assert_eq!(w.channels, 2);
        assert_eq!(w.bit_depth, 16);
        assert_eq!(w.bit_rate, 1411);
        assert!(err_a.is_none());
        assert_eq!(a.codec, "alac".to_string());
        assert_eq!(a.bit_depth, 16);
        assert_eq!(a.sample_rate, 44100);
        assert_eq!(a.channels, 2);
        assert_eq!(a.bit_rate, 1011);
    }

    #[test]
    fn parse_probe_output_hires_flac_and_dsd() {
        // Arrange / Act: 192kHz/24-bit 7.1 FLAC (format-level bit_rate) and a
        // DSD64 DSF, which reports 8 bits_per_sample.
        let (h, err_h) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"flac","codec_long_name":"FLAC (Free Lossless Audio Codec)","codec_type":"audio","sample_fmt":"s32","sample_rate":"192000","channels":8,"channel_layout":"7.1","bits_per_sample":0,"bits_per_raw_sample":"24"},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Progressive"}],"format":{"bit_rate":"18432000"}}"#.as_bytes().to_vec());
        let (d, err_d) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"dsd_lsbf_planar","codec_long_name":"DSD (Direct Stream Digital), least significant bit first, planar","codec_type":"audio","sample_fmt":"fltp","sample_rate":"352800","channels":2,"channel_layout":"stereo","bits_per_sample":8,"bit_rate":"5644800"},{"index":1,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline"}]}"#.as_bytes().to_vec());
        // Assert
        assert!(err_h.is_none());
        assert_eq!(h.codec, "flac".to_string());
        assert_eq!(h.sample_rate, 192000);
        assert_eq!(h.bit_depth, 24);
        assert_eq!(h.channels, 8);
        assert_eq!(h.bit_rate, 18432);
        assert!(err_d.is_none());
        assert_eq!(d.codec, "dsd_lsbf_planar".to_string());
        assert_eq!(d.bit_depth, 8);
        assert_eq!(d.sample_rate, 352800);
        assert_eq!(d.channels, 2);
        assert_eq!(d.bit_rate, 5644);
    }

    #[test]
    fn parse_probe_output_bitrate_and_profile_precedence() {
        // Arrange / Act: stream-level bit_rate beats format-level when both are
        // present; neither leaves 0; "unknown" profile is cleared.
        let (wins, err_wins) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"alac","codec_type":"audio","sample_fmt":"s16p","sample_rate":"44100","channels":2,"bits_per_sample":0,"bit_rate":"1011003","bits_per_raw_sample":"16"}],"format":{"bit_rate":"1050000"}}"#.as_bytes().to_vec());
        let (neither, err_neither) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"flac","codec_type":"audio","sample_fmt":"s16","sample_rate":"44100","channels":2,"bits_per_sample":0,"bits_per_raw_sample":"16"}],"format":{}}"#.as_bytes().to_vec());
        let (unknown, err_unknown) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"flac","codec_type":"audio","profile":"unknown","sample_rate":"44100","channels":2,"bits_per_sample":0}]}"#.as_bytes().to_vec());
        // Assert
        assert!(err_wins.is_none());
        assert_eq!(wins.bit_rate, 1011);
        assert!(err_neither.is_none());
        assert_eq!(neither.bit_rate, 0);
        assert!(err_unknown.is_none());
        assert_eq!(unknown.profile, "");
    }

    #[test]
    fn parse_probe_output_failures() {
        // Arrange / Act: video-only, empty streams array, and non-JSON payload
        let (v, err_v) = parse_probe_output(r#"{"streams":[{"index":0,"codec_name":"mjpeg","codec_type":"video","profile":"Baseline"}]}"#.as_bytes().to_vec());
        let (e, err_e) = parse_probe_output(r#"{"streams":[]}"#.as_bytes().to_vec());
        let (_n, err_n) = parse_probe_output("not json".as_bytes().to_vec());
        // Assert: no audio stream found in each of the first two, JSON error in
        // the third
        assert!(err_v.is_some());
        assert!(matches!(err_v, Some(FFmpegError::NoAudioStream)));
        assert!(err_v.unwrap().to_string().contains("no audio stream"));
        assert_eq!(v.bit_rate, 0);
        assert_eq!(e.codec, "");
        assert!(err_e.is_some());
        assert!(matches!(err_e, Some(FFmpegError::NoAudioStream)));
        assert!(err_n.is_some());
        assert!(matches!(err_n, Some(FFmpegError::ProbeParse(_))));
        assert!(err_n
            .unwrap()
            .to_string()
            .contains("parsing ffprobe output: "));
    }

    #[test]
    fn probe_detail_and_client_reason() {
        // Arrange / Act: ffprobe stderr wins for the log detail; a launch error
        // with no stderr falls back to the rendered error, and the client-safe
        // reason strips the media path.
        let stderr = "\n/music/Track.mp3: Invalid data found when processing input\n"
            .as_bytes()
            .to_vec();
        // Assert
        assert_eq!(
            probe_detail(stderr.clone(), "ffprobe exited with code 1".to_string()),
            "/music/Track.mp3: Invalid data found when processing input"
        );
        assert_eq!(
            probe_detail(
                "".as_bytes().to_vec(),
                "No such file or directory".to_string()
            ),
            "No such file or directory"
        );
        assert_eq!(
            probe_client_reason(stderr.clone(), "/music/Track.mp3".to_string()),
            "the file: Invalid data found when processing input"
        );
        assert_eq!(
            probe_client_reason("".as_bytes().to_vec(), "/music/Track.mp3".to_string()),
            "could not read file"
        );
    }
}
