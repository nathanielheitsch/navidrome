//! Format-specific regression tests over actual media. ID3 frames below are
//! encoded independently of Lofty, avoiding a writer/reader tautology.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::TagExt;
use nd_tagreader::{get_extractor, read_metadata, Info};

static SERIAL: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nd-tagreader-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn copy(&self, extension: &str) -> PathBuf {
        let path = self.0.join(format!("track.{extension}"));
        fs::copy(fixture(extension), &path).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/real")
        .join(format!("known.{extension}"))
}

fn values<'a>(info: &'a Info, key: &str) -> Vec<&'a str> {
    info.tags
        .get(key)
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default()
}

fn syncsafe(size: usize) -> [u8; 4] {
    [
        (size >> 21 & 127) as u8,
        (size >> 14 & 127) as u8,
        (size >> 7 & 127) as u8,
        (size & 127) as u8,
    ]
}

fn frame(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut result = id.to_vec();
    result.extend(syncsafe(payload.len()));
    result.extend([0, 0]);
    result.extend(payload);
    result
}

fn replace_id3(path: &Path, frames: &[Vec<u8>]) {
    let original = fs::read(path).unwrap();
    assert_eq!(&original[..3], b"ID3");
    let size = original[6..10]
        .iter()
        .fold(0, |size, byte| (size << 7) | usize::from(*byte));
    let payload = frames.concat();
    let mut result = b"ID3\x04\0\0".to_vec();
    result.extend(syncsafe(payload.len()));
    result.extend(payload);
    result.extend(&original[10 + size..]);
    fs::write(path, result).unwrap();
}

#[test]
fn id3_multivalues_comments_lyrics_roles_and_custom_tags() {
    let dir = TempDir::new();
    let path = dir.copy("mp3");
    let mut sylt = b"\x03eng\x02\x01\0First\0".to_vec();
    sylt.extend(0_u32.to_be_bytes());
    sylt.extend(b"Second\0");
    sylt.extend(2500_u32.to_be_bytes());
    replace_id3(
        &path,
        &[
            frame(b"TIT2", b"\x03Real title"),
            frame(b"TPE1", b"\x03Alice\0Bob / Carol"),
            frame(b"TCON", b"\x03Rock\0Jazz"),
            frame(b"TRCK", b"\x032/10"),
            frame(b"TPOS", b"\x031/2"),
            frame(b"COMM", b"\x03eng\0Line one\nLine two"),
            frame(b"COMM", b"\x03engNote\0Named comment"),
            frame(b"USLT", b"\x03eng\0English lyrics"),
            frame(b"USLT", b"\x03fraTranslation\0French lyrics"),
            frame(b"USLT", b"\x03xxx\0Unspecified lyrics"),
            frame(b"SYLT", &sylt),
            frame(
                b"TXXX",
                b"\x03LYRICS\0Generic must not replace coded lyrics",
            ),
            frame(b"TXXX", b"\x03MUSICBRAINZ_PRODUCER_ID\0id-one\0id-two"),
            frame(b"TXXX", b"\x03REPLAYGAIN_TRACK_GAIN\0-1.48 dB"),
            frame(
                b"TIPL",
                b"\x03engineer\0Engineer One\0engineer\0Engineer Two\0mix\0Mixer One",
            ),
            frame(b"TMCL", b"\x03guitar\0Player One\0flute\0Player Two"),
        ],
    );
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "title"), ["Real title"]);
    assert_eq!(values(&info, "artist"), ["Alice", "Bob / Carol"]);
    assert_eq!(values(&info, "genre"), ["Rock", "Jazz"]);
    assert_eq!(values(&info, "tracknumber"), ["2"]);
    assert_eq!(values(&info, "tracktotal"), ["10"]);
    assert_eq!(values(&info, "disctotal"), ["2"]);
    assert_eq!(values(&info, "comment"), ["Line one\nLine two"]);
    assert_eq!(values(&info, "comment:note"), ["Named comment"]);
    assert_eq!(
        values(&info, "lyrics:eng"),
        ["[00:00.00]First\n[00:02.50]Second\n", "English lyrics"]
    );
    assert_eq!(values(&info, "lyrics:fra"), ["French lyrics"]);
    assert_eq!(values(&info, "lyrics:xxx"), ["Unspecified lyrics"]);
    assert!(!info.tags.contains_key("lyrics"));
    assert_eq!(
        values(&info, "musicbrainz_producer_id"),
        ["id-one", "id-two"]
    );
    assert_eq!(values(&info, "replaygain_track_gain"), ["-1.48 dB"]);
    assert_eq!(values(&info, "engineer"), ["Engineer One", "Engineer Two"]);
    assert_eq!(values(&info, "mixer"), ["Mixer One"]);
    assert_eq!(values(&info, "performer:guitar"), ["Player One"]);
    assert_eq!(values(&info, "performer:flute"), ["Player Two"]);
    assert!(!info.tags.contains_key("tmcl"));
}

#[test]
fn vorbis_preserves_multiple_values_custom_keys_and_comments() {
    let dir = TempDir::new();
    let path = dir.copy("flac");
    let mut tag = VorbisComments::new();
    for (key, value) in [
        ("ARTIST", "Alice"),
        ("ARTIST", "Bob"),
        ("COMMENT", "Comment one\nComment two"),
        ("DESCRIPTION", "Different description"),
        ("LYRICS", "First version"),
        ("LYRICS", "Second version"),
        ("MUSICBRAINZ_PRODUCER_ID", "one"),
        ("MUSICBRAINZ_PRODUCER_ID", "two"),
        ("REPLAYGAIN_TRACK_GAIN", "-2.5 dB"),
    ] {
        tag.push(key.into(), value.into());
    }
    tag.save_to_path(&path, WriteOptions::default()).unwrap();
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "artist"), ["Alice", "Bob"]);
    assert_eq!(values(&info, "comment"), ["Comment one\nComment two"]);
    assert_eq!(values(&info, "description"), ["Different description"]);
    assert_eq!(
        values(&info, "lyrics:xxx"),
        ["First version", "Second version"]
    );
    assert_eq!(values(&info, "musicbrainz_producer_id"), ["one", "two"]);
    assert_eq!(values(&info, "replaygain_track_gain"), ["-2.5 dB"]);
}

#[test]
fn mp4_normalized_property_wins_over_raw_itunes_atom() {
    let dir = TempDir::new();
    let path = dir.copy("m4a");
    let mut tag = Ilst::new();
    for (ident, data) in [
        (
            AtomIdent::Fourcc(*b"\xa9nam"),
            vec![AtomData::UTF8("Canonical".into())],
        ),
        (
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "TITLE".into(),
            },
            vec![AtomData::UTF8("Must not override".into())],
        ),
        (
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "CUSTOM_ROLE".into(),
            },
            vec![AtomData::UTF8("one".into()), AtomData::UTF8("two".into())],
        ),
        (
            AtomIdent::Freeform {
                mean: "example.test".into(),
                name: "PRIVATE".into(),
            },
            vec![AtomData::UTF8("private".into())],
        ),
    ] {
        tag.insert(Atom::from_collection(ident, data).unwrap());
    }
    tag.save_to_path(&path, WriteOptions::default()).unwrap();
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "title"), ["Canonical"]);
    assert_eq!(values(&info, "custom_role"), ["one", "two"]);
    assert_eq!(values(&info, "----:example.test:private"), ["private"]);
}

#[test]
fn id3v2_does_not_fill_missing_fields_from_stale_id3v1() {
    let dir = TempDir::new();
    let path = dir.copy("mp3");
    replace_id3(&path, &[frame(b"TIT2", b"\x03New title")]);
    let mut data = fs::read(&path).unwrap();
    let mut id3v1 = [0_u8; 128];
    id3v1[..3].copy_from_slice(b"TAG");
    id3v1[3..12].copy_from_slice(b"Old title");
    id3v1[33..43].copy_from_slice(b"Old artist");
    data.extend(id3v1);
    fs::write(&path, data).unwrap();
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "title"), ["New title"]);
    assert!(values(&info, "artist").is_empty());
}

#[test]
fn embedded_pictures_in_all_four_containers() {
    use lofty::picture::{Picture, PictureType};
    // Valid 2x2 PNG, generated independently of Lofty by ffmpeg.
    let png = include_bytes!("fixtures/real/cover.png");
    for extension in ["mp3", "flac", "m4a", "ogg"] {
        let dir = TempDir::new();
        let path = dir.copy(extension);
        let mut file = lofty::read_from_path(&path).unwrap();
        let mut picture = Picture::from_reader(&mut &png[..]).unwrap();
        picture.set_pic_type(PictureType::CoverFront);
        file.primary_tag_mut().unwrap().push_picture(picture);
        use lofty::file::AudioFile;
        file.save_to_path(&path, WriteOptions::default()).unwrap();
        assert!(read_metadata(path).unwrap().has_picture, "{extension}");
    }
}

#[test]
fn sidecars_cannot_create_or_override_a_track() {
    let dir = TempDir::new();
    let path = dir.copy("mp3");
    let sidecar = include_bytes!("fixtures/test.mp3.tags.json");
    fs::write(path.with_extension("mp3.tags.json"), sidecar).unwrap();
    fs::write(dir.0.join("missing.mp3.tags.json"), sidecar).unwrap();
    fs::write(dir.0.join("broken.mp3"), b"not audio").unwrap();
    fs::write(dir.0.join("broken.mp3.tags.json"), sidecar).unwrap();
    let extractor = get_extractor("taglib").unwrap()(dir.0.to_str().unwrap().into(), "".into());
    let results = extractor.parse(vec![
        "track.mp3".into(),
        "missing.mp3".into(),
        "broken.mp3".into(),
    ]);
    assert_eq!(results.len(), 1);
    assert_eq!(values(&results["track.mp3"], "title"), ["Real café"]);
    assert!(read_metadata(dir.0.join("broken.mp3")).is_err());
    assert!(read_metadata(dir.0.join("missing.mp3")).is_err());
}

#[test]
fn content_detection_handles_unknown_extensions() {
    let dir = TempDir::new();
    let path = dir.0.join("audio.bin");
    fs::copy(fixture("flac"), &path).unwrap();
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "title"), ["Real café"]);
    assert!(info.audio_properties.codec.is_empty());
}

#[test]
fn unsupported_asf_is_an_error_not_a_fabricated_success() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/test.wma");
    assert!(path.exists());
    assert!(read_metadata(path).is_err());
}

#[test]
fn batch_paths_cannot_escape_the_filesystem_root() {
    let dir = TempDir::new();
    let outside = dir.copy("mp3");
    fs::create_dir(dir.0.join("root")).unwrap();
    let extractor =
        get_extractor("taglib").unwrap()(dir.0.join("root").to_str().unwrap().into(), "".into());
    let result = extractor.parse(vec![
        "../track.mp3".into(),
        outside.to_str().unwrap().into(),
        "".into(),
    ]);
    assert!(result.is_empty());
}

#[test]
fn tuple_totals_override_separate_total_properties() {
    let dir = TempDir::new();
    let path = dir.copy("mp3");
    replace_id3(
        &path,
        &[
            frame(b"TRCK", b"\x032/10"),
            frame(b"TPOS", b"\x031/2"),
            frame(b"TXXX", b"\x03TRACKTOTAL\0\x39\x39"),
            frame(b"TXXX", b"\x03DISCTOTAL\0\x39\x39"),
        ],
    );
    let info = read_metadata(path).unwrap();
    assert_eq!(values(&info, "tracktotal"), ["10"]);
    assert_eq!(values(&info, "disctotal"), ["2"]);
}

#[test]
fn untagged_mp3_still_has_audio_properties() {
    let dir = TempDir::new();
    let path = dir.copy("mp3");
    let expected = read_metadata(&path).unwrap().audio_properties;
    replace_id3(&path, &[]);
    let info = read_metadata(path).unwrap();
    assert!(info.tags.is_empty());
    assert_eq!(info.audio_properties, expected);
    assert_eq!(info.audio_properties.sample_rate, 44100);
    assert!(!info.has_picture);
}
