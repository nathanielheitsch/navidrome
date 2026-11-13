//! Read committed, ffmpeg-generated audio, never sidecars. Generation is
//! explicit (`fixtures/real/generate.sh`), so these tests cannot silently skip.

use std::path::PathBuf;

use nd_tagreader::{get_extractor, read_metadata, Info};

fn fixture(extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/real")
        .join(format!("known.{extension}"))
}

fn values<'a>(info: &'a Info, key: &str) -> Vec<&'a str> {
    info.tags
        .get(key)
        .map(|values| values.iter().map(String::as_str).collect())
        .unwrap_or_default()
}

fn check_file(extension: &str, codec: &str) {
    let info = read_metadata(fixture(extension)).unwrap();
    assert_eq!(values(&info, "title"), ["Real café"]);
    assert_eq!(values(&info, "artist"), ["Artist One"]);
    assert_eq!(values(&info, "album"), ["Synthetic Album"]);
    assert_eq!(values(&info, "albumartist"), ["Album Artist"]);
    assert_eq!(values(&info, "genre"), ["Rock"]);
    assert_eq!(values(&info, "tracknumber"), ["2"]);
    assert_eq!(values(&info, "tracktotal"), ["10"]);
    assert_eq!(values(&info, "discnumber"), ["1"]);
    assert_eq!(values(&info, "disctotal"), ["2"]);
    // ffmpeg writes Vorbis DESCRIPTION for its comment option. Preserve the
    // raw key; #16 applies comment/description aliases (resources/mappings.yaml:117-118).
    let comment = if matches!(extension, "flac" | "ogg") {
        "description"
    } else {
        "comment"
    };
    assert_eq!(values(&info, comment), ["Comment one\nComment two"]);
    assert_eq!(values(&info, "date"), ["2024-03-02"]);
    assert_eq!(
        values(&info, "lyrics:xxx"),
        ["[00:00.00]Real lyrics\n[00:00.20]Second line"]
    );
    assert!(!info.tags.contains_key("lyrics"));
    let props = info.audio_properties;
    assert!((0.39..=0.46).contains(&props.duration), "{props:?}");
    if extension == "ogg" {
        // The native ffmpeg Vorbis encoder leaves nominal/min/max rates unset.
        // Lofty cannot infer an audio bitrate from that header; 0 means unknown.
        assert_eq!(props.bit_rate, 0);
    } else {
        assert!(props.bit_rate > 0, "{props:?}");
    }
    assert_eq!(props.channels, 2);
    assert_eq!(props.sample_rate, 44100);
    assert_eq!(props.codec, codec);
    if extension == "flac" {
        assert_eq!(props.bit_depth, 16);
    }
    assert!(!info.has_picture);
}

#[test]
fn ffmpeg_mp3() {
    check_file("mp3", "MP3");
}

#[test]
fn ffmpeg_flac() {
    check_file("flac", "");
}

#[test]
fn ffmpeg_m4a() {
    check_file("m4a", "AAC");
}

#[test]
fn ffmpeg_ogg() {
    check_file("ogg", "");
}

#[test]
fn default_registry_reads_real_media_and_skips_missing_files() {
    let constructor = get_extractor("taglib").unwrap();
    let extractor = constructor(
        env!("CARGO_MANIFEST_DIR").into(),
        "tests/fixtures/real".into(),
    );
    let result = extractor.parse(vec!["known.mp3".into(), "missing.mp3".into()]);
    assert_eq!(result.len(), 1);
    assert_eq!(values(&result["known.mp3"], "title"), ["Real café"]);
    assert_eq!(extractor.version(), "lofty/0.25.5");
    assert!(get_extractor("lofty").is_ok());
}
