//! #09 — port of `model/lyrics_lrc.go`: LRC / Enhanced LRC (A2 extension)
//! parsing.

use crate::model::{Agent, Cue, Line, Lyrics};
use crate::normalize::normalize_cue_lines;
use nd_utils::str::sanitize_text;
use regex::Regex;
use std::sync::OnceLock;

// Support the standard [mm:ss.mm], as well as [hh:*] and [*.mmm]
// (Go timeRegexString).
const TIME_REGEX_STRING: &str = r"\[([0-9]{1,2}:)?([0-9]{1,2}):([0-9]{1,2})(\.[0-9]{1,3})?\]";

/// Enhanced LRC inline word-level timing markers like `<00:12.34>`
/// (Go enhancedLRCTimeString).
const ENHANCED_LRC_TIME_STRING: &str = r"<([0-9]{1,2}:)?([0-9]{1,2}):([0-9]{1,2})(\.[0-9]{1,3})?>";

fn sync_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(^|\n)\s*{TIME_REGEX_STRING}")).unwrap())
}

fn time_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(TIME_REGEX_STRING).unwrap())
}

fn lrc_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[(ar|ti|offset|lang):([^]]+)]").unwrap())
}

fn bg_tag_regex() -> &'static Regex {
    // Non-standard background-vocal tag: [bg: <mm:ss.mm>word...] attaches to
    // the preceding line.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[bg:(.*)]$").unwrap())
}

fn unknown_tag_regex() -> &'static Regex {
    // Any other [name:value] line is a tag we don't support (e.g. [al:],
    // [by:]) and must be skipped whole.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[[A-Za-z][^]:]*:[^]]*]").unwrap())
}

fn enhanced_lrc_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(ENHANCED_LRC_TIME_STRING).unwrap())
}

// Capture group indices in the Rust timeRegex / enhancedLRCRegex layout:
// 0 full, 1 hour?, 2 min, 3 sec, 4 .ms? (only the group starts are read;
// see parse_time).
const GRP_HOUR_START: usize = 1;
const GRP_MIN_START: usize = 2;
const GRP_SEC_START: usize = 3;
const GRP_MS_START: usize = 4;

/// A time-marker match: byte position plus the capture-group spans.
type TimeMatches = Vec<(usize, Vec<Option<(usize, usize)>>)>;

/// `parseLRC`: parses LRC text (plain or Enhanced LRC) into a single
/// `Lyrics`. `language` is the default language, overridable via `[lang:]`.
pub fn parse_lrc(language: &str, text: &str) -> Result<Lyrics, String> {
    let text = sanitize_text(text);

    let lines = text.split('\n');
    let mut structured_lines: Vec<Line> = Vec::new();

    let mut artist = String::new();
    let mut title = String::new();
    let mut offset: Option<i64> = None;
    let mut language = language.to_string();

    let synced = sync_regex().is_match(&text);
    let mut prior_line = String::new();
    let mut valid_line = false;
    let mut repeated = false;
    let mut has_bg = false;
    let mut timestamps: Vec<i64> = Vec::new();
    let mut pending_bg: Vec<String> = Vec::new();

    let flush_line = |prior_line: &mut String,
                      valid_line: &mut bool,
                      timestamps: &mut Vec<i64>,
                      pending_bg: &mut Vec<String>,
                      structured_lines: &mut Vec<Line>,
                      has_bg: &mut bool| {
        let (value, cues) = parse_enhanced_line(prior_line);
        let (value, cues, merged) = merge_bg_layers(value, cues, pending_bg);
        *has_bg = *has_bg || merged;
        if let Some(&first) = timestamps.first() {
            for &ts in timestamps.iter() {
                let mut line = Line {
                    start: Some(ts),
                    value: value.clone(),
                    ..Default::default()
                };
                if !cues.is_empty() {
                    line.cue = shift_elrc_cues(&cues, ts - first);
                }
                structured_lines.push(line);
            }
        }
        timestamps.clear();
        pending_bg.clear();
        prior_line.clear();
        let _ = valid_line;
    };

    for raw_line in lines {
        let line = raw_line.trim();
        if line.is_empty() {
            if valid_line {
                prior_line.push('\n');
            }
            continue;
        }

        if !synced {
            structured_lines.push(Line {
                value: line.to_string(),
                ..Default::default()
            });
            continue;
        }

        if let Some(id_tag) = lrc_id_regex().captures(line) {
            let value = id_tag
                .get(2)
                .map_or(String::new(), |m| m.as_str().to_string());
            match id_tag.get(1).map_or("", |m| m.as_str()) {
                "ar" => artist = sanitize_text(value.trim()),
                "lang" => language = sanitize_text(value.trim()),
                // Go logs a warning on a bad offset; we keep no-offset silently.
                "offset" => {
                    if let Ok(off) = value.trim().parse::<i64>() {
                        offset = Some(off)
                    }
                }
                "ti" => title = sanitize_text(value.trim()),
                _ => {}
            }
            continue;
        }

        if let Some(bg_match) = bg_tag_regex().captures(line) {
            if valid_line {
                pending_bg.push(
                    bg_match
                        .get(1)
                        .map_or_else(String::new, |m| m.as_str().to_string()),
                );
            }
            continue;
        }

        if unknown_tag_regex().is_match(line) {
            continue;
        }

        let times: TimeMatches = time_regex()
            .captures_iter(line)
            .map(|c| {
                let full = c.get(0).unwrap();
                let pos = full.start();
                // Rust regex layout: 0 full, 1 hour?, 2 min, 3 sec, 4 .ms?
                // (the Go regex duplicated groups; parse_time indexes via
                // GRP_* which map onto this layout below).
                let groups: Vec<Option<(usize, usize)>> = (0..5)
                    .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                    .collect();
                (pos, groups)
            })
            .collect();
        if times.len() > 1 {
            repeated = true;
        }

        // The second condition is for when there is a timestamp in the middle
        // of a line (after any text).
        if times.is_empty() || times[0].0 != 0 {
            if valid_line {
                prior_line.push('\n');
                prior_line.push_str(line);
            }
            continue;
        }

        if valid_line {
            flush_line(
                &mut prior_line,
                &mut valid_line,
                &mut timestamps,
                &mut pending_bg,
                &mut structured_lines,
                &mut has_bg,
            );
        }
        valid_line = true;

        let mut end: usize = 0;
        let mut parsed_timestamps: Vec<i64> = Vec::new();

        // [pos, groups] entries; later matches must sit at the end of the
        // previous one (no text in between) to count.
        for (match_pos, groups) in &times {
            if end != 0 {
                let middle = line[end..*match_pos].trim();
                if !middle.is_empty() {
                    break;
                }
            }

            let groups: Vec<Option<(usize, usize)>> = groups.clone();
            end = match_pos + groups[0].map(|(s, e)| e - s).unwrap_or(0);
            let time_in_millis =
                parse_time(line, &groups).map_err(|e| format!("parsing LRC time: {e}"))?;
            parsed_timestamps.push(time_in_millis);
        }

        timestamps = parsed_timestamps;

        if end >= line.len() {
            prior_line = String::new();
        } else {
            prior_line = line[end..].trim().to_string();
        }
    }

    if valid_line {
        flush_line(
            &mut prior_line,
            &mut valid_line,
            &mut timestamps,
            &mut pending_bg,
            &mut structured_lines,
            &mut has_bg,
        );
    }

    if has_bg {
        fill_main_agent_id(&mut structured_lines);
    }

    // If there are repeated values, there is no guarantee that they are in
    // order; in this case, sort the lyrics by start time.
    if repeated {
        structured_lines.sort_by_key(|l| l.start);
    }

    let mut agents = Vec::new();
    if has_bg {
        agents = vec![
            Agent {
                id: "main".into(),
                role: "main".into(),
                ..Default::default()
            },
            Agent {
                id: background_agent_id("main"),
                role: "bg".into(),
                ..Default::default()
            },
        ];
    }

    Ok(Lyrics {
        display_artist: artist,
        display_title: title,
        lang: language,
        line: normalize_cue_lines(structured_lines),
        offset,
        synced,
        agents,
        ..Default::default()
    })
}

/// `ttmlBackgroundAgentPrefix` (Go model/lyrics_ttml.go): derived background
/// agent id prefix, shared by the LRC `[bg:]` merge.
const TTML_BACKGROUND_AGENT_PREFIX: &str = "__nd_bg__|";

/// `backgroundAgentID` (Go model/lyrics_ttml.go).
pub(crate) fn background_agent_id(agent_id: &str) -> String {
    format!("{TTML_BACKGROUND_AGENT_PREFIX}{agent_id}")
}

/// `mergeBgLayers`: appends parsed `[bg:]` content to a line's value and cues
/// as a background agent layer, mirroring the TTML background-vocal
/// representation.
fn merge_bg_layers(
    value: String,
    mut cues: Vec<Cue>,
    bg_lines: &[String],
) -> (String, Vec<Cue>, bool) {
    let mut value = value;
    let mut merged = false;
    for bg in bg_lines {
        let (bg_value, bg_cues) = parse_enhanced_line(bg);
        if bg_cues.is_empty() {
            continue;
        }
        let mut offset = 0usize;
        if !value.is_empty() {
            offset = value.len() + 1;
            value.push(' ');
        }
        value.push_str(&bg_value);
        for c in bg_cues {
            let mut c = c;
            c.byte_start += offset;
            c.byte_end += offset;
            c.agent_id = background_agent_id("main");
            cues.push(c);
        }
        merged = true;
    }
    (value, cues, merged)
}

fn fill_main_agent_id(lines: &mut [Line]) {
    for line in lines {
        for cue in &mut line.cue {
            if cue.agent_id.is_empty() {
                cue.agent_id = "main".into();
            }
        }
    }
}

struct Segment {
    start: i64,
    raw_start: usize,
    raw_end: usize,
}

/// `parseEnhancedLine`: extracts word-level timing cues from Enhanced LRC
/// inline markers and computes UTF-8 byte offsets against the final stripped
/// line value.
fn parse_enhanced_line(text: &str) -> (String, Vec<Cue>) {
    let matches: TimeMatches = enhanced_lrc_regex()
        .captures_iter(text)
        .map(|c| {
            let full = c.get(0).unwrap();
            // 5-group rust layout: 0 full, 1 hour?, 2 min, 3 sec, 4 .ms?
            (
                full.start(),
                (0..5)
                    .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                    .collect(),
            )
        })
        .collect();
    if matches.is_empty() {
        return (text.trim().to_string(), Vec::new());
    }

    let mut segments: Vec<Segment> = Vec::with_capacity(matches.len());
    let mut raw_value = String::new();
    let mut trailing_end: Option<i64> = None;

    for (i, (match_pos, groups)) in matches.iter().enumerate() {
        // groups[0] is already absolute (c.get(0) spans), not relative.
        let full_end = groups[0].map(|(_, e)| e).unwrap_or(*match_pos);
        // Rewrite <...> as [...] so parse_time can handle it with the same
        // logic: the marker spans [match_pos, full_end) in `text`.
        let rewritten = format!("[{}]", &text[*match_pos + 1..full_end - 1]);
        // Groups remapped into the rewritten string (group 1 is the hour
        // prefix including ':', groups 2.. are inside the brackets).
        // remapped[0] = full (relative), remapped[1..] = groups 1..4 (relative)
        // `rewritten` = "[" + text[match_pos+1..full_end-1] + "]": inner
        // content starts at absolute match_pos+1, which is index 1 in
        // `rewritten` (after the added '[').
        let shift = (match_pos + 1) - 1; // absolute->rewritten offset
        let remapped: Vec<Option<(usize, usize)>> = groups
            .iter()
            .map(|g| g.map(|(s, e)| (s - shift, e - shift)))
            .collect();
        let Ok(time_ms) = parse_time(&rewritten, &remapped) else {
            continue;
        };

        // Text runs from after this marker to the start of the next marker
        // (or end of string).
        let text_start = full_end;
        let text_end = if i + 1 < matches.len() {
            matches[i + 1].0
        } else {
            text.len()
        };

        let word = &text[text_start..text_end];
        if word.is_empty() {
            if i == matches.len() - 1 {
                trailing_end = Some(time_ms);
            }
            continue;
        }

        let raw_start = raw_value.len();
        raw_value.push_str(word);
        segments.push(Segment {
            start: time_ms,
            raw_start,
            raw_end: raw_value.len(),
        });
    }

    if segments.is_empty() {
        return (strip_enhanced_markers(text).trim().to_string(), Vec::new());
    }

    let final_raw = raw_value.as_str();
    let left_trim_bytes = final_raw.len() - final_raw.trim_start().len();
    let right_trim_bytes = final_raw.len() - final_raw.trim_end().len();
    let mut trimmed_end = final_raw.len() - right_trim_bytes;
    if trimmed_end < left_trim_bytes {
        trimmed_end = left_trim_bytes;
    }

    let mut cues: Vec<Cue> = Vec::with_capacity(segments.len());
    for seg in &segments {
        let byte_start = seg.raw_start.max(left_trim_bytes);
        let byte_end = seg.raw_end.min(trimmed_end);
        if byte_start >= byte_end {
            continue;
        }

        cues.push(Cue {
            start: Some(seg.start),
            value: final_raw[byte_start..byte_end].to_string(),
            byte_start: byte_start - left_trim_bytes,
            byte_end: byte_end - left_trim_bytes - 1,
            ..Default::default()
        });
    }

    if let Some(t) = trailing_end {
        if let Some(last) = cues.last_mut() {
            last.end = Some(t);
        }
    }

    (final_raw.trim().to_string(), cues)
}

/// `stripEnhancedMarkers`: removes all `<mm:ss.mm>` inline markers from text,
/// returning the plain lyric text.
fn strip_enhanced_markers(text: &str) -> String {
    enhanced_lrc_regex().replace_all(text, "").to_string()
}

/// `shiftELRCCues`: returns a copy of `base_cues` with each cue's start/end
/// timestamps shifted by `offset_ms`. Inline ELRC word markers parse to
/// absolute timestamps anchored at the line's first occurrence, so
/// repeated-line LRC inputs of the form `[t0][t1]...` must shift the cues by
/// (t1-t0) for the second occurrence to point at the correct moment.
fn shift_elrc_cues(base_cues: &[Cue], offset_ms: i64) -> Vec<Cue> {
    if base_cues.is_empty() {
        return Vec::new();
    }
    base_cues
        .iter()
        .map(|c| Cue {
            start: c.start.map(|s| s + offset_ms),
            end: c.end.map(|e| e + offset_ms),
            ..c.clone()
        })
        .collect()
}

/// `parseTime`: converts an LRC `[hh:mm:ss.ms]` match into milliseconds.
/// `groups` follows the Go capture layout: index 0 is the full match span,
/// 1 the optional hour prefix (including ':'), 2/3 the hour digits, 4/5
/// minutes, 6/7 seconds, 8/9 the fractional part (including '.').
fn parse_time(line: &str, groups: &[Option<(usize, usize)>]) -> Result<i64, String> {
    let mut hours: i64 = 0;
    let mut millis: i64 = 0;

    let full = groups
        .first()
        .copied()
        .flatten()
        .ok_or("missing time match")?;

    if let Some((hs, he)) = groups.get(GRP_HOUR_START).copied().flatten() {
        // Subtract 1 because the hour group has ':' at the end (in the Go
        // regex the hour capture includes the trailing ':'); with the Rust
        // regex the hour group excludes it, so use the span as-is.
        let _ = full;
        let hour_text = &line[hs..he];
        // The hour capture includes the trailing ':' (Go layout); strip it.
        hours = hour_text
            .trim_end_matches(':')
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
    }

    let (ms_, me) = groups
        .get(GRP_MIN_START)
        .copied()
        .flatten()
        .ok_or("missing minutes")?;
    let minutes: i64 = line[ms_..me].parse::<i64>().map_err(|e| e.to_string())?;

    let (ss, se) = groups
        .get(GRP_SEC_START)
        .copied()
        .flatten()
        .ok_or("missing seconds")?;
    let sec: i64 = line[ss..se].parse::<i64>().map_err(|e| e.to_string())?;

    if let Some((mss, mse)) = groups.get(GRP_MS_START).copied().flatten() {
        // The Rust regex capture excludes the leading '.', unlike Go's.
        // Go keeps the '.' in the group and scales by span length (3 => x10,
        // 2 => x100); our capture also includes the '.' so mirror that.
        let ms_text = &line[mss..mse];
        millis = ms_text[1..].parse::<i64>().map_err(|e| e.to_string())?;
        let length = mse - mss;
        if length == 3 {
            millis *= 10;
        } else if length == 2 {
            millis *= 100;
        }
    }

    Ok(((((hours * 60) + minutes) * 60 + sec) * 1000) + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    fn cue(
        start: Option<i64>,
        end: Option<i64>,
        value: &str,
        byte_start: usize,
        byte_end: usize,
        agent_id: &str,
    ) -> Cue {
        Cue {
            start,
            end,
            value: value.into(),
            byte_start,
            byte_end,
            agent_id: agent_id.into(),
        }
    }

    // Port of model/lyrics_lrc_test.go
    #[test]
    fn parses_tags_with_spaces() {
        let lyrics = parse_lrc(
            "xxx",
            "[lang:  eng  ]\n[offset: 1551 ]\n[ti: A title ]\n[ar: An artist ]\n[00:00.00]Hi there",
        )
        .unwrap();
        assert_eq!(lyrics.lang, "eng");
        assert!(lyrics.synced);
        assert_eq!(lyrics.display_artist, "An artist");
        assert_eq!(lyrics.display_title, "A title");
        assert_eq!(lyrics.offset, p(1551));
    }

    #[test]
    fn ignores_bad_offset() {
        let lyrics = parse_lrc("xxx", "[offset: NotANumber ]\n[00:00.00]Hi there").unwrap();
        assert_eq!(lyrics.offset, None);
    }

    #[test]
    fn accepts_lines_with_no_text_and_weird_times() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:00.00]Hi there\n\n\n[00:10.040]\n[00:40]Test\n[01:00:00]late",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(0),
                    value: "Hi there".into(),
                    ..Default::default()
                },
                Line {
                    start: p(10040),
                    value: String::new(),
                    ..Default::default()
                },
                Line {
                    start: p(40000),
                    value: "Test".into(),
                    ..Default::default()
                },
                Line {
                    start: p(1000 * 60 * 60),
                    value: "late".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn supports_multiple_timestamps_per_line() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:00.00]  [00:10.00]Repeated\n[13:00][51:00:00.00]",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(0),
                    value: "Repeated".into(),
                    ..Default::default()
                },
                Line {
                    start: p(10000),
                    value: "Repeated".into(),
                    ..Default::default()
                },
                Line {
                    start: p(13 * 60 * 1000),
                    value: String::new(),
                    ..Default::default()
                },
                Line {
                    start: p(1000 * 60 * 60 * 51),
                    value: String::new(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn supports_parsing_multiline_string() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:00.00]This is\na multiline  \n\n  [:0] string\n[10:00.001]This is\nalso one",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(0),
                    value: "This is\na multiline\n\n[:0] string".into(),
                    ..Default::default()
                },
                Line {
                    start: p(10 * 60 * 1000 + 1),
                    value: "This is\nalso one".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn does_not_match_timestamp_in_middle_of_line() {
        let lyrics = parse_lrc("xxx", "This could [00:00:00] be a synced file").unwrap();
        assert!(!lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![Line {
                value: "This could [00:00:00] be a synced file".into(),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn allows_timestamp_in_middle_of_line_if_also_at_beginning() {
        let lyrics = parse_lrc(
            "xxx",
            "  [00:00] This is [00:00:00] be a synced file\n\t\t[00:01]Line 2",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(0),
                    value: "This is [00:00:00] be a synced file".into(),
                    ..Default::default()
                },
                Line {
                    start: p(1000),
                    value: "Line 2".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn ignores_lines_prior_to_first_timestamp() {
        let lyrics = parse_lrc(
            "xxx",
            "This is some prelude\nThat doesn't\nmatter\n[00:00]Text",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![Line {
                start: p(0),
                value: "Text".into(),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn handles_all_possible_ms_cases() {
        let lyrics = parse_lrc("xxx", "[00:00.001]a\n[00:00.01]b\n[00:00.1]c").unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(1),
                    value: "a".into(),
                    ..Default::default()
                },
                Line {
                    start: p(10),
                    value: "b".into(),
                    ..Default::default()
                },
                Line {
                    start: p(100),
                    value: "c".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn properly_sorts_repeated_lyrics_out_of_order() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:00.00]  [13:00]Repeated\n[00:10.00][51:00:00.00]Test\n[00:40.00]Not repeated",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![
                Line {
                    start: p(0),
                    value: "Repeated".into(),
                    ..Default::default()
                },
                Line {
                    start: p(10000),
                    value: "Test".into(),
                    ..Default::default()
                },
                Line {
                    start: p(40000),
                    value: "Not repeated".into(),
                    ..Default::default()
                },
                Line {
                    start: p(13 * 60 * 1000),
                    value: "Repeated".into(),
                    ..Default::default()
                },
                Line {
                    start: p(1000 * 60 * 60 * 51),
                    value: "Test".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn parses_enhanced_lrc_with_word_level_timing() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:01.00]<00:01.00>Some <00:01.50>lyrics <00:02.00>here\n[00:03.00]<00:03.00>More <00:03.50>words",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.line.len(), 2);

        let line0 = &lyrics.line[0];
        assert_eq!(line0.start, p(1000));
        assert_eq!(line0.end, p(3000));
        assert_eq!(line0.value, "Some lyrics here");
        assert_eq!(
            line0.cue,
            vec![
                cue(p(1000), p(1500), "Some ", 0, 4, ""),
                cue(p(1500), p(2000), "lyrics ", 5, 11, ""),
                cue(p(2000), p(3000), "here", 12, 15, ""),
            ]
        );

        let line1 = &lyrics.line[1];
        assert_eq!(line1.start, p(3000));
        assert_eq!(line1.end, p(3500));
        assert_eq!(line1.value, "More words");
        assert_eq!(
            line1.cue,
            vec![
                cue(p(3000), None, "More ", 0, 4, ""),
                cue(p(3500), None, "words", 5, 9, ""),
            ]
        );
        assert_eq!(line1.cue[1].end, None);
    }

    #[test]
    fn does_not_parse_malformed_enhanced_lrc_timing_markers() {
        let lyrics = parse_lrc("xxx", "[00:01.00]<00:01a50>Not a marker").unwrap();
        assert!(lyrics.synced);
        assert_eq!(
            lyrics.line,
            vec![Line {
                start: p(1000),
                value: "<00:01a50>Not a marker".into(),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn skips_unknown_tag_lines_instead_of_merging_into_prior_line() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:01.00]<00:01.00>Some <00:01.50>lyrics\n[by: someone]\n[al: An album]\n[00:03.00]Next",
        )
        .unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.line.len(), 2);

        assert_eq!(lyrics.line[0].value, "Some lyrics");
        assert_eq!(
            lyrics.line[0].cue,
            vec![
                cue(p(1000), p(1500), "Some ", 0, 4, ""),
                cue(p(1500), p(3000), "lyrics", 5, 10, ""),
            ]
        );
        assert_eq!(lyrics.line[1].value, "Next");
    }

    #[test]
    fn attaches_bg_vocals_to_previous_line_as_bg_agent_layer() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:52.00]<00:52.00>Main <00:52.50>line\n[bg: <00:53.00>Okay<00:53.50>]\n[00:55.00]Next",
        )
        .unwrap();
        assert_eq!(
            lyrics.agents,
            vec![
                Agent {
                    id: "main".into(),
                    role: "main".into(),
                    ..Default::default()
                },
                Agent {
                    id: "__nd_bg__|main".into(),
                    role: "bg".into(),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(lyrics.line.len(), 2);

        let line = &lyrics.line[0];
        assert_eq!(line.value, "Main line Okay");
        assert_eq!(
            line.cue,
            vec![
                cue(p(52000), p(52500), "Main ", 0, 4, "main"),
                cue(p(52500), p(55000), "line", 5, 8, "main"),
                cue(p(53000), p(53500), "Okay", 10, 13, "__nd_bg__|main"),
            ]
        );
        assert_eq!(lyrics.line[1].value, "Next");
    }

    #[test]
    fn skips_bg_line_without_inline_timing() {
        let lyrics = parse_lrc("xxx", "[00:01.00]Hi\n[bg: untimed]\n[00:03.00]Next").unwrap();
        assert!(lyrics.agents.is_empty());
        assert_eq!(lyrics.line.len(), 2);
        assert_eq!(lyrics.line[0].value, "Hi");
        assert_eq!(lyrics.line[1].value, "Next");
    }

    #[test]
    fn handles_mixed_enhanced_and_plain_lrc_lines() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:01.00]<00:01.00>Some <00:01.50>lyrics\n[00:03.00]Plain line\n[00:05.00]<00:05.00>More <00:05.50>words",
        )
        .unwrap();
        assert_eq!(lyrics.line.len(), 3);

        assert_eq!(
            lyrics.line[0].cue,
            vec![
                cue(p(1000), p(1500), "Some ", 0, 4, ""),
                cue(p(1500), p(3000), "lyrics", 5, 10, ""),
            ]
        );
        assert_eq!(lyrics.line[0].value, "Some lyrics");
        assert_eq!(lyrics.line[0].end, p(3000));

        assert!(lyrics.line[1].cue.is_empty());
        assert_eq!(lyrics.line[1].value, "Plain line");

        assert_eq!(
            lyrics.line[2].cue,
            vec![
                cue(p(5000), None, "More ", 0, 4, ""),
                cue(p(5500), None, "words", 5, 9, ""),
            ]
        );
        assert_eq!(lyrics.line[2].value, "More words");
    }

    #[test]
    fn preserves_byte_offsets_for_enhanced_lrc_cues() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:00.00]<00:00.00>Oh <00:00.90>love<00:01.30> me <00:01.60>tonight",
        )
        .unwrap();
        assert_eq!(lyrics.line.len(), 1);

        let line = &lyrics.line[0];
        assert_eq!(line.value, "Oh love me tonight");
        assert_eq!(
            line.cue,
            vec![
                cue(p(0), None, "Oh ", 0, 2, ""),
                cue(p(900), None, "love", 3, 6, ""),
                cue(p(1300), None, " me ", 7, 10, ""),
                cue(p(1600), None, "tonight", 11, 17, ""),
            ]
        );
    }

    #[test]
    fn uses_trailing_marker_as_end_of_last_word() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:01.00]<00:01.00>Some <00:01.50>lyrics<00:02.00>\n[00:30.00]Instrumental over",
        )
        .unwrap();
        assert_eq!(lyrics.line.len(), 2);

        let line = &lyrics.line[0];
        assert_eq!(line.value, "Some lyrics");
        assert_eq!(line.end, p(2000));
        assert_eq!(
            line.cue,
            vec![
                cue(p(1000), p(1500), "Some ", 0, 4, ""),
                cue(p(1500), p(2000), "lyrics", 5, 10, ""),
            ]
        );
    }

    #[test]
    fn shifts_trailing_marker_for_repeated_line_occurrences() {
        let lyrics = parse_lrc(
            "xxx",
            "[00:10.00][00:30.00]<00:10.10>Hello <00:10.50>world<00:10.90>",
        )
        .unwrap();
        assert_eq!(lyrics.line.len(), 2);

        assert_eq!(lyrics.line[0].end, p(10900));
        assert_eq!(
            lyrics.line[0].cue,
            vec![
                cue(p(10100), p(10500), "Hello ", 0, 5, ""),
                cue(p(10500), p(10900), "world", 6, 10, ""),
            ]
        );

        assert_eq!(lyrics.line[1].end, p(30900));
        assert_eq!(
            lyrics.line[1].cue,
            vec![
                cue(p(30100), p(30500), "Hello ", 0, 5, ""),
                cue(p(30500), p(30900), "world", 6, 10, ""),
            ]
        );
    }

    #[test]
    fn shifts_inline_elrc_word_timestamps_for_each_repeated_occurrence() {
        let lyrics =
            parse_lrc("xxx", "[00:10.00][00:30.00]<00:10.10>Hello <00:10.50>world").unwrap();
        assert_eq!(lyrics.line.len(), 2);

        assert_eq!(lyrics.line[0].start, p(10000));
        assert_eq!(lyrics.line[0].end, p(30000));
        assert_eq!(lyrics.line[0].value, "Hello world");
        assert_eq!(
            lyrics.line[0].cue,
            vec![
                cue(p(10100), p(10500), "Hello ", 0, 5, ""),
                cue(p(10500), p(30000), "world", 6, 10, ""),
            ]
        );

        assert_eq!(lyrics.line[1].start, p(30000));
        assert_eq!(lyrics.line[1].end, p(30500));
        assert_eq!(lyrics.line[1].value, "Hello world");
        assert_eq!(
            lyrics.line[1].cue,
            vec![
                cue(p(30100), None, "Hello ", 0, 5, ""),
                cue(p(30500), None, "world", 6, 10, ""),
            ]
        );
    }
}
