//! Edge cases in the Go adapter's post-transform, independent of the parser.

use nd_tagreader::gotaglib::{extract_metadata, RawTags};

fn raw() -> RawTags {
    let mut raw: RawTags =
        serde_json::from_str(include_str!("fixtures/test.mp3.tags.json")).unwrap();
    raw.tags.clear();
    raw.raw.clear();
    raw
}

#[test]
fn tuple_retains_only_first_number_even_without_slash() {
    // adapters/gotaglib/gotaglib.go:160-165
    let mut raw = raw();
    raw.tags
        .insert("TRACKNUMBER".into(), vec!["3".into(), "4".into()]);
    let info = extract_metadata(&raw);
    assert_eq!(info.tags["tracknumber"], ["3"]);
}

#[test]
fn malformed_tuple_does_not_replace_a_separate_total() {
    // Go strings.Split produces three parts; only exactly two replace total.
    let mut raw = raw();
    raw.tags
        .insert("TRACKNUMBER".into(), vec!["2/10/99".into()]);
    raw.tags.insert("TRACKTOTAL".into(), vec!["42".into()]);
    let info = extract_metadata(&raw);
    assert_eq!(info.tags["tracknumber"], ["2"]);
    assert_eq!(info.tags["tracktotal"], ["42"]);
}

#[test]
fn unknown_format_does_not_apply_id3_raw_overlay() {
    // adapters/gotaglib/gotaglib.go:181-190 (no switch default overlay).
    let mut raw = raw();
    raw.format.clear();
    raw.tags.insert("LYRICS".into(), vec!["Generic".into()]);
    raw.raw.insert("USLT:eng".into(), vec!["Not ID3".into()]);
    let info = extract_metadata(&raw);
    assert_eq!(info.tags["lyrics:xxx"], ["Generic"]);
    assert!(!info.tags.contains_key("lyrics:eng"));
}

#[test]
fn empty_lyrics_are_not_moved_to_language_key() {
    // adapters/gotaglib/gotaglib.go:172-176 checks len(lyrics) > 0.
    let mut raw = raw();
    raw.tags.insert("LYRICS".into(), vec![]);
    let info = extract_metadata(&raw);
    assert!(info.tags["lyrics"].is_empty());
    assert!(!info.tags.contains_key("lyrics:xxx"));
}
