//! The probe-JSON codec: what Go marshals into `MediaFile.ProbeData`
//! (`json.Marshal(result)` + `string(data)` at `decider.go:444-448`) and what
//! [`crate::decider::parse_probe_data`] reads back (`decider.go:181-190`).
//! The `AudioProbeResult` wire shape itself belongs to nd-ffmpeg (#07), so
//! only the marshal helper lives at this consumer boundary — it lets the
//! tests reproduce Go's `withProbe` helper byte-for-byte instead of hand-
//! writing JSON.

use serde_json::to_string;

use nd_ffmpeg::probe::AudioProbeResult;

/// Marshal an ffprobe verdict into the `probe_data` payload
/// (`json.Marshal(result)` + `string(data)` at `decider.go:444-448`).
#[must_use]
pub fn probe_json(probe: &AudioProbeResult) -> String {
    to_string(probe).unwrap_or_default()
}

/// The bytes-shaped sibling of [`probe_json`] — Go's `json.Marshal` returns
/// `[]byte` before the `string(data)` cast (`decider.go:444-448`).
#[must_use]
pub fn probe_bytes(probe: &AudioProbeResult) -> Vec<u8> {
    to_string(probe).unwrap_or_default().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decider::parse_probe_data;

    #[test]
    fn probe_json_round_trips_through_parse_probe_data() {
        // Arrange: Go's `withProbe`-style canned verdict.
        let probe = AudioProbeResult {
            codec: "wavpack".into(),
            profile: "".into(),
            bit_rate: 1000,
            sample_rate: 44100,
            bit_depth: 16,
            channels: 2,
        };
        // Act
        let data = probe_json(&probe);
        let back = parse_probe_data(&data);
        // Assert
        assert!(!data.is_empty());
        let Some(result) = back else {
            unreachable!("round-trip")
        };
        assert_eq!(result.codec, "wavpack");
        assert_eq!(result.bit_rate, 1000);
        assert_eq!(result.sample_rate, 44100);
        assert_eq!(result.bit_depth, 16);
        assert_eq!(result.channels, 2);
    }

    #[test]
    fn probe_bytes_matches_probe_json_bytes() {
        // Act/Assert: the `[]byte` then `string(data)` pair (`decider.go:444-448`)
        let probe = AudioProbeResult {
            codec: "mp3".into(),
            profile: "".into(),
            bit_rate: 320,
            sample_rate: 44100,
            bit_depth: 0,
            channels: 2,
        };
        assert_eq!(probe_bytes(&probe), probe_json(&probe).as_bytes().to_vec());
    }
}
