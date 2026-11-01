//! #09 — port of `model/lyrics_parse.go`: the single entry point for parsing
//! lyrics. A known suffix routes to that format's parser; an empty or
//! `"auto"` suffix content-sniffs. Either way, a structured parser that does
//! not match falls back to the LRC/plain-text floor.

use crate::lrc::parse_lrc;
use crate::lyricsfile::parse_lyricsfile;
use crate::model::LyricList;
use crate::srt::parse_srt;
use crate::ttml::parse_ttml;

/// `stripBOM`.
fn strip_bom(contents: &[u8]) -> &[u8] {
    contents
        .strip_prefix("\u{feff}".as_bytes())
        .unwrap_or(contents)
}

/// `ParseLyrics`: the single entry point for parsing lyrics. A known suffix
/// routes to that format's parser; an empty or "auto" suffix content-sniffs.
/// Either way, a structured parser that does not match falls back to the
/// LRC/plain-text floor.
///
/// (Go takes a `context.Context` for log attribution; the Rust port drops
/// logging to a trace macro, which carries file/line attribution.)
pub fn parse_lyrics(suffix: &str, lang: &str, contents: &[u8]) -> Result<LyricList, String> {
    let contents = strip_bom(contents);
    let suffix = suffix.to_lowercase();
    let sniff = suffix.is_empty() || suffix == "auto";

    // Sniffing tries every format in order; a known suffix selects just its
    // own. Unmatched suffixes leave no candidates, so parse_first_match
    // falls to plain.
    let mut candidates: Vec<Parser> = Vec::new();
    let formats: &[(&[&str], Parser)] = &[
        (&[".ttml"], parse_ttml_parser),
        (&[".srt"], parse_srt_parser),
        (&[".yaml", ".yml"], parse_lyricsfile_parser),
    ];
    for (suffixes, parse) in formats {
        if sniff || suffixes.contains(&suffix.as_str()) {
            candidates.push(*parse);
        }
    }
    parse_first_match(sniff, lang, contents, &candidates)
}

type Parser = fn(&str, &[u8]) -> Result<Option<LyricList>, String>;

fn parse_ttml_parser(lang: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    parse_ttml(lang, contents)
}

fn parse_srt_parser(lang: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    parse_srt(lang, contents)
}

fn parse_lyricsfile_parser(lang: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    parse_lyricsfile(lang, contents)
}

fn parse_first_match(
    sniff: bool,
    lang: &str,
    contents: &[u8],
    candidates: &[Parser],
) -> Result<LyricList, String> {
    for parse in candidates {
        match parse(lang, contents) {
            Ok(Some(list)) if !list.is_empty() => return Ok(list),
            Err(err) => {
                // Go logs a probe miss at trace while sniffing (expected
                // control flow) and warns under an explicit suffix. The Rust
                // port has no logging facade wired yet (#12 owns it); the
                // observable parse outcomes are identical without it.
                let _ = (sniff, err);
            }
            _ => {}
        }
    }
    plain_lrc(lang, contents)
}

fn plain_lrc(lang: &str, contents: &[u8]) -> Result<LyricList, String> {
    let text = std::str::from_utf8(contents)
        .map_err(|e| format!("parsing lyrics: {e}"))?
        .to_string();
    let lyric = parse_lrc(lang, &text).map_err(|e| format!("parsing lyrics: {e}"))?;
    if lyric.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![lyric])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Lyrics;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    // Port of model/lyrics_parse_test.go
    #[test]
    fn known_suffix_routes_to_the_matching_parser() {
        let cases: &[(&str, &str, bool, &str)] = &[
            (".lrc", "[00:01.00]lrc line", true, "lrc line"),
            (".txt plain", "plain line", false, "plain line"),
            (".srt", "1\n00:00:01,000 --> 00:00:02,000\nsrt line\n", true, "srt line"),
            (".ttml", "<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"00:00.000\" end=\"00:01.000\">ttml line</p></div></body></tt>", true, "ttml line"),
            (".yaml", "version: \"1.0\"\nmetadata:\n  language: eng\nlines:\n  - text: yaml line\n    start_ms: 1000\n", true, "yaml line"),
        ];
        for (name, contents, want_synced, want_first) in cases {
            let list = parse_lyrics(name, "eng", contents.as_bytes()).unwrap();
            assert_eq!(list.len(), 1, "case {name}");
            assert_eq!(list[0].synced, *want_synced, "case {name}");
            assert_eq!(list[0].line[0].value, *want_first, "case {name}");
        }
    }

    #[test]
    fn empty_suffix_content_sniffs_ttml() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="00:00.000" end="00:01.000">auto ttml</p></div></body></tt>"#;
        let list = parse_lyrics("", "eng", ttml.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line[0].value, "auto ttml");
    }

    #[test]
    fn empty_suffix_content_sniffs_yaml() {
        let yaml = "version: \"1.0\"\nmetadata:\n  language: eng\nlines:\n  - text: auto yaml\n    start_ms: 1000\n";
        let list = parse_lyrics("auto", "eng", yaml.as_bytes()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line[0].value, "auto yaml");
    }

    #[test]
    fn falls_back_to_plain_text_when_a_known_suffix_fails_to_parse_structurally() {
        let list = parse_lyrics(".srt", "eng", b"not actually an srt file").unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].synced);
        assert_eq!(list[0].line[0].value, "not actually an srt file");
    }

    // The Go test also asserts trace/warn log behavior with file
    // attribution; no logging facade is wired into the Rust port yet (#12
    // owns it), so only the observable parsing outcomes are asserted here.

    #[test]
    fn parses_embedded_ttml_with_the_tag_language_as_the_default() {
        let content = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <head>
    <metadata>
      <ttm:agent xml:id="lead" ttm:type="person">
        <ttm:name>Lead Vocal</ttm:name>
      </ttm:agent>
    </metadata>
  </head>
  <body>
    <div>
      <p begin="00:00:01.000" end="00:00:03.000">
        <span begin="00:00:01.000" end="00:00:02.000" ttm:agent="lead">Hello </span><span begin="00:00:02.000" end="00:00:03.000" ttm:agent="lead">world</span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_lyrics("", "ENG", content.as_bytes()).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, "main");
        assert_eq!(list[0].lang, "eng");
        assert!(list[0].synced);
        assert_eq!(list[0].line[0].value, "Hello world");
    }

    #[test]
    fn preserves_embedded_ttml_translation_and_pronunciation_tracks() {
        let content = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal">
  <head>
    <metadata>
      <iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal">
        <translations>
          <translation xml:lang="es">
            <text for="L1">Hola</text>
          </translation>
        </translations>
        <transliterations>
          <transliteration xml:lang="ja-Latn">
            <text for="L1"><span begin="00:00:01.000" end="00:00:01.300" xmlns="http://www.w3.org/ns/ttml">ko</span><span begin="00:00:01.300" end="00:00:01.600" xmlns="http://www.w3.org/ns/ttml">nni</span></text>
          </transliteration>
        </transliterations>
      </iTunesMetadata>
    </metadata>
  </head>
  <body xml:lang="ja">
    <div>
      <p begin="00:00:01.000" end="00:00:02.000" itunes:key="L1">こんにちは</p>
    </div>
  </body>
</tt>"#;

        let list = parse_lyrics("", "eng", content.as_bytes()).unwrap();

        assert_eq!(list.len(), 3);
        assert_eq!(list[0].kind, "main");
        assert_eq!(list[0].lang, "ja");
        assert_eq!(list[0].line[0].value, "こんにちは");
        assert_eq!(list[1].kind, "translation");
        assert_eq!(list[1].lang, "es");
        assert_eq!(list[1].line[0].value, "Hola");
        assert_eq!(list[2].kind, "pronunciation");
        assert_eq!(list[2].lang, "ja-latn");
        assert_eq!(list[2].line[0].value, "konni");
        assert_eq!(list[2].line[0].cue.len(), 2);
    }

    #[test]
    fn parses_embedded_srt_with_the_tag_language() {
        let content = "1\n00:00:18,800 --> 00:00:22,800\nWe're from subtitles\n\n2\n00:00:22,801 --> 00:00:26,000\nAnother subtitle line";

        let list = parse_lyrics("", "POR", content.as_bytes()).unwrap();

        let expected = Lyrics {
            lang: "por".into(),
            line: vec![
                crate::model::Line {
                    start: p(18800),
                    end: p(22800),
                    value: "We're from subtitles".into(),
                    ..Default::default()
                },
                crate::model::Line {
                    start: p(22801),
                    end: p(26000),
                    value: "Another subtitle line".into(),
                    ..Default::default()
                },
            ],
            synced: true,
            ..Default::default()
        };
        assert_eq!(list, vec![expected]);
    }

    #[test]
    fn keeps_embedded_enhanced_lrc_cues() {
        let content = "[00:01.00]<00:01.00>Lead <00:01.50>words";

        let list = parse_lyrics("", "eng", content.as_bytes()).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "eng");
        assert!(list[0].synced);
        assert_eq!(list[0].line[0].value, "Lead words");
        assert_eq!(list[0].line[0].cue.len(), 2);
    }

    #[test]
    fn falls_back_to_plain_lyrics_when_embedded_ttml_is_invalid() {
        let content = "<tt xmlns=\"http://www.w3.org/ns/ttml\">\n  <body>\n    <p begin=\"not-a-time\">Broken</p>\n  </body>\n</tt>";

        let list = parse_lyrics("", "eng", content.as_bytes()).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "eng");
        assert!(!list[0].synced);
        assert!(!list[0].line.is_empty());
        let values: Vec<&str> = list[0].line.iter().map(|l| l.value.as_str()).collect();
        assert!(values.join("\n").contains("Broken"));
    }

    #[test]
    fn detects_a_lyricsfile_yaml_payload_via_content_sniffing() {
        let yaml = "version: \"1.0\"\nmetadata:\n  title: Song\n  language: eng\nlines:\n  - text: sniffed yaml line\n    start_ms: 1000\n";

        let list = parse_lyrics("", "eng", yaml.as_bytes()).unwrap();

        assert_eq!(list.len(), 1);
        assert!(list[0].synced);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].value, "sniffed yaml line");
    }
}
