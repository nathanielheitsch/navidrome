//! Navidrome's Go TagLib post-transform (`adapters/gotaglib/gotaglib.go`).
//!
//! The real-file backend is [`crate::backend`]. `RawTags` also remains the
//! serialized input format for Go-produced golden oracles; its sidecar loader
//! is a test utility, never a runtime fallback.

use serde::{Deserialize, Serialize};
use serde_json::from_str;
use std::collections::BTreeMap;

use crate::info::{AudioProperties, Info};

/// Go-fork version used to produce the golden snapshots (`go.mod:6`).
/// The real extractor reports its own Lofty version, not this oracle version.
pub const PINNED_TAGLIB_VERSION: &str = "v0.0.0-20260913142955-d55e0c9353cb";

/// `adapters/gotaglib/gotaglib.go:219` — iTunes atom prefix.
const ITUNES_KEY_PREFIX: &str = "----:com.apple.iTunes:";

/// `tiplMapping` (`adapters/gotaglib/gotaglib.go:251-257`).
const TIPL_MAPPING: &[(&str, &str)] = &[
    ("arranger", "arranger"),
    ("engineer", "engineer"),
    ("producer", "producer"),
    ("mix", "mixer"),
    ("DJ-mix", "djmixer"),
];

/// `adapters/gotaglib/gotaglib.go:181-189` — raw overlay formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Format {
    /// `taglib.FormatMPEG`.
    MPEG,
    /// `taglib.FormatWAV`.
    WAV,
    /// `taglib.FormatAIFF`.
    AIFF,
    /// `taglib.FormatMP4`.
    MP4,
    /// `taglib.FormatASF`.
    ASF,
}

fn format_of(name: &str) -> Option<Format> {
    match name.to_lowercase().as_str() {
        "mpeg" | "mp3" => Some(Format::MPEG),
        "wav" => Some(Format::WAV),
        "aiff" | "aif" => Some(Format::AIFF),
        "mp4" | "m4a" => Some(Format::MP4),
        "asf" | "wma" => Some(Format::ASF),
        _ => None,
    }
}

/// The raw TagLib surface — `allTags.Tags`, `allTags.Raw` and the `props`
/// fields `extractMetadata` reads (`gotaglib.go:82-121`). BTreeMap keeps
/// iteration deterministic (Go map order is unspecified; sorted keys match
/// every fixture case).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTags {
    /// Container name from the shim dump ("mpeg"/"mp4"/"asf"/"" for others).
    pub format: String,
    /// `allTags.Tags` — PropertyMap tags, UPPERCASE keys as TagLib returns.
    pub tags: BTreeMap<String, Vec<String>>,
    /// `allTags.Raw` — ID3v2 frames / MP4 atoms / ASF attributes.
    /// Raw frames/atoms/attributes keyed per format.
    pub raw: BTreeMap<String, Vec<String>>,
    /// `props.Images` count; `has_picture` is `> 0` (`gotaglib.go:116`).
    pub image_count: usize,
    /// `props.Length` in seconds.
    pub duration: f64,
    /// `props.Bitrate` in kbps.
    pub bitrate: i64,
    /// `props.Channels`.
    pub channels: i64,
    /// `props.SampleRate`.
    pub sample_rate: i64,
    /// `props.BitsPerSample`.
    pub bits_per_sample: i64,
    /// `props.Codec` container codec name.
    pub codec: String,
}

/// `gotaglib.go:82-122` minus the file-open: pure transform over RawTags.
pub fn extract_metadata(raw: &RawTags) -> Info {
    let tags = normalize_tags(raw);
    let mut tags = process_raw_tags(raw, tags);
    tags = parse_tuple(tags, "track".to_string());
    tags = parse_tuple(tags, "disc".to_string());
    tags = parse_lyrics(tags);
    tags = parse_tipl(tags);
    tags.remove("tmcl");
    let ap = AudioProperties {
        duration: round_10ms(raw.duration),
        bit_rate: raw.bitrate,
        channels: raw.channels,
        sample_rate: raw.sample_rate,
        bit_depth: raw.bits_per_sample,
        codec: raw.codec.clone(),
    };
    let has_picture = raw.image_count > 0;
    Info {
        tags,
        audio_properties: ap,
        has_picture,
    }
}

/// `gotaglib.go:96-101` — lowercase every PropertyMap key.
fn normalize_tags(raw: &RawTags) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for (key, values) in raw.tags.iter() {
        out.insert(key.to_lowercase(), values.clone());
    }
    out
}

/// `props.Length.Round(time.Millisecond * 10)` (`gotaglib.go:88`).
fn round_10ms(d: f64) -> f64 {
    (d * 100.0).round() / 100.0
}

/// `processRawTags` (`gotaglib.go:181-190`) — merge format-specific overlays.
fn process_raw_tags(
    raw: &RawTags,
    tags: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    match format_of(&raw.format) {
        Some(Format::MPEG | Format::WAV | Format::AIFF) => parse_id3v2_frames(raw, tags),
        Some(Format::MP4) => parse_mp4_atoms(raw, tags),
        Some(Format::ASF) => parse_asf_attributes(raw, tags),
        None => tags,
    }
}

/// `parseID3v2Frames` (`gotaglib.go:194-217`): language-coded USLT/SYLT
/// frames become `lyrics:<lang>`; coded lyrics supersede the generic key.
fn parse_id3v2_frames(
    raw: &RawTags,
    mut tags: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    for (key, values) in raw.raw.iter() {
        let lower = key.to_lowercase();
        let is_uslt = lower.starts_with("uslt:");
        let is_sylt = lower.starts_with("sylt:");
        if !is_uslt && !is_sylt {
            continue;
        }
        match lower.split_once(":") {
            None => (),
            Some((_, lang)) => {
                if lang.is_empty() {
                    continue;
                }
                let lyrics_key = format!("lyrics:{}", lang);
                tags.entry(lyrics_key)
                    .or_default()
                    .extend(values.iter().map(|v| v.to_string()));
            }
        }
    }
    let mut coded = false;
    for k in tags.keys() {
        if k.starts_with("lyrics:") && k != "lyrics" {
            coded = true;
            break;
        }
    }
    if coded {
        tags.remove("lyrics");
    }
    tags
}

/// `parseMP4Atoms` (`gotaglib.go:222-234`).
fn parse_mp4_atoms(
    raw: &RawTags,
    mut tags: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    for (key, values) in raw.raw.iter() {
        let normalized = strip_itunes_prefix(key.to_string()).to_lowercase();
        tags.entry(normalized).or_insert_with(|| values.clone());
    }
    tags
}

fn strip_itunes_prefix(key: String) -> String {
    match key.strip_prefix(ITUNES_KEY_PREFIX) {
        Some(rest) => rest.to_string(),
        None => key,
    }
}

/// `parseASFAttributes` (`gotaglib.go:237-247`).
fn parse_asf_attributes(
    raw: &RawTags,
    mut tags: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    for (key, values) in raw.raw.iter() {
        let normalized = key.to_lowercase();
        tags.entry(normalized).or_insert_with(|| values.clone());
    }
    tags
}

/// `parseTuple` (`gotaglib.go:157-167`): first number wins; a two-part tuple overrides total.
fn parse_tuple(
    mut tags: BTreeMap<String, Vec<String>>,
    prop: String,
) -> BTreeMap<String, Vec<String>> {
    let tag_name = prop.to_string() + "number";
    let tag_total = prop.to_string() + "total";
    if let Some(value) = tags
        .get(&tag_name)
        .and_then(|values| values.first())
        .cloned()
    {
        let parts: Vec<_> = value.split('/').collect();
        tags.insert(tag_name, vec![parts[0].to_owned()]);
        if parts.len() == 2 {
            tags.insert(tag_total, vec![parts[1].to_owned()]);
        }
    }
    tags
}

/// `parseLyrics` (`gotaglib.go:171-177`).
fn parse_lyrics(mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    if let Some(lyrics) = tags.get("lyrics").filter(|values| !values.is_empty()) {
        tags.insert("lyrics:xxx".to_string(), lyrics.clone());
        tags.remove("lyrics");
    }
    tags
}

/// `parseTIPL` (`gotaglib.go:266-290`) with `tiplMapping` (`:251-257`).
fn parse_tipl(mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    match tags.get("tipl") {
        None => return tags,
        Some(v) => {
            if v.is_empty() {
                return tags;
            }
        }
    }
    let mut role: String = "".to_string();
    let mut names: Vec<String> = Vec::new();
    let tipl_key = "tipl".to_string();
    let tipl0 = tags.get(&tipl_key).unwrap()[0].clone();
    for part in tipl0.split(" ") {
        if tipl_is_role(part.to_string()) {
            tags = tipl_flush(role.clone(), &names, tags);
            role = part.to_string();
            names.clear();
        } else {
            names.push(part.to_string());
        }
    }
    tags = tipl_flush(role.clone(), &names, tags);
    tags.remove("tipl");
    tags
}

fn tipl_is_role(part: String) -> bool {
    for (k, _) in TIPL_MAPPING {
        if part == *k {
            return true;
        }
    }
    false
}

fn tipl_flush(
    role: String,
    names: &[String],
    mut tags: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    for (k, mapped) in TIPL_MAPPING {
        if !names.is_empty() && role == *k {
            let joined = names.join(" ");
            tags.entry(mapped.to_string()).or_default().extend([joined]);
            break;
        }
    }
    tags
}

/// `extractMetadata`'s return type (`gotaglib.go:118-122`).
pub type ExtractMetadataResult = Info;

/// Load a Go-produced golden snapshot (`adapters/gotaglib/golden_dump_test.go:46-58`).
/// Retained for oracle tests only; the runtime extractor never calls this.
pub fn load_shim_raw_tags(fs_root: &str, base_dir: &str, path: &str) -> Result<RawTags, String> {
    let root = format!("{}/{}", fs_root, base_dir);
    let dump = match std::fs::read_to_string(format!("{}/{}.tags.json", root, path)) {
        Ok(t) => t,
        Err(e) => return Err(format!("shim dump: {}", e)),
    };
    match from_str::<RawTags>(&dump) {
        Ok(raw) => Ok(raw),
        Err(e) => Err(format!("shim dump: {}", e)),
    }
}
