//! Shim backend for `adapters/gotaglib/gotaglib.go`.
//!
//! Decision (issue #08): pure-Rust TagLib is unavailable offline and a C
//! binding would fork the pinned `go.senan.xyz/taglib ->
//! github.com/deluan/go-taglib` raw-tag surface (go.mod:6). The raw TagLib
//! surface (PropertyMap tags, raw frames/atoms/attrs, stream properties) is
//! kept as *data* — produced today by the pinned fork, tomorrow by whatever
//! binary the operator wires — and every Navidrome-side transform below is
//! ported byte-exactly: key lowercasing, raw-tag merging per format,
//! track/disc tuple split, lyrics/TIPL handling, `has_picture`.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::from_str;

use crate::info::{AudioProperties, Info};

/// Pinned go-taglib fork version (`go.mod:6`), replacing Go's
/// `debug.ReadBuildInfo()` lookup (`gotaglib.go:26-36`).
pub const PINNED_TAGLIB_VERSION: &'static str =
    "v0.0.0-20260913142955-d55e0c9353cb";

/// `gotaglib.go:194` — iTunes atom prefix.
const ITUNES_KEY_PREFIX: &'static str = "----:com.apple.iTunes:";

/// `tiplMapping` (`gotaglib.go:223-229`) — Picard's supported roles only.
const TIPL_MAPPING: &'static [(&'static str, &'static str)] = &[
    ("arranger", "arranger"),
    ("engineer", "engineer"),
    ("producer", "producer"),
    ("mix", "mixer"),
    ("DJ-mix", "djmixer"),
];

/// `taglib.Format*` (`gotaglib.go:158-165`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// `taglib.Format*` (`gotaglib.go:158-165`) — the shim dump spells it as the
/// container name; anything unrecognized gets no raw-tag processing.
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

fn format_of(name: String) -> Format {
    let lower = name.to_lowercase();
    if lower == "mpeg".to_string() || lower == "mp3".to_string() {
        return Format::MPEG
    }
    if lower == "wav".to_string() {
        return Format::WAV
    }
    if lower == "aiff".to_string() || lower == "aif".to_string() {
        return Format::AIFF
    }
    if lower == "mp4".to_string() || lower == "m4a".to_string() {
        return Format::MP4
    }
    if lower == "asf".to_string() || lower == "wma".to_string() {
        return Format::ASF
    }
    Format::MPEG
}

/// The raw TagLib surface — `allTags.Tags`, `allTags.Raw` and the `props`
/// fields `extractMetadata` reads (`gotaglib.go:107-133`). BTreeMap keeps
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
    /// `props.Images` count; `has_picture` is `> 0` (`gotaglib.go:130`).
    pub image_count: usize,
    /// `props.Length` in seconds.
    /// `props.Length` in seconds.
    pub duration: f64,
    /// `props.Bitrate`.
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


/// `gotaglib.go:37-133` minus the file-open: pure transform over RawTags.
pub fn extract_metadata(raw: &RawTags) -> Info {
    let tags = normalize_tags(&raw);
    let mut tags = process_raw_tags(&raw, tags);
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
    Info { tags: tags, audio_properties: ap, has_picture: raw.image_count > 0 }
}

/// `gotaglib.go:102-107` — lowercase every PropertyMap key.
fn normalize_tags(raw: &RawTags) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for (key, values) in raw.tags.iter() {
        out.insert(key.to_lowercase(), values.clone());
    }
    out
}

/// `props.Length.Round(time.Millisecond * 10)` (`gotaglib.go:107`).
fn round_10ms(d: f64) -> f64 {
    (d * 100.0).round() / 100.0
}

/// `processRawTags` (`gotaglib.go:152-165`).
/// `processRawTags` (`gotaglib.go:152-165`) — returns the merged tag map.
fn process_raw_tags(raw: &RawTags, tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    match format_of(raw.format.clone()) {
        Format::MPEG | Format::WAV | Format::AIFF => parse_id3v2_frames(raw, tags),
        Format::MP4 => parse_mp4_atoms(raw, tags),
        Format::ASF => parse_asf_attributes(raw, tags),
    }
}

/// `parseID3v2Frames` (`gotaglib.go:167-192`): language-coded USLT/SYLT
/// frames become `lyrics:<lang>`; coded lyrics supersede the generic key.
fn parse_id3v2_frames(raw: &RawTags, mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
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
                if lang.len() == 0 {
                    continue;
                }
                let lyrics_key = format!("lyrics:{}", lang);
                tags.entry(lyrics_key).or_default().extend(values.iter().map(|v| v.to_string()));
            },
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

/// `parseMP4Atoms` (`gotaglib.go:196-207`).
fn parse_mp4_atoms(raw: &RawTags, mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    for (key, values) in raw.raw.iter() {
        let normalized = strip_itunes_prefix(key.to_string()).to_lowercase();
        if !tags.contains_key(&normalized) {
            tags.insert(normalized, values.clone());
        }
    }
    tags
}

fn strip_itunes_prefix(key: String) -> String {
    match key.strip_prefix(ITUNES_KEY_PREFIX) {
        Some(rest) => rest.to_string(),
        None => key,
    }
}

/// `parseASFAttributes` (`gotaglib.go:211-221`).
fn parse_asf_attributes(raw: &RawTags, mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    for (key, values) in raw.raw.iter() {
        let normalized = key.to_lowercase();
        if !tags.contains_key(&normalized) {
            tags.insert(normalized, values.clone());
        }
    }
    tags
}

/// `parseTuple` (`gotaglib.go:111-127`): "2/10" -> track=2, tracktotal=10.
fn parse_tuple(mut tags: BTreeMap<String, Vec<String>>, prop: String) -> BTreeMap<String, Vec<String>> {
    let tag_name = prop.to_string() + "number";
    let tag_total = prop.to_string() + "total";
    match tags.get(&tag_name) {
        None => (),
        Some(vs) => {
            match vs.len() {
                0 => (),
                _ => {
                    match vs[0].split_once("/") {
                        None => (),
                        Some(pair) => {
                            let mut n: Vec<String> = Vec::new();
                            n.push(pair.0.to_string());
                            let mut t: Vec<String> = Vec::new();
                            t.push(pair.1.to_string());
                            tags.insert(tag_name, n);
                            tags.insert(tag_total, t);
                        },
                    };
                },
            };
        },
    }
    tags
}

/// `parseLyrics` (`gotaglib.go:139-146`).
fn parse_lyrics(mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    match tags.get("lyrics") {
        None => (),
        Some(lyrics) => {
            tags.insert("lyrics:xxx".to_string(), lyrics.clone());
            tags.remove("lyrics");
        },
    }
    tags
}

/// `parseTIPL` (`gotaglib.go:230-260`) with `tiplMapping` (`:223-229`).
fn parse_tipl(mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    match tags.get("tipl") {
        None => return tags,
        Some(v) => {
            match v.len() { 0 => return tags, _ => () };
        }
    }
    let mut role: String = "".to_string();
    let mut names: Vec<String> = Vec::new();
    let tipl_key = "tipl".to_string();
    let tipl0 = tags.get(&tipl_key).unwrap()[0].clone();
    for part in tipl0.split(" ") {
        if tipl_is_role(part) {
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

fn tipl_is_role(part: &str) -> bool {
    for (k, _) in TIPL_MAPPING {
        if k.to_string() == part.to_string() {
            return true;
        }
    }
    false
}

fn tipl_flush(role: String, names: &[String], mut tags: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    for (k, mapped) in TIPL_MAPPING {
        if k.to_string() == role.to_string() && names.len() > 0 {
            let joined = names.join(" ");
            tags.entry(mapped.to_string()).or_default().extend([joined]);
            break;
        }
    }
    tags
}

/// `extractMetadata`'s return type (`gotaglib.go:38` returns `metadata.Info`).
pub type ExtractMetadataResult = Info;


/// Shim loader: reads the `<file>.tags.json` sidecar emitted by the pinned
/// go-taglib binary (shim backend, #08). Missing sidecar = Go's open failure
/// (`gotaglib.go:47` warn+skip).
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
