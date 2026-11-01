//! #09/#22 — port of `model/lyrics.go`: lyrics DTOs (Go `model.Cue`,
//! `model.Agent`, `model.Line`, `model.Lyrics`, `model.LyricList`) and their
//! pure helpers. Field names, JSON tags, kinds, and defaulting semantics match
//! the Go source verbatim.

use serde::{Deserialize, Serialize};

/// A word-level timing cue (Go `model.Cue`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    /// Cue start, in milliseconds (Go `start,omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Cue end, in milliseconds (Go `end,omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    /// The cue's text (Go `value`).
    #[serde(default)]
    pub value: String,
    /// Inclusive UTF-8 byte start offset into `Line::value` (Go `byteStart`).
    #[serde(default)]
    pub byte_start: usize,
    /// Inclusive UTF-8 byte end offset into `Line::value` (Go `byteEnd`).
    #[serde(default)]
    pub byte_end: usize,
    /// Owning agent (Go `agentId,omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_id: String,
}

/// A singing/speaking agent (Go `model.Agent`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Agent id (Go `id`).
    #[serde(default)]
    pub id: String,
    /// Agent role (`main`, `voice`, `group`, `bg`) (Go `role`).
    #[serde(default)]
    pub role: String,
    /// Optional display name (Go `name,omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// A lyric line, optionally with word-level cues (Go `model.Line`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Line start, in milliseconds (Go `start,omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Line end, in milliseconds (Go `end,omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    /// The lyric text (Go `value`).
    #[serde(default)]
    pub value: String,
    /// Word-level timing cues (Go `cue,omitempty`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cue: Vec<Cue>,
}

/// Lyric kinds, as defined by the OpenSubsonic songLyrics v2 contract (Go
/// `model.LyricKind*`). These are the canonical wire values; keep them in sync
/// with the spec.
pub mod kind {
    /// The main lyric track (`model.LyricKindMain`).
    pub const MAIN: &str = "main";
    /// A translation track (`model.LyricKindTranslation`).
    pub const TRANSLATION: &str = "translation";
    /// A pronunciation track (`model.LyricKindPronunciation`).
    pub const PRONUNCIATION: &str = "pronunciation";
}

/// One lyric track (Go `model.Lyrics`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    /// Display artist override (Go `displayArtist,omitempty`).
    #[serde(
        rename = "displayArtist",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub display_artist: String,
    /// Display title override (Go `displayTitle,omitempty`).
    #[serde(
        rename = "displayTitle",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub display_title: String,
    /// Track kind (Go `kind,omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Language code (`xxx` when unknown) (Go `lang`).
    #[serde(default)]
    pub lang: String,
    /// Agents referenced by cues (Go `agents,omitempty`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<Agent>,
    /// The lyric lines (Go `line`).
    #[serde(default)]
    pub line: Vec<Line>,
    /// Time offset, in milliseconds (Go `offset,omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    /// Whether the track carries timing (Go `synced`).
    #[serde(default)]
    pub synced: bool,
}

impl Lyrics {
    /// `model.Lyrics.IsEmpty`: true when no lines.
    pub fn is_empty(&self) -> bool {
        self.line.is_empty()
    }

    /// `model.Lyrics.IsMainKind`: reports whether the lyric is the main
    /// track. A blank kind is an untyped (single-track) lyric, which the
    /// contract treats as main.
    pub fn is_main_kind(&self) -> bool {
        self.effective_kind() == kind::MAIN
    }

    /// `model.Lyrics.EffectiveKind`: returns the lyric kind, defaulting to
    /// main when blank. A blank kind means an untyped (single-track) lyric,
    /// which the contract treats as main.
    pub fn effective_kind(&self) -> &str {
        if self.kind.trim().is_empty() {
            kind::MAIN
        } else {
            &self.kind
        }
    }
}

/// A list of lyric tracks (Go `model.LyricList`).
pub type LyricList = Vec<Lyrics>;

/// `model.LyricList.Main`: returns the main-kind lyric, falling back to the
/// first entry so untyped lyrics still resolve. `None` only when the list is
/// empty. It is used to surface a single lyric through the plain-text legacy
/// getLyrics endpoint, which has no notion of translation/pronunciation
/// tracks.
pub fn lyric_list_main(list: &LyricList) -> Option<&Lyrics> {
    list.iter().find(|l| l.is_main_kind()).or(list.first())
}

/// `model.LyricList.MarshalJSON`'s invariant for the lyrics column: an
/// empty/nil list serializes to `[]`, never `null`.
pub fn lyric_list_to_json(list: &LyricList) -> String {
    if list.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(list).unwrap_or_else(|_| "[]".to_string())
    }
}

/// `normalizeLyricLang` (Go `model/lyrics_ttml.go`): lowercases and trims,
/// yielding `xxx` when empty.
pub fn normalize_lyric_lang(lang: &str) -> String {
    let lang = lang.trim().to_lowercase();
    if lang.is_empty() {
        "xxx".to_string()
    } else {
        lang
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of model/lyrics_test.go: Lyrics.EffectiveKind
    #[test]
    fn effective_kind_defaults_a_blank_kind_to_main() {
        assert_eq!(Lyrics::default().effective_kind(), kind::MAIN);
        assert_eq!(
            Lyrics {
                kind: "  ".into(),
                ..Default::default()
            }
            .effective_kind(),
            kind::MAIN
        );
    }

    #[test]
    fn effective_kind_returns_the_kind_as_is_when_set() {
        let l = Lyrics {
            kind: kind::TRANSLATION.into(),
            ..Default::default()
        };
        assert_eq!(l.effective_kind(), kind::TRANSLATION);
    }

    // Port of model/lyrics_test.go: Lyrics.IsMainKind
    #[test]
    fn is_main_kind_is_true_for_a_blank_untyped_kind() {
        assert!(Lyrics::default().is_main_kind());
    }

    #[test]
    fn is_main_kind_is_true_for_the_main_kind() {
        let l = Lyrics {
            kind: kind::MAIN.into(),
            ..Default::default()
        };
        assert!(l.is_main_kind());
    }

    #[test]
    fn is_main_kind_is_false_for_translation_and_pronunciation_kinds() {
        assert!(!Lyrics {
            kind: kind::TRANSLATION.into(),
            ..Default::default()
        }
        .is_main_kind());
        assert!(!Lyrics {
            kind: kind::PRONUNCIATION.into(),
            ..Default::default()
        }
        .is_main_kind());
    }

    // Port of model/lyrics_test.go: LyricList.Main
    #[test]
    fn main_returns_none_when_the_list_is_empty() {
        let list: LyricList = vec![];
        assert!(lyric_list_main(&list).is_none());
    }

    #[test]
    fn main_returns_the_main_kind_entry_when_present() {
        let list = vec![
            Lyrics {
                kind: kind::TRANSLATION.into(),
                lang: "en".into(),
                ..Default::default()
            },
            Lyrics {
                kind: kind::MAIN.into(),
                lang: "xxx".into(),
                ..Default::default()
            },
        ];
        let main = lyric_list_main(&list).unwrap();
        assert_eq!(main.kind, kind::MAIN);
    }

    #[test]
    fn main_falls_back_to_the_first_entry_when_no_main_kind_exists() {
        let list = vec![
            Lyrics {
                kind: kind::TRANSLATION.into(),
                lang: "en".into(),
                ..Default::default()
            },
            Lyrics {
                kind: kind::PRONUNCIATION.into(),
                lang: "ja".into(),
                ..Default::default()
            },
        ];
        let main = lyric_list_main(&list).unwrap();
        assert_eq!(main.lang, "en");
    }

    #[test]
    fn main_treats_a_blank_kind_as_main() {
        let list = vec![
            Lyrics {
                kind: kind::TRANSLATION.into(),
                lang: "en".into(),
                ..Default::default()
            },
            Lyrics {
                lang: "xxx".into(),
                ..Default::default()
            },
        ];
        let main = lyric_list_main(&list).unwrap();
        assert_eq!(main.lang, "xxx");
    }

    // MarshalJSON invariant: empty list serializes to [], never null.
    #[test]
    fn lyric_list_to_json_never_emits_null_for_empty() {
        let list: LyricList = vec![];
        assert_eq!(lyric_list_to_json(&list), "[]");
    }
}
