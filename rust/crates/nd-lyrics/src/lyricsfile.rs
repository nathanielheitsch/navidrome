//! #09 — port of `model/lyrics_lyricsfile.go`: LRCLIB Lyricsfile YAML
//! parsing (see
//! <https://github.com/tranxuanthang/lrcget/blob/main/LYRICSFILE_CONCEPT.md>).

use crate::model::kind;
use crate::model::{normalize_lyric_lang, Agent, Cue, Line, LyricList, Lyrics};
use crate::normalize::normalize_lyrics;
use nd_utils::str::sanitize_text;
use serde::Deserialize;

const LYRICSFILE_VERSION: &str = "1.0";

#[derive(Debug, Default, Deserialize)]
struct LyricsfileDocument {
    #[serde(default)]
    version: String,
    #[serde(default)]
    metadata: LyricsfileMetadata,
    #[serde(default)]
    lines: Vec<LyricsfileLineEntry>,
    #[serde(default)]
    plain: String,
}

#[derive(Debug, Default, Deserialize)]
struct LyricsfileMetadata {
    #[serde(default)]
    #[allow(dead_code)]
    title: String,
    #[serde(default)]
    #[allow(dead_code)]
    artist: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    offset_ms: i64,
    #[serde(default)]
    instrumental: bool,
}

#[derive(Debug, Default, Deserialize)]
struct LyricsfileLineEntry {
    #[serde(default)]
    text: String,
    #[serde(default)]
    start_ms: i64,
    #[serde(default)]
    end_ms: Option<i64>,
    #[serde(default)]
    words: Vec<LyricsfileWordEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct LyricsfileWordEntry {
    #[serde(default)]
    text: String,
    #[serde(default)]
    start_ms: i64,
    #[serde(default)]
    end_ms: Option<i64>,
}

/// `parseLyricsfile`: parses a Lyricsfile YAML document into a
/// single-entry `LyricList` containing a single main `Lyrics`. Returns
/// `Ok(None)` when the input parses as YAML but does not declare Lyricsfile
/// version 1.0.
///
/// When the source contains per-word timing via `lines[].words[]`, each word
/// becomes a `Cue` with inclusive UTF-8 byte offsets into `Line::value`, and
/// overlapping lines are attributed to synthetic voice agents via
/// lowest-free voice ID assignment so the OpenSubsonic v2 enhanced response
/// can split parallel vocals.
pub fn parse_lyricsfile(lang: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    let doc: LyricsfileDocument = serde_yaml::from_slice(contents)
        .map_err(|e| format!("not a valid Lyricsfile YAML: {e}"))?;

    if doc.version.trim() != LYRICSFILE_VERSION {
        return Ok(None);
    }

    // Fall back to the caller's language when the document omits its own,
    // matching the SRT/TTML parsers; normalize_lyric_lang yields "xxx" only
    // if both are empty.
    let doc_lang = if doc.metadata.language.trim().is_empty() {
        lang.to_string()
    } else {
        doc.metadata.language.clone()
    };
    let mut lyrics = Lyrics {
        display_artist: sanitize_text(&doc.metadata.artist),
        display_title: sanitize_text(&doc.metadata.title),
        lang: normalize_lyric_lang(&doc_lang),
        kind: kind::MAIN.to_string(),
        offset: if doc.metadata.offset_ms != 0 {
            Some(doc.metadata.offset_ms)
        } else {
            None
        },
        ..Default::default()
    };

    if doc.metadata.instrumental {
        return Ok(Some(vec![normalize_lyrics(lyrics)]));
    }

    if doc.lines.is_empty() {
        let lines = build_plain_lyricsfile_lines(&doc.plain);
        if lines.is_empty() {
            return Ok(None);
        }
        lyrics.line = lines;
        return Ok(Some(vec![normalize_lyrics(lyrics)]));
    }

    let (lines, agents) = build_lyricsfile_lines(doc.lines);
    lyrics.line = lines;
    lyrics.agents = agents;
    lyrics.synced = true;
    Ok(Some(vec![normalize_lyrics(lyrics)]))
}

/// `buildLyricsfileLines`: converts YAML line entries to `Line` entries with
/// per-cue agent IDs assigned by streaming overlap clustering (lowest-free
/// voice ID). The agents slice is emitted only when at least one cue carries
/// attribution AND more than one voice is used; otherwise agent IDs are
/// stripped so the wire shape stays simple per the OpenSubsonic spec rule
/// "agents should not be emitted without cueLine data".
fn build_lyricsfile_lines(entries: Vec<LyricsfileLineEntry>) -> (Vec<Line>, Vec<Agent>) {
    if entries.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Resolved end timestamps per entry: explicit end_ms, final word end_ms,
    // then the next entry's start. The last entry's end stays None when no
    // explicit or word-level end is available.
    let ends: Vec<Option<i64>> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| lyricsfile_line_end(entry, entries.get(i + 1).map(|e| e.start_ms)))
        .collect();

    let mut active: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut max_voice: i64 = -1;
    let mut any_cues = false;
    let mut lines: Vec<Line> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        active.retain(|_, v_end| *v_end > entry.start_ms);

        let mut voice_id = 0i64;
        while active.contains_key(&voice_id) {
            voice_id += 1;
        }
        if voice_id > max_voice {
            max_voice = voice_id;
        }

        let agent_id = format!("voice-{voice_id}");
        let (cues, value) = words_to_line_cues(entry, &agent_id);
        if !cues.is_empty() {
            any_cues = true;
        }

        let line = Line {
            start: Some(entry.start_ms),
            end: ends[i],
            value,
            cue: cues,
        };
        lines.push(line);

        let end_ms = ends[i].unwrap_or(entry.start_ms);
        active.insert(voice_id, end_ms);
    }

    // Monophonic source, or attribution that has nowhere to land: emit no
    // agents and strip per-cue agent IDs to keep the wire shape simple.
    if max_voice <= 0 || !any_cues {
        for line in &mut lines {
            for cue in &mut line.cue {
                cue.agent_id = String::new();
            }
        }
        return (lines, Vec::new());
    }

    let mut agents: Vec<Agent> = Vec::with_capacity((max_voice + 1) as usize);
    for v in 0..=max_voice {
        let role = if v == 0 { "main" } else { "voice" };
        agents.push(Agent {
            id: format!("voice-{v}"),
            role: role.to_string(),
            ..Default::default()
        });
    }
    (lines, agents)
}

fn lyricsfile_line_end(entry: &LyricsfileLineEntry, next_start: Option<i64>) -> Option<i64> {
    if let Some(e) = entry.end_ms {
        return Some(e);
    }
    if let Some(last_word) = entry.words.last() {
        if let Some(e) = last_word.end_ms {
            return Some(e);
        }
    }
    next_start
}

fn build_plain_lyricsfile_lines(plain: &str) -> Vec<Line> {
    let plain = sanitize_text(plain);
    let raw_lines = plain.split('\n');
    let mut lines: Vec<Line> = Vec::new();
    for raw in raw_lines {
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        lines.push(Line {
            value: value.to_string(),
            ..Default::default()
        });
    }
    lines
}

/// `wordsToLineCues`: converts a Lyricsfile line entry's words[] into `Cue`
/// entries with inclusive UTF-8 byte offsets into the reconstructed line
/// value. The line value is built from cue text concatenation rather than
/// trusting entry.text, because the Lyricsfile spec only requires word.text
/// to "approximate" line.text — byte offsets must always land inside
/// `Line::value`.
fn words_to_line_cues(entry: &LyricsfileLineEntry, agent_id: &str) -> (Vec<Cue>, String) {
    if entry.words.is_empty() {
        return (Vec::new(), sanitize_text(&entry.text));
    }

    let line_value = entry
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<String>();

    let mut cues: Vec<Cue> = Vec::with_capacity(entry.words.len());
    let mut cursor: usize = 0;
    for w in &entry.words {
        let value_bytes = w.text.len();
        let bs = cursor;
        let mut be = bs;
        if value_bytes > 0 {
            be = bs + value_bytes - 1;
            cursor = be + 1;
        }

        let mut cue = Cue {
            start: Some(w.start_ms),
            value: w.text.clone(),
            byte_start: bs,
            byte_end: be,
            agent_id: agent_id.to_string(),
            ..Default::default()
        };
        if let Some(e) = w.end_ms {
            cue.end = Some(e);
        }
        cues.push(cue);
    }

    for i in 0..cues.len().saturating_sub(1) {
        if cues[i].end.is_none() {
            if let Some(s) = cues[i + 1].start {
                cues[i].end = Some(s);
            }
        }
    }
    (cues, line_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    // Port of model/lyrics_lyricsfile_test.go
    #[test]
    fn returns_none_for_yaml_without_the_lyricsfile_version_marker() {
        // Arbitrary YAML
        assert!(parse_lyricsfile("", b"hello: world\n").unwrap().is_none());
        // Lyricsfile-shaped but unversioned
        let input = b"metadata:\n  title: 'Looks close'\nlines:\n  - text: \"But should not be claimed\"\n    start_ms: 1000\n";
        assert!(parse_lyricsfile("", input).unwrap().is_none());
    }

    #[test]
    fn returns_an_error_for_invalid_yaml() {
        assert!(parse_lyricsfile("", b"not: valid: yaml: [").is_err());
    }

    #[test]
    fn parses_line_level_metadata_without_cues() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Sample Track'\n  artist: 'Test Artist'\n  language: 'eng'\n  offset_ms: -100\nlines:\n  - text: \"We're no strangers to love\"\n    start_ms: 18800\n  - text: \"You know the rules and so do I\"\n    start_ms: 22801\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(l.kind, "main");
        assert_eq!(l.lang, "eng");
        assert_eq!(l.display_artist, "Test Artist");
        assert_eq!(l.display_title, "Sample Track");
        assert!(l.synced);
        assert_eq!(l.offset, p(-100));
        assert!(l.agents.is_empty());

        assert_eq!(l.line.len(), 2);
        assert_eq!(l.line[0].start, p(18800));
        assert_eq!(l.line[0].end, p(22801));
        assert_eq!(l.line[0].value, "We're no strangers to love");
        assert!(l.line[0].cue.is_empty());

        assert_eq!(l.line[1].start, p(22801));
        assert_eq!(l.line[1].end, None);
        assert_eq!(l.line[1].value, "You know the rules and so do I");
        assert!(l.line[1].cue.is_empty());
    }

    #[test]
    fn resolves_the_lyric_language() {
        let cases = [
            // (metadata language, caller lang, want)
            ("eng", "deu", "eng"),
            ("", "deu", "deu"),
            ("", "", "xxx"),
        ];
        for (meta_language, caller_lang, want) in cases {
            let mut input = String::from("version: '1.0'\nmetadata:\n  title: 'T'\n");
            if !meta_language.is_empty() {
                input += &format!("  language: '{meta_language}'\n");
            }
            input += "lines:\n  - text: \"line\"\n    start_ms: 0\n";

            let lyrics = parse_lyricsfile(caller_lang, input.as_bytes())
                .unwrap()
                .unwrap();
            assert_eq!(lyrics.len(), 1, "case ({meta_language}, {caller_lang})");
            assert_eq!(
                lyrics[0].lang, want,
                "case ({meta_language}, {caller_lang})"
            );
        }
    }

    #[test]
    fn parses_plain_only_lyrics_as_unsynced_lines() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Plain Track'\n  artist: 'Plain Artist'\n  language: 'en'\nlines: []\nplain: |\n  [Verse 1]\n  First line\n\n  Second line\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(l.kind, "main");
        assert_eq!(l.lang, "en");
        assert_eq!(l.display_artist, "Plain Artist");
        assert_eq!(l.display_title, "Plain Track");
        assert!(!l.synced);
        assert!(l.agents.is_empty());
        assert_eq!(
            l.line,
            vec![
                Line {
                    value: "[Verse 1]".into(),
                    ..Default::default()
                },
                Line {
                    value: "First line".into(),
                    ..Default::default()
                },
                Line {
                    value: "Second line".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn produces_word_cues_with_inclusive_byte_offsets_for_monophonic_word_data() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Karaoke'\n  artist: 'Singer'\n  language: 'eng'\nlines:\n  - text: \"Hello world\"\n    start_ms: 1000\n    end_ms: 3000\n    words:\n      - text: \"Hello \"\n        start_ms: 1000\n        end_ms: 1500\n      - text: \"world\"\n        start_ms: 1500\n        end_ms: 3000\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert!(l.synced);
        assert!(l.agents.is_empty());
        assert_eq!(l.line.len(), 1);

        let line = &l.line[0];
        assert_eq!(line.start, p(1000));
        assert_eq!(line.end, p(3000));
        assert_eq!(line.value, "Hello world");
        assert_eq!(line.cue.len(), 2);

        assert_eq!(line.cue[0].start, p(1000));
        assert_eq!(line.cue[0].end, p(1500));
        assert_eq!(line.cue[0].value, "Hello ");
        assert_eq!(line.cue[0].byte_start, 0);
        assert_eq!(line.cue[0].byte_end, 5);
        assert_eq!(line.cue[0].agent_id, "");

        assert_eq!(line.cue[1].start, p(1500));
        assert_eq!(line.cue[1].end, p(3000));
        assert_eq!(line.cue[1].value, "world");
        assert_eq!(line.cue[1].byte_start, 6);
        assert_eq!(line.cue[1].byte_end, 10);
        assert_eq!(line.cue[1].agent_id, "");
    }

    #[test]
    fn prefers_final_word_end_over_next_line_start_when_inferring_line_end() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Overlap From Words'\nlines:\n  - text: \"Long vocal\"\n    start_ms: 1000\n    words:\n      - text: \"Long \"\n        start_ms: 1000\n        end_ms: 2000\n      - text: \"vocal\"\n        start_ms: 2000\n        end_ms: 4000\n  - text: \"echo\"\n    start_ms: 3000\n    end_ms: 3500\n    words:\n      - text: \"echo\"\n        start_ms: 3000\n        end_ms: 3500\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(
            l.agents,
            vec![
                Agent {
                    id: "voice-0".into(),
                    role: "main".into(),
                    ..Default::default()
                },
                Agent {
                    id: "voice-1".into(),
                    role: "voice".into(),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(l.line.len(), 2);
        assert_eq!(l.line[0].end, Some(4000));
        assert_eq!(l.line[0].cue[1].end, l.line[0].end);
        assert_eq!(l.line[1].cue[0].agent_id, "voice-1");
    }

    #[test]
    fn synthesises_voice_agents_for_overlapping_lines_and_attributes_per_cue() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Duet'\nlines:\n  - text: \"Lead vocal\"\n    start_ms: 1000\n    end_ms: 4000\n    words:\n      - text: \"Lead \"\n        start_ms: 1000\n        end_ms: 2000\n      - text: \"vocal\"\n        start_ms: 2000\n        end_ms: 4000\n  - text: \"echo\"\n    start_ms: 2000\n    end_ms: 3000\n    words:\n      - text: \"echo\"\n        start_ms: 2000\n        end_ms: 3000\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(
            l.agents,
            vec![
                Agent {
                    id: "voice-0".into(),
                    role: "main".into(),
                    ..Default::default()
                },
                Agent {
                    id: "voice-1".into(),
                    role: "voice".into(),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(l.line.len(), 2);

        assert_eq!(l.line[0].value, "Lead vocal");
        assert_eq!(l.line[0].start, p(1000));
        assert_eq!(l.line[0].end, p(4000));
        assert_eq!(l.line[0].cue.len(), 2);
        assert_eq!(l.line[0].cue[0].agent_id, "voice-0");
        assert_eq!(l.line[0].cue[1].agent_id, "voice-0");
        assert_eq!(l.line[0].cue[0].byte_start, 0);
        assert_eq!(l.line[0].cue[0].byte_end, 4);
        assert_eq!(l.line[0].cue[1].byte_start, 5);
        assert_eq!(l.line[0].cue[1].byte_end, 9);

        assert_eq!(l.line[1].value, "echo");
        assert_eq!(l.line[1].start, p(2000));
        assert_eq!(l.line[1].end, p(3000));
        assert_eq!(l.line[1].cue.len(), 1);
        assert_eq!(l.line[1].cue[0].agent_id, "voice-1");
        assert_eq!(l.line[1].cue[0].byte_start, 0);
        assert_eq!(l.line[1].cue[0].byte_end, 3);
    }

    #[test]
    fn emits_empty_lines_with_synced_false_for_instrumental_tracks() {
        let input = b"version: '1.0'\nmetadata:\n  title: 'Solo Piano'\n  artist: 'Composer'\n  language: 'eng'\n  instrumental: true\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(l.kind, "main");
        assert_eq!(l.lang, "eng");
        assert_eq!(l.display_artist, "Composer");
        assert_eq!(l.display_title, "Solo Piano");
        assert!(!l.synced);
        assert!(l.line.is_empty());
        assert!(l.agents.is_empty());
    }

    #[test]
    fn strips_agent_attribution_when_overlapping_lines_carry_no_cues() {
        let input = b"version: '1.0'\nlines:\n  - text: \"Lead\"\n    start_ms: 1000\n    end_ms: 4000\n  - text: \"echo\"\n    start_ms: 2000\n    end_ms: 3000\n";
        let lyrics = parse_lyricsfile("", input).unwrap().unwrap();
        assert_eq!(lyrics.len(), 1);

        let l = &lyrics[0];
        assert_eq!(l.line.len(), 2);
        assert!(l.agents.is_empty());
        assert!(l.line[0].cue.is_empty());
        assert!(l.line[1].cue.is_empty());
    }
}
