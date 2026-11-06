//! Golden cases for the FLAC STREAMINFO patcher — `core/ffmpeg/flac_streaminfo_test.go`
//! shapes, emitted by the Python oracle mirror of `setFLACTotalSamples` (provenance
//! recorded with the fixture; never regenerate the fixture from Rust logic).
//! TSV lines: name <TAB> duration <TAB> input-hex <TAB> expected-hex.

use nd_ffmpeg::flac_patcher::{patch_flac_duration, set_flac_total_samples, FLAC_PREFIX_LEN};

fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => 0,
    }
}

fn hex_to_bytes(s: String) -> Vec<u8> {
    let bs = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i + 1 < bs.len() {
        out.push((nibble(bs[i]) << 4) | nibble(bs[i + 1]));
        i += 2;
    }
    out
}

#[test]
fn flac_streaminfo_goldens_match() {
    // cwd is the crate dir under `cargo test`, same convention as the #08
    // golden sidecars.
    let dump = match std::fs::read_to_string("tests/fixtures/flac_streaminfo.golden.tsv") {
        Ok(t) => t,
        Err(e) => panic!("fixture read: {}", e),
    };

    let mut cases = 0;
    for line in dump.split("\n") {
        if line.is_empty() {
            continue;
        }
        let cols: Vec<String> = line.split("\t").map(|c| c.to_string()).collect();
        assert!(cols.len() >= 4);
        let duration = match cols[1].to_string().parse::<f32>() {
            Ok(d) => d,
            Err(_) => panic!("bad duration column"),
        };
        let input = hex_to_bytes(cols[2].to_string());
        let expected = hex_to_bytes(cols[3].to_string());

        let got = patch_flac_duration(input.to_vec(), duration);
        assert_eq!(got, expected);
        cases += 1;
    }
    assert_eq!(cases, 13);
}

#[test]
fn flac_streaminfo_prefix_only() {
    // The set helper applied alone over the first case's 26-byte prefix
    // (`FLAC_PREFIX_LEN`), pinning the window the patcher commits to.
    let dump = match std::fs::read_to_string("tests/fixtures/flac_streaminfo.golden.tsv") {
        Ok(t) => t,
        Err(e) => panic!("fixture read: {}", e),
    };
    let lines: Vec<String> = dump.split("\n").map(|l| l.to_string()).collect();
    let cols: Vec<String> = lines[0].split("\t").map(|c| c.to_string()).collect();
    let input = hex_to_bytes(cols[2].to_string());
    let expected_full = hex_to_bytes(cols[3].to_string());
    let prefix = input[..FLAC_PREFIX_LEN].to_vec();
    let got = set_flac_total_samples(prefix.to_vec(), 1.0);
    assert_eq!(got, expected_full[..FLAC_PREFIX_LEN].to_vec());
}
