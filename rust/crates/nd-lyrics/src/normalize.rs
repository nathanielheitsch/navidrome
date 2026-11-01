//! #09/#22 — port of `model/lyrics_normalize.go`: cue-end and line-timing
//! normalization. Semantics match the Go source verbatim, including the
//! all-or-none end resolution and per-agent end normalization.

use crate::model::{Cue, Line, Lyrics};

/// `normalizeLyrics`: fills cue ends/timings and drops an empty agents slice.
pub fn normalize_lyrics(mut lyrics: Lyrics) -> Lyrics {
    lyrics.line = normalize_cue_lines(lyrics.line);
    if lyrics.agents.is_empty() {
        lyrics.agents = Vec::new();
    }
    lyrics
}

/// `normalizeCueLines`: normalizes each line's cue ends (with the next line's
/// start as fallback) without mutating the caller's lines.
pub fn normalize_cue_lines(lines: Vec<Line>) -> Vec<Line> {
    if lines.is_empty() {
        return lines;
    }

    let mut normalized = lines;

    for i in 0..normalized.len() {
        let fallback_end = normalized[i]
            .end
            .or_else(|| normalized.get(i + 1).and_then(|next| next.start));

        let line = std::mem::take(&mut normalized[i]);
        normalized[i] = normalize_cue_line(line, fallback_end);
    }

    normalized
}

/// `normalizeLineTiming`: fills a line's missing start/end from its cues.
pub fn normalize_line_timing(mut line: Line) -> Line {
    if line.cue.is_empty() {
        return line;
    }

    let mut earliest_start: Option<i64> = None;
    let mut latest_end: Option<i64> = None;
    for token in &line.cue {
        if let Some(s) = token.start {
            if earliest_start.is_none_or(|e| s < e) {
                earliest_start = Some(s);
            }
        }

        let candidate_end = token.end.or(token.start);
        if let Some(e) = candidate_end {
            if latest_end.is_none_or(|l| e > l) {
                latest_end = Some(e);
            }
        }
    }

    if line.start.is_none() {
        line.start = earliest_start;
    }
    if line.end.is_none() {
        line.end = latest_end;
    }
    line
}

fn normalize_cue_line(mut line: Line, fallback_end: Option<i64>) -> Line {
    if line.cue.is_empty() {
        return line;
    }
    line.cue = normalize_cue_ends_by_agent(std::mem::take(&mut line.cue), fallback_end);
    normalize_line_timing(line)
}

/// `normalizeCueEndsByAgent`: resolves cue end times independently per agent
/// so that background (or other parallel) layers, whose cues interleave with
/// the main timeline but are stored together in document order, do not clamp
/// each other's ends. Each agent group is normalized in its own document
/// order; results are reassembled into the original cue positions.
fn normalize_cue_ends_by_agent(cues: Vec<Cue>, fallback_end: Option<i64>) -> Vec<Cue> {
    // Group cue indices by agent id, in first-seen order.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, cue) in cues.iter().enumerate() {
        match groups.iter_mut().find(|(id, _)| *id == cue.agent_id) {
            Some((_, idxs)) => idxs.push(i),
            None => groups.push((cue.agent_id.clone(), vec![i])),
        }
    }

    // Single agent: the document order already matches the timeline, so the
    // straightforward normalization applies without regrouping.
    if groups.len() <= 1 {
        return normalize_cue_ends(cues, fallback_end);
    }

    let mut out = cues;
    for (_, idxs) in groups {
        let group: Vec<Cue> = idxs.iter().map(|&i| out[i].clone()).collect();
        let group = normalize_cue_ends(group, fallback_end);
        for (gi, &pos) in idxs.iter().enumerate() {
            out[pos] = group[gi].clone();
        }
    }
    out
}

/// `NormalizeCueEnds` (Go export, used by the Subsonic enhanced-lyrics
/// serializer): resolves missing cue end times within a single ordered cue
/// group — each end is filled from the next cue's start, then from
/// `fallback_end`, and is clamped so it never precedes the cue's own start
/// nor overruns the next cue. End times are all-or-none — if any cue still
/// lacks an end afterwards, all ends in the group are cleared. The input
/// slice is never mutated.
pub fn normalize_cue_ends(cues: Vec<Cue>, fallback_end: Option<i64>) -> Vec<Cue> {
    if cues.is_empty() {
        return cues;
    }

    let mut out = cues;
    let len = out.len();
    for i in 0..len {
        let mut end = out[i].end;
        if end.is_none() {
            if i + 1 < len {
                end = out[i + 1].start;
            }
            if end.is_none() {
                end = fallback_end;
            }
        }
        if let Some(e) = end {
            if i + 1 < len && out[i + 1].start.is_some_and(|s| e > s) {
                end = out[i + 1].start;
            }
            if out[i].start.is_some_and(|s| e < s) {
                end = out[i].start;
            }
        }
        out[i].end = end;
    }

    // All-or-none: if any cue still lacks an end, clear every end.
    if out.iter().any(|c| c.end.is_none()) {
        for cue in &mut out {
            cue.end = None;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    // Port of model/lyrics_normalize_test.go
    #[test]
    fn normalize_cue_lines_fills_missing_ends() {
        let lines = vec![
            Line {
                start: p(1000),
                value: "Some lyrics".into(),
                cue: vec![
                    Cue {
                        start: p(1000),
                        value: "Some ".into(),
                        byte_start: 0,
                        byte_end: 4,
                        ..Default::default()
                    },
                    Cue {
                        start: p(1500),
                        value: "lyrics".into(),
                        byte_start: 5,
                        byte_end: 10,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            Line {
                start: p(3000),
                value: "Next line".into(),
                ..Default::default()
            },
        ];

        let normalized = normalize_cue_lines(lines);

        assert_eq!(normalized[0].cue[0].end, p(1500));
        assert_eq!(normalized[0].cue[1].end, p(3000));
    }

    // Port of model/lyrics_normalize_test.go: NormalizeCueEnds table.
    #[test]
    fn normalize_cue_ends_returns_empty_input() {
        assert!(normalize_cue_ends(Vec::new(), p(1000)).is_empty());
    }

    #[test]
    fn normalize_cue_ends_fills_a_missing_end_from_the_next_cue_start() {
        let cues = vec![
            Cue {
                start: p(1000),
                ..Default::default()
            },
            Cue {
                start: p(1500),
                ..Default::default()
            },
        ];
        let out = normalize_cue_ends(cues, p(3000));
        assert_eq!(out[0].end, p(1500));
        assert_eq!(out[1].end, p(3000));
    }

    #[test]
    fn normalize_cue_ends_fills_last_cue_end_from_fallback() {
        let cues = vec![
            Cue {
                start: p(1000),
                end: p(1200),
                ..Default::default()
            },
            Cue {
                start: p(1500),
                ..Default::default()
            },
        ];
        let out = normalize_cue_ends(cues, p(3000));
        assert_eq!(out[0].end, p(1200));
        assert_eq!(out[1].end, p(3000));
    }

    #[test]
    fn normalize_cue_ends_clamps_end_overrunning_next_cue_start() {
        let cues = vec![
            Cue {
                start: p(1000),
                end: p(9999),
                ..Default::default()
            },
            Cue {
                start: p(1500),
                end: p(2000),
                ..Default::default()
            },
        ];
        let out = normalize_cue_ends(cues, p(3000));
        assert_eq!(out[0].end, p(1500));
        assert_eq!(out[1].end, p(2000));
    }

    #[test]
    fn normalize_cue_ends_clamps_end_preceding_own_start() {
        let cues = vec![Cue {
            start: p(1000),
            end: p(500),
            ..Default::default()
        }];
        let out = normalize_cue_ends(cues, p(3000));
        assert_eq!(out[0].end, p(1000));
    }

    #[test]
    fn normalize_cue_ends_clears_all_ends_when_any_cue_lacks_one() {
        // The last cue has no end and there is no fallback, so it stays None
        // and every end in the group is cleared.
        let cues = vec![
            Cue {
                start: p(1000),
                end: p(1200),
                ..Default::default()
            },
            Cue {
                start: p(1500),
                ..Default::default()
            },
        ];
        let out = normalize_cue_ends(cues, None);
        assert_eq!(out[0].end, None);
        assert_eq!(out[1].end, None);
    }
}
