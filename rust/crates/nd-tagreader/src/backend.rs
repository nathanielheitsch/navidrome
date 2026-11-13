//! Real-file metadata extraction with pinned, pure-Rust Lofty (no subprocess or C library).
//!
//! The output boundary is `model/metadata/metadata.go:21-40`, not a mapped
//! MediaFile: cleaning, participant construction and file stat enrichment belong
//! to #16/#17 (`model/metadata/metadata.go:70-76`, `map_mediafile.go:18-115`).
//! See the crate README for the explicit remaining differences from Go TagLib.

use std::collections::BTreeMap;
use std::path::Path;

use lofty::config::ParseOptions;
use lofty::error::FileParseError;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, Id3v2Tag, SynchronizedTextFrame, TimestampFormat};
use lofty::iff::{aiff::AiffFile, wav::WavFile};
use lofty::mp4::{AtomData, AtomIdent, Ilst, Mp4Codec, Mp4File};
use lofty::mpeg::{Layer, MpegFile};
use lofty::ogg::{OpusFile, SpeexFile, VorbisFile};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, Tag, TagType};

use crate::gotaglib::{extract_metadata, RawTags};
use crate::Info;

/// The actual parser version, unlike the old shim's Go-fork version string.
pub const BACKEND_VERSION: &str = "lofty/0.25.5";

type Tags = BTreeMap<String, Vec<String>>;

enum NativeTag {
    Id3(Id3v2Tag),
    Mp4(Ilst),
}

/// Read tags, audio properties and embedded-picture presence directly from media.
///
/// Errors are exposed here; the batch extractor skips individual failures just
/// like `adapters/gotaglib/gotaglib.go:34-43`. Sidecars are never opened.
pub fn read_metadata(path: impl AsRef<Path>) -> Result<Info, FileParseError> {
    let probe = Probe::open(path)?.guess_file_type()?;
    let mut comments = None;
    let mut native_tag = None;
    let (file, codec): (TaggedFile, String) = match probe.file_type() {
        Some(FileType::Mp4) => {
            let native = Mp4File::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            let codec = match native.properties().codec() {
                Some(Mp4Codec::AAC) => "AAC",
                Some(Mp4Codec::ALAC) => "ALAC",
                Some(Mp4Codec::MP3) => "MP3",
                Some(Mp4Codec::FLAC) => "FLAC",
                _ => "",
            };
            native_tag = native.ilst().cloned().map(NativeTag::Mp4);
            (native.into(), codec.to_owned())
        }
        Some(FileType::Mpeg) => {
            let native = MpegFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            let codec = match native.properties().layer() {
                Layer::Layer1 => "MP1",
                Layer::Layer2 => "MP2",
                Layer::Layer3 => "MP3",
            };
            native_tag = native.id3v2().cloned().map(NativeTag::Id3);
            (native.into(), codec.to_owned())
        }
        Some(FileType::Wav) => {
            let native = WavFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            native_tag = native.id3v2().cloned().map(NativeTag::Id3);
            (native.into(), String::new())
        }
        Some(FileType::Aiff) => {
            let native = AiffFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            native_tag = native.id3v2().cloned().map(NativeTag::Id3);
            (native.into(), String::new())
        }
        Some(FileType::Flac) => {
            let native = FlacFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            comments = native.vorbis_comments().cloned();
            (native.into(), String::new())
        }
        Some(FileType::Vorbis) => {
            let native = VorbisFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            comments = Some(native.vorbis_comments().clone());
            (native.into(), String::new())
        }
        Some(FileType::Opus) => {
            let native = OpusFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            comments = Some(native.vorbis_comments().clone());
            (native.into(), String::new())
        }
        Some(FileType::Speex) => {
            let native = SpeexFile::read_from(&mut probe.into_inner(), ParseOptions::new())?;
            comments = Some(native.vorbis_comments().clone());
            (native.into(), String::new())
        }
        // The Go oracle only reports codec labels for MPEG/MP4; the FLAC,
        // Vorbis, Opus, WAV, AIFF and WavPack goldens leave Codec empty
        // (adapters/gotaglib/golden_dump_test.go:43-56). Do not guess from
        // a container name, particularly for compressed WAV/AIFF.
        _ => (probe.read()?, String::new()),
    };
    let props = file.properties();
    let mut raw = RawTags {
        format: match file.file_type() {
            FileType::Mp4 => "mp4",
            FileType::Mpeg => "mpeg",
            FileType::Wav => "wav",
            FileType::Aiff => "aiff",
            _ => "",
        }
        .to_owned(),
        tags: Tags::new(),
        raw: Tags::new(),
        image_count: file.tags().iter().map(|tag| tag.pictures().len()).sum(),
        duration: props.duration().as_secs_f64(),
        bitrate: props.audio_bitrate().unwrap_or(0).into(),
        channels: props.channels().unwrap_or(0).into(),
        sample_rate: props.sample_rate().unwrap_or(0).into(),
        bits_per_sample: props.bit_depth().unwrap_or(0).into(),
        codec,
    };
    // Do not merge stale ID3v1 fields into an existing ID3v2 PropertyMap.
    // TagLib selects a tag source, then Go applies the raw overlays below
    // (adapters/gotaglib/gotaglib.go:82-113). MPEG fallback is APE then ID3v1.
    let tag = file
        .primary_tag()
        .or_else(|| file.tag(TagType::Ape).or_else(|| file.first_tag()));
    if let Some(comments) = comments {
        // Keep native aliases distinct (e.g. TOTALTRACKS and TRACKTOTAL).
        // A generic Tag conversion coalesces them and duplicates values;
        // Go lowercases the PropertyMap without such coalescing (go:96-101).
        for (key, value) in comments.items() {
            push(&mut raw.tags, key.to_uppercase(), value);
        }
    } else if let Some(tag) = tag {
        collect_tag(tag, &mut raw, native_tag);
    }
    Ok(extract_metadata(&raw))
}

fn push(tags: &mut Tags, key: impl Into<String>, value: impl Into<String>) {
    tags.entry(key.into()).or_default().push(value.into());
}

fn text(value: &ItemValue) -> Option<&str> {
    match value {
        ItemValue::Text(s) | ItemValue::Locator(s) => Some(s),
        ItemValue::Binary(_) => None,
    }
}

fn property_key(key: ItemKey) -> Option<String> {
    // Most PropertyMap names match Vorbis. These are the exceptions in
    // TagLib's vocabulary (Go consumes them unchanged at gotaglib.go:96-101).
    let name = match key {
        ItemKey::IntegerBpm | ItemKey::Bpm => "BPM",
        ItemKey::ComposerSortOrder => "COMPOSERSORT",
        ItemKey::MusicBrainzReleaseType => "MUSICBRAINZ_ALBUMTYPE",
        ItemKey::UnsyncLyrics => "LYRICS",
        ItemKey::EncoderSoftware => "ENCODING",
        _ => key.map_key(TagType::VorbisComments)?,
    };
    Some(name.to_owned())
}

fn collect_tag(tag: &Tag, raw: &mut RawTags, native: Option<NativeTag>) {
    for item in tag.items() {
        let Some(value) = text(item.value()) else {
            continue;
        };
        let Some(mut key) = property_key(item.key()) else {
            continue;
        };
        if tag.tag_type() == TagType::Id3v1 && item.key() == ItemKey::Year {
            key = "DATE".into();
        }
        if tag.tag_type() == TagType::Id3v2 {
            if matches!(item.key(), ItemKey::Lyrics | ItemKey::UnsyncLyrics) {
                // Go prefers coded USLT/SYLT over the generic lyrics property
                // (gotaglib.go:194-216), without dropping language or variants.
                let lang = String::from_utf8_lossy(item.lang()).to_lowercase();
                push(&mut raw.raw, format!("USLT:{lang}"), value);
                continue;
            }
            if item.key() == ItemKey::Comment && !item.description().is_empty() {
                key = format!("COMMENT:{}", item.description());
            }
        }
        push(&mut raw.tags, key, value);
    }
    // Lofty 0.25 keeps unknown items in a companion tag, not Tag::items().
    // Prefer the original native tag so conversion cannot change numeric
    // atoms or malformed tuples. Fallbacks recover unknown companion items.
    match tag.tag_type() {
        TagType::Id3v2 => {
            let native = match native {
                Some(NativeTag::Id3(native)) => native,
                _ => tag.clone().into(),
            };
            collect_id3(native, raw);
        }
        TagType::Mp4Ilst => {
            let native = match native {
                Some(NativeTag::Mp4(native)) => native,
                _ => tag.clone().into(),
            };
            collect_mp4(native, raw);
        }
        TagType::VorbisComments => {
            let native: lofty::ogg::tag::VorbisComments = tag.clone().into();
            for (key, value) in native.items() {
                if ItemKey::from_key(TagType::VorbisComments, key).is_none() {
                    push(&mut raw.tags, key.to_uppercase(), value);
                }
            }
        }
        TagType::Ape => {
            let native: lofty::ape::ApeTag = tag.clone().into();
            for item in &native {
                if let ItemValue::Binary(data) = item.value() {
                    if lofty::picture::Picture::from_ape_bytes(item.key(), data).is_ok() {
                        raw.image_count += 1;
                    }
                }
                if ItemKey::from_key(TagType::Ape, item.key()).is_none() {
                    if let Some(values) = item.text_values() {
                        for value in values {
                            push(&mut raw.tags, item.key().to_uppercase(), value);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_id3(native: Id3v2Tag, raw: &mut RawTags) {
    for frame in &native {
        match frame {
            Frame::Text(frame) if matches!(frame.id().as_str(), "TRCK" | "TPOS") => {
                // Preserve the tuple until Go's parseTuple runs: its total
                // overrides a separately stored total (gotaglib.go:157-165).
                let key = if frame.id().as_str() == "TRCK" {
                    "TRACKNUMBER"
                } else {
                    "DISCNUMBER"
                };
                raw.tags.insert(key.into(), vec![frame.value.to_string()]);
            }
            Frame::UserText(frame) => {
                if ItemKey::from_key(TagType::Id3v2, &frame.description).is_none() {
                    for value in frame.content.split('\0') {
                        push(&mut raw.tags, frame.description.to_uppercase(), value);
                    }
                }
            }
            Frame::KeyValue(frame) if frame.id().as_str() == "TMCL" => {
                // TagLib exposes musician credits as PERFORMER:<instrument>;
                // Go intentionally removes TMCL itself (gotaglib.go:113).
                for (role, name) in frame.key_value_pairs.iter() {
                    push(&mut raw.tags, format!("PERFORMER:{role}"), name.as_ref());
                }
            }
            Frame::Binary(frame) if frame.id().as_str() == "SYLT" => {
                let Ok(lyrics) = SynchronizedTextFrame::parse(&frame.data, frame.flags()) else {
                    continue;
                };
                // Frame-based timestamps need stream timing; unlike TagLib,
                // Lofty does not convert them. Never label them milliseconds.
                if lyrics.timestamp_format != TimestampFormat::MS {
                    continue;
                }
                let mut lrc = String::new();
                for (ms, line) in lyrics.content {
                    use std::fmt::Write;
                    let _ = writeln!(
                        lrc,
                        "[{:02}:{:02}.{:02}]{}",
                        ms / 60000,
                        ms / 1000 % 60,
                        ms / 10 % 100,
                        line
                    );
                }
                let lang = String::from_utf8_lossy(&lyrics.language).to_lowercase();
                push(&mut raw.raw, format!("SYLT:{lang}"), lrc);
            }
            _ => {}
        }
    }
}

fn collect_mp4(native: Ilst, raw: &mut RawTags) {
    // Normalized properties win over raw atoms; the existing Go transform
    // handles stripping ----:com.apple.iTunes: and insert-if-absent
    // (adapters/gotaglib/gotaglib.go:219-233).
    for atom in &native {
        let key = match atom.ident() {
            AtomIdent::Fourcc(bytes) => bytes.iter().map(|b| char::from(*b)).collect(),
            AtomIdent::Freeform { mean, name } => format!("----:{mean}:{name}"),
        };
        let values: Vec<String> = atom
            .data()
            .filter_map(|value| match value {
                AtomData::UTF8(s) | AtomData::UTF16(s) => Some(s.clone()),
                AtomData::SignedInteger(n) => Some(n.to_string()),
                AtomData::UnsignedInteger(n) => Some(n.to_string()),
                AtomData::Bool(b) => Some(u8::from(*b).to_string()),
                _ => None,
            })
            .collect();
        if values.is_empty() {
            continue;
        }
        // Lofty's generic conversion retains numeric atoms (e.g. tmpo) in
        // its native companion instead of creating an ItemKey. Fill their
        // normalized PropertyMap names here, without overriding text fields.
        if let Some(property) = ItemKey::from_key(TagType::Mp4Ilst, &key).and_then(property_key) {
            raw.tags.entry(property).or_insert_with(|| values.clone());
        }
        raw.raw.entry(key).or_default().extend(values);
    }
}
