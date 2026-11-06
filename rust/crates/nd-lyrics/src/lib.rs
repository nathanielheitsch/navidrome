//! # nd-lyrics — Rust port of the Go lyrics models and parsers
//!
//! Owned by issues #09 / #22 (`docs/rust-port/issues.md` Part B):
//! - [`model`] — `model/lyrics.go`: `Cue`, `Agent`, `Line`, `Lyrics`,
//!   `LyricList` DTOs and their pure helpers.
//! - [`lrc`] — `model/lyrics_lrc.go`: LRC / Enhanced LRC parsing.
//! - [`srt`] — `model/lyrics_srt.go`: SRT subtitle parsing.
//! - [`ttml`] — `model/lyrics_ttml.go`: TTML parsing (word timing, agents,
//!   translations/transliterations).
//! - [`lyricsfile`] — `model/lyrics_lyricsfile.go`: LRCLIB Lyricsfile YAML.
//! - [`parse`] — `model/lyrics_parse.go`: the single entry point with
//!   content sniffing and the plain-text fallback floor.
//! - [`normalize`] — `model/lyrics_normalize.go`: cue-end and line-timing
//!   normalization.
//!
//! Fetch/merge (`core/lyrics/**`) is #22 and lands here later.

pub mod lrc;
pub mod lyricsfile;
pub mod model;
pub mod normalize;
pub mod parse;
pub mod srt;
pub mod ttml;

pub use model::{Agent, Cue, Line, LyricList, Lyrics};
pub use parse::parse_lyrics;

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use std::path::Path;

    /// Reads a fixture from the repo-root `tests/fixtures/` tree (Go sidecar
    /// fixtures used by `core/lyrics/sources_test.go`).
    fn fixture(name: &str) -> Vec<u8> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|p| p.join("go.mod").is_file())
            .unwrap()
            .to_path_buf();
        std::fs::read(root.join("tests/fixtures").join(name)).expect("fixture present")
    }

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    /// Go core/lyrics/sources_test.go: LRC files with UTF-8 BOM marker
    /// (issue #4631). `ParseLyrics` must strip the BOM before parsing so the
    /// first `[00:00.00]` line still syncs.
    #[test]
    fn handles_lrc_files_with_utf8_bom_marker_issue_4631() {
        let raw = fixture("bom-test.lrc");
        assert!(
            raw.starts_with("\u{feff}".as_bytes()),
            "fixture must start with a BOM"
        );

        let list = parse_lyrics(".lrc", "xxx", &raw).unwrap();
        assert_eq!(list.len(), 1);
        assert!(
            list[0].synced,
            "Lyrics with BOM marker should be recognized as synced"
        );
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].start, p(0));
        assert!(list[0].line[0].value.contains("作曲"));
    }

    /// Go core/lyrics/sources_test.go: TTML files with UTF-8 BOM marker.
    #[test]
    fn handles_ttml_files_with_utf8_bom_marker() {
        let raw = fixture("bom-test.ttml");
        assert!(
            raw.starts_with("\u{feff}".as_bytes()),
            "fixture must start with a BOM"
        );

        let list = parse_lyrics(".ttml", "xxx", &raw).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, "main");
        assert!(list[0].synced);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].start, p(0));
        assert_eq!(list[0].line[0].value, "BOM test line");
    }

    /// Go core/lyrics/sources_test.go: UTF-16 LE encoded LRC files. The
    /// nd-utils `utf8_read_file` port (ioutils.UTF8ReadFile) handles the
    /// UTF-16 → UTF-8 conversion; here we verify it end-to-end with the
    /// BOM-stripping parse.
    #[test]
    fn handles_utf16_le_encoded_lrc_files() {
        let raw = nd_utils::ioutils::utf8_read_file(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|p| p.join("go.mod").is_file())
                .unwrap()
                .join("tests/fixtures/bom-utf16-test.lrc"),
        )
        .expect("read fixture");

        let list = parse_lyrics(".lrc", "xxx", &raw).unwrap();
        assert_eq!(list.len(), 1);
        assert!(
            list[0].synced,
            "UTF-16 encoded lyrics should be recognized as synced"
        );
        assert_eq!(list[0].line.len(), 2);
        assert_eq!(list[0].line[0].start, p(18800));
        assert_eq!(list[0].line[0].value, "We're no strangers to love");
        assert_eq!(list[0].line[1].start, p(22801));
        assert_eq!(list[0].line[1].value, "You know the rules and so do I");
    }

    /// Go core/lyrics/sources_test.go: UTF-16 BE encoded TTML files.
    #[test]
    fn handles_utf16_be_encoded_ttml_files() {
        let raw = nd_utils::ioutils::utf8_read_file(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find(|p| p.join("go.mod").is_file())
                .unwrap()
                .join("tests/fixtures/bom-utf16-test.ttml"),
        )
        .expect("read fixture");

        let list = parse_lyrics(".ttml", "xxx", &raw).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, "main");
        assert!(list[0].synced);
        assert_eq!(list[0].line.len(), 2);
        assert_eq!(list[0].line[0].start, p(18800));
        assert_eq!(list[0].line[0].value, "UTF16 line one");
        assert_eq!(list[0].line[1].start, p(22801));
        assert_eq!(list[0].line[1].value, "UTF16 line two");
    }

    /// Go core/lyrics/sources_test.go (DescribeTable): all `tests/fixtures/
    /// lyrics/auld-lang-syne.*` variants parse, with the expected synced
    /// flag per format.
    #[test]
    fn auld_lang_syne_fixtures_parse_with_expected_synced_flags() {
        let cases = [
            (".lrc", "auld-lang-syne.lrc", true),
            (".elrc", "auld-lang-syne.elrc", true),
            (".txt", "auld-lang-syne.txt", false),
            (".srt", "auld-lang-syne.srt", true),
            (".ttml", "auld-lang-syne.ttml", true),
            (".yaml", "auld-lang-syne.yaml", true),
        ];
        for (suffix, file, expect_synced) in cases {
            let raw = fixture(&format!("lyrics/{file}"));
            let list = parse_lyrics(suffix, "xxx", &raw).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(!list.is_empty(), "{file}");
            assert!(!list[0].line.is_empty(), "{file}");
            assert_eq!(list[0].synced, expect_synced, "{file}");
        }
    }

    /// Normalize round-trip idempotence: normalizing an already-normalized
    /// lyric list must be a no-op. This is the Rust counterpart of the
    /// normalize contract exercised across the Go tests (normalizeLyrics is
    /// applied by every parser; re-applying must not change output).
    #[test]
    fn normalize_is_idempotent_across_all_fixture_formats() {
        let files = [
            ("lyrics/auld-lang-syne.lrc", ".lrc"),
            ("lyrics/auld-lang-syne.elrc", ".lrc"),
            ("lyrics/auld-lang-syne.srt", ".srt"),
            ("lyrics/auld-lang-syne.ttml", ".ttml"),
            ("lyrics/auld-lang-syne.yaml", ".yaml"),
            ("bom-test.lrc", ".lrc"),
            ("bom-test.ttml", ".ttml"),
        ];
        for (file, suffix) in files {
            let raw = fixture(file);
            let list = parse_lyrics(suffix, "xxx", &raw).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(!list.is_empty(), "{file}");

            // Re-normalize each parsed track; output must be identical.
            for l in &list {
                let once = normalize::normalize_lyrics(l.clone());
                let twice = normalize::normalize_lyrics(once.clone());
                assert_eq!(once, twice, "{file}: normalize must be idempotent");
            }
        }
    }
}
