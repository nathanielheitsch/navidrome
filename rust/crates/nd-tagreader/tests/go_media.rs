//! End-to-end comparison against Go: read the actual media behind the existing
//! dumps. The few explicitly enumerated backend differences are documented in
//! README.md; every other expected tag/value is checked, not just core fields.

use std::path::PathBuf;

use nd_tagreader::gotaglib::{extract_metadata, load_shim_raw_tags};
use nd_tagreader::read_metadata;

#[test]
fn actual_media_matches_go_oracles_except_documented_backend_gaps() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let media_root = crate_root.join("../../../tests/fixtures");
    for name in [
        "test.mp3",
        "test.flac",
        "test.m4a",
        "test.ogg",
        "test.opus",
        "test.wv",
        "test.wav",
        "test.aiff",
    ] {
        let expected = extract_metadata(
            &load_shim_raw_tags(crate_root.to_str().unwrap(), "tests/fixtures", name).unwrap(),
        );
        let actual = read_metadata(media_root.join(name)).unwrap();
        for (key, expected_value) in &expected.tags {
            if name == "test.m4a" {
                // Redundant raw binary-atom aliases/placeholders, not lost fields:
                // canonical track/disc values and HasPicture are checked below.
                if ["covr", "disk:num", "disk:total", "trkn:num", "trkn:total"]
                    .contains(&key.as_str())
                {
                    assert!(!actual.tags.contains_key(key), "{name} {key}");
                    continue;
                }
                // This fixture has two malformed empty-name freeform atoms;
                // Lofty preserves both instead of TagLib retaining only one.
                if key.is_empty() {
                    assert_eq!(expected_value, &["Test"]);
                    assert_eq!(actual.tags[key], ["Test", "test"]);
                    continue;
                }
            }
            assert_eq!(actual.tags.get(key), Some(expected_value), "{name} {key}");
        }
        let want = expected.audio_properties;
        let got = actual.audio_properties;
        assert_eq!(got.duration, want.duration, "{name}");
        assert_eq!(got.channels, want.channels, "{name}");
        assert_eq!(got.sample_rate, want.sample_rate, "{name}");
        assert_eq!(got.codec, want.codec, "{name}");
        assert_eq!(actual.has_picture, expected.has_picture, "{name}");
        if name == "test.m4a" {
            assert_eq!((got.bit_depth, want.bit_depth), (0, 16));
        } else {
            assert_eq!(got.bit_depth, want.bit_depth, "{name}");
        }
        match name {
            "test.flac" => assert_eq!((got.bit_rate, want.bit_rate), (103, 104)),
            "test.ogg" => assert_eq!((got.bit_rate, want.bit_rate), (16, 19)),
            _ => assert_eq!(got.bit_rate, want.bit_rate, "{name}"),
        }
    }
}
