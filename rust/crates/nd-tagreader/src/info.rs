//! `Info` / `AudioProperties`: `model/metadata/metadata.go:21-40`.
//!
//! The tag adapter fills Tags, AudioProperties and HasPicture
//! (`adapters/gotaglib/gotaglib.go:118-122`). Go's FileInfo field is enriched
//! by storage later (`core/storage/local/local.go:96-107`); #17 owns that
//! consumer, not the tag parser. Keys are lowercase; value order is preserved.

use std::collections::BTreeMap;

/// `model/metadata/metadata.go:33-40` (filled by `gotaglib.go:87-94`).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioProperties {
    /// Duration in seconds, rounded to 10ms (`gotaglib.go:88`).
    pub duration: f64,
    /// kbps.
    pub bit_rate: i64,
    /// Channel count.
    pub channels: i64,
    /// Sample rate in Hz.
    pub sample_rate: i64,
    /// Bits per sample.
    pub bit_depth: i64,
    /// Container codec name (props.Codec).
    pub codec: String,
}

/// Adapter output: `model/metadata/metadata.go:21-26`, excluding storage FileInfo.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Tag name → values; keys are sorted for reproducible Rust tests.
    /// Go's map order is unspecified, but value order within a key is retained.
    pub tags: BTreeMap<String, Vec<String>>,
    /// Audio stream properties.
    pub audio_properties: AudioProperties,
    /// File has an embedded picture (`gotaglib.go:116`; len(props.Images) > 0).
    pub has_picture: bool,
}
