//! `Info` / `AudioProperties` DTOs — port of `model/metadata.go` Info shape as
//! consumed by `adapters/gotaglib` (`gotaglib.go:115-133`).
//!
//! Go: `type Info struct { Tags map[string][]string; AudioProperties AudioProperties; HasPicture bool }`.
//! Tags keys are lowercase normalized (gotaglib.go:96-99 lowercases PropertyMap
//! keys); values preserve order per key.

use std::collections::BTreeMap;

/// `model/metadata.go` AudioProperties (fields set by gotaglib.go:106-111).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioProperties {
    /// Duration in seconds, rounded to 10ms by the extractor (gotaglib.go:107).
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

/// `model/metadata.go` Info.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Tag name → values (Go `Tags map[string][]string`; BTreeMap = sorted keys,
    /// matching Go's deterministic map iteration for tests).
    pub tags: BTreeMap<String, Vec<String>>,
    /// Audio stream properties.
    pub audio_properties: AudioProperties,
    /// File has an embedded picture (gotaglib.go:130; len(props.Images) > 0).
    pub has_picture: bool,
}
