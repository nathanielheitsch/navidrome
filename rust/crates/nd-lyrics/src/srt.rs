//! #09 — port of `model/lyrics_srt.go`: SRT subtitle parsing.

use crate::model::normalize_lyric_lang;
use crate::model::{Line, LyricList, Lyrics};
use crate::normalize::normalize_lyrics;
use nd_utils::str::sanitize_text;
use regex::Regex;
use std::sync::OnceLock;

fn srt_time_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(\d{1,2}):(\d{2}):(\d{2})[,.](\d{1,3})\s*$").unwrap())
}

/// `parseSRT`: parses an SRT document into a single-entry `LyricList`, or
/// `None` when no valid blocks exist.
pub fn parse_srt(language: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    let mut raw = String::from_utf8_lossy(contents).replace("\r\n", "\n");
    raw = raw.replace('\r', "\n");

    let blocks = split_srt_blocks(&raw);
    let mut lines: Vec<Line> = Vec::with_capacity(blocks.len());

    for block in blocks {
        if let Some(line) = parse_srt_block(&block)? {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        return Ok(None);
    }

    let lyrics = normalize_lyrics(Lyrics {
        lang: normalize_lyric_lang(language),
        line: lines,
        synced: true,
        ..Default::default()
    });
    Ok(Some(vec![lyrics]))
}

fn srt_block_separator_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n\s*\n").unwrap())
}

/// `splitSRTBlocks`: trim, split on `\n\s*\n`, drop empty parts.
fn split_srt_blocks(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    srt_block_separator_regex()
        .split(raw)
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn parse_srt_block(block: &str) -> Result<Option<Line>, String> {
    let lines: Vec<&str> = block.split('\n').map(str::trim).collect();

    if lines.is_empty() {
        return Ok(None);
    }

    let mut start_idx = 0;
    if digits_only(lines[0]) {
        start_idx = 1;
    }
    if start_idx >= lines.len() {
        return Ok(None);
    }

    let timing: Vec<&str> = lines[start_idx].split("-->").collect();
    if timing.len() != 2 {
        return Ok(None);
    }

    let start_ms = parse_srt_time(timing[0])?;
    let end_ms = parse_srt_time(timing[1])?;

    let text_lines: Vec<&str> = lines[start_idx + 1..]
        .iter()
        .copied()
        .filter(|l| !l.is_empty())
        .collect();

    let value = sanitize_text(&text_lines.join("\n"));
    if value.is_empty() {
        return Ok(None);
    }

    Ok(Some(Line {
        start: Some(start_ms),
        end: Some(end_ms),
        value,
        ..Default::default()
    }))
}

fn parse_srt_time(value: &str) -> Result<i64, String> {
    let Some(m) = srt_time_regex().captures(value.trim()) else {
        return Err(format!("invalid SRT time: {value}"));
    };

    let hours: i64 = m[1]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let minutes: i64 = m[2]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let seconds: i64 = m[3]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let mut millis: i64 = m[4]
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;

    match m[4].len() {
        1 => millis *= 100,
        2 => millis *= 10,
        _ => {}
    }

    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

fn digits_only(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    // Port of model/lyrics_srt_test.go
    #[test]
    fn parses_srt_blocks_with_the_default_language() {
        let content = b"1\n00:00:01,000 --> 00:00:02,000\nFirst subtitle\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond subtitle";

        let list = parse_srt("xxx", content).unwrap().unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "xxx");
        assert!(list[0].synced);
        assert_eq!(
            list[0].line,
            vec![
                Line {
                    start: p(1000),
                    end: p(2000),
                    value: "First subtitle".into(),
                    ..Default::default()
                },
                Line {
                    start: p(3000),
                    end: p(4000),
                    value: "Second subtitle".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn returns_none_for_input_with_no_valid_blocks() {
        let list = parse_srt("xxx", b"not actually an srt file").unwrap();
        assert!(list.is_none());
    }

    // From model/lyrics_parse_test.go: SRT blocks separated by whitespace-only
    // blank lines.
    #[test]
    fn parses_blocks_separated_by_whitespace_only_blank_lines() {
        let content = b"1\n00:00:01,000 --> 00:00:02,000\nFirst subtitle\n   \n2\n00:00:03,000 --> 00:00:04,000\nSecond subtitle";
        let list = parse_srt("eng", content).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].line,
            vec![
                Line {
                    start: p(1000),
                    end: p(2000),
                    value: "First subtitle".into(),
                    ..Default::default()
                },
                Line {
                    start: p(3000),
                    end: p(4000),
                    value: "Second subtitle".into(),
                    ..Default::default()
                },
            ]
        );
    }
}
