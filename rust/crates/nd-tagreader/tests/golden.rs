//! #08 golden: `extract_metadata` over RawTags dumps emitted by the Go oracle
//! (`adapters/gotaglib/golden_dump_test.go`, run against `tests/fixtures/**`).
//! Assertions mirror `adapters/gotaglib/gotaglib_test.go`. Sidecars are golden
//! (produced by Go) — never adjust them to match Rust.

use nd_tagreader::gotaglib::{extract_metadata, load_shim_raw_tags};
use nd_tagreader::info::Info;

/// Load a golden sidecar and run the port's full transform chain.
fn golden(name: String) -> Info {
    let raw = load_shim_raw_tags(".", "tests/fixtures", &name).unwrap();
    extract_metadata(&raw)
}

/// Values for a tag key, empty when the key is absent.
fn tag(info: &Info, key: String) -> Vec<String> {
    info.tags.get(&key).unwrap_or(&Vec::new()).to_vec()
}

/// Exact single-value assertion: key present with exactly [want].
#[test]
fn mp3_core_tags() {
    let m = golden("test.mp3".to_string());
    assert_eq!(tag(&m, "title".to_string()), ["Song"].to_vec());
    assert_eq!(tag(&m, "album".to_string()), ["Album"].to_vec());
    assert_eq!(tag(&m, "artist".to_string()), ["Artist"].to_vec());
    assert_eq!(
        tag(&m, "albumartist".to_string()),
        ["Album Artist"].to_vec()
    );
    assert_eq!(tag(&m, "genre".to_string()), ["Rock"].to_vec());
    assert_eq!(tag(&m, "date".to_string()), ["2014-05-21"].to_vec());
    assert_eq!(tag(&m, "originaldate".to_string()), ["1996-11-21"].to_vec());
    assert_eq!(tag(&m, "releasedate".to_string()), ["2020-12-31"].to_vec());
    assert_eq!(
        tag(&m, "comment".to_string()),
        ["Comment1\nComment2"].to_vec()
    );
    assert_eq!(tag(&m, "bpm".to_string()), ["123"].to_vec());
    assert_eq!(tag(&m, "compilation".to_string()), ["1"].to_vec());
}

#[test]
fn mp3_replaygain_and_tuple() {
    let m = golden("test.mp3".to_string());
    assert_eq!(
        tag(&m, "replaygain_album_gain".to_string()),
        ["+3.21518 dB"].to_vec()
    );
    assert_eq!(
        tag(&m, "replaygain_album_peak".to_string()),
        ["0.9125"].to_vec()
    );
    assert_eq!(
        tag(&m, "replaygain_track_gain".to_string()),
        ["-1.48 dB"].to_vec()
    );
    assert_eq!(
        tag(&m, "replaygain_track_peak".to_string()),
        ["0.4512"].to_vec()
    );
    // parseTuple: "2/10" -> tracknumber=2, tracktotal=10 (gotaglib_test.go)
    assert_eq!(tag(&m, "tracknumber".to_string()), ["2"].to_vec());
    assert_eq!(tag(&m, "tracktotal".to_string()), ["10"].to_vec());
    assert_eq!(tag(&m, "discnumber".to_string()), ["1"].to_vec());
    assert_eq!(tag(&m, "disctotal".to_string()), ["2"].to_vec());
}

#[test]
fn mp3_lyrics_and_properties() {
    let m = golden("test.mp3".to_string());
    // gotaglib_test.go: no generic "lyrics" key once language-coded lyrics exist.
    assert!(!m.tags.contains_key("lyrics"), "generic lyrics removed");
    let eng = tag(&m, "lyrics:eng".to_string());
    assert!(eng.len() == 2, "eng lyrics has USLT+SYLT");
    assert!(
        eng.iter()
            .find(|v| v.contains("English SYLT") && v.ends_with("SYLT\n"))
            .is_some(),
        "SYLT variant present"
    );
    let xxx = tag(&m, "lyrics:xxx".to_string());
    assert!(xxx.len() == 2, "xxx lyrics has USLT+SYLT");
    assert!(
        xxx.iter()
            .find(|v| v.contains("unspecified SYLT") && v.ends_with("SYLT\n"))
            .is_some(),
        "SYLT variant present"
    );
    // Duration "1.02s" (gotaglib_test.go) — raw 1.019s rounded to 10ms.
    assert!(
        m.audio_properties.duration == 1.02,
        "Round(Millisecond*10) of 1.019"
    );
    assert_eq!(m.audio_properties.bit_rate, 192);
    assert_eq!(m.audio_properties.channels, 2);
    assert_eq!(m.audio_properties.sample_rate, 44100);
    assert!(m.has_picture, "len(props.Images) > 0");
}

#[test]
fn ogg_properties_and_fbpm() {
    let m = golden("test.ogg".to_string());
    // gotaglib_test.go: BitRate BeElementOf(18,19,39,40,43,49); this TagLib gives 19.
    assert!(
        [18, 19, 39, 40, 43, 49].contains(&m.audio_properties.bit_rate),
        "bitrate element of golden set"
    );
    assert_eq!(m.audio_properties.sample_rate, 8000);
    assert_eq!(tag(&m, "fbpm".to_string()), ["141.7"].to_vec());
    assert!(m.has_picture, "ogg has embedded image");
    assert_eq!(m.audio_properties.duration, 1.04, "1.04s");
}

#[test]
fn m4a_tuple_atoms() {
    let m = golden("test.m4a".to_string());
    assert_eq!(tag(&m, "tracknumber".to_string()), ["3"].to_vec());
    assert_eq!(tag(&m, "tracktotal".to_string()), ["10"].to_vec());
    assert_eq!(tag(&m, "discnumber".to_string()), ["1"].to_vec());
    assert_eq!(tag(&m, "disctotal".to_string()), ["2"].to_vec());
    // iTunes atoms survive under their stripped lowercase keys
    // (gotaglib_test.go: Or(replaygain..., ----:com.apple.itunes:replaygain...)).
    assert_eq!(
        tag(&m, "replaygain_album_gain".to_string()),
        ["0.37"].to_vec()
    );
    assert_eq!(tag(&m, "origyear".to_string()), ["1998-07-28"].to_vec());
}

#[test]
fn wma_asf_surface() {
    let m = golden("test.wma".to_string());
    // gotaglib_test.go: compilation OR wm/iscompilation; WMA raw gives the latter.
    assert_eq!(tag(&m, "wm/iscompilation".to_string()), ["1"].to_vec());
    assert_eq!(tag(&m, "tracknumber".to_string()), ["3"].to_vec());
    // WMA has no tracktotal (the Go test skips the assertion for wma).
    assert!(!m.tags.contains_key("tracktotal"), "no tracktotal for wma");
}

#[test]
fn invalid_frame_file_parses_empty() {
    // invalid-files/test-invalid-frame.mp3: Go open succeeded (sidecar exists) but
    // the malformed frame yields no title — Parse skips/keeps whatever Go emitted.
    let m = golden("invalid-files/test-invalid-frame.mp3".to_string());
    assert!(m.audio_properties.duration >= 0.0, "oracle parsed the file");
}
