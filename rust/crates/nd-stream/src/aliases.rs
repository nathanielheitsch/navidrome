//! Port of `core/stream/aliases.go` (92 LOC): container/codec alias tables and
//! the case-insensitive matchers built on them.

use std::collections::HashMap;
use std::sync::OnceLock;

/// `containerAliasGroups` (`aliases.go:9-27`): each container alias mapped to
/// its canonical group name (the first entry of the group).
#[must_use]
pub fn container_alias_groups() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let groups: &[&[&str]] = &[
            &["aac", "adts", "m4a", "mp4", "m4b", "m4p"],
            &["mpeg", "mp3", "mp2"],
            &["ogg", "oga", "opus"],
            &["aif", "aiff"],
            &["asf", "wma"],
            &["mpc", "mpp"],
            &["wv"],
        ];
        build_alias_map(groups)
    })
}

/// `codecAliasGroups` (`aliases.go:31-52`): each codec alias mapped to its
/// canonical group name (the first entry of the group).
#[must_use]
pub fn codec_alias_groups() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let groups: &[&[&str]] = &[
            &["aac", "adts"],
            &["ac3", "ac-3"],
            &["eac3", "e-ac3", "e-ac-3", "eac-3"],
            &["mpc7", "musepack7"],
            &["mpc8", "musepack8"],
            &["wma1", "wmav1"],
            &["wma2", "wmav2"],
            &["wmalossless", "wma9lossless"],
            &["wmapro", "wma9pro"],
            &["shn", "shorten"],
            &["mp4als", "als"],
        ];
        build_alias_map(groups)
    })
}

fn build_alias_map(groups: &[&[&str]]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for g in groups {
        let canonical = g[0];
        for name in *g {
            m.insert((*name).to_string(), canonical.to_string());
        }
    }
    m
}

/// `matchesWithAliases` (`aliases.go:56-69`): checks if a value matches any
/// entry in candidates, consulting the alias map for equivalent names.
#[must_use]
pub fn matches_with_aliases(
    value: &str,
    candidates: &[String],
    aliases: &HashMap<String, String>,
) -> bool {
    let value = value.to_lowercase();
    let canonical = aliases.get(&value);
    candidates.iter().any(|c| {
        let c = c.to_lowercase();
        if c == value {
            return true;
        }
        match canonical {
            Some(canonical) => aliases.get(&c) == Some(canonical),
            None => false,
        }
    })
}

/// `matchesContainer` (`aliases.go:73-75`): checks if a file suffix matches any
/// of the container names, including common aliases.
#[must_use]
pub fn matches_container(suffix: &str, containers: &[String]) -> bool {
    matches_with_aliases(suffix, containers, container_alias_groups())
}

/// `matchesCodec` (`aliases.go:79-81`): checks if a codec matches any of the
/// codec names, including common aliases.
#[must_use]
pub fn matches_codec(codec: &str, codecs: &[String]) -> bool {
    matches_with_aliases(codec, codecs, codec_alias_groups())
}

/// `IsAACCodec` (`aliases.go:84-86`): true if the given codec or container name
/// resolves to AAC.
#[must_use]
pub fn is_aac_codec(name: &str) -> bool {
    matches_codec(name, &["aac".to_string()]) || matches_container(name, &["aac".to_string()])
}

/// `containsIgnoreCase` (`aliases.go:88-92`): `slices.ContainsFunc` with
/// `strings.EqualFold`.
#[must_use]
pub fn contains_ignore_case(slice: &[String], s: &str) -> bool {
    slice.iter().any(|item| item.eq_ignore_ascii_case(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_aac_codec_true_for_aac_and_aliases() {
        // Act/Assert (`aliases_test.go:10-17`)
        assert!(is_aac_codec("aac"));
        assert!(is_aac_codec("AAC"));
        assert!(is_aac_codec("adts"));
        assert!(is_aac_codec("m4a"));
        assert!(is_aac_codec("mp4"));
        assert!(is_aac_codec("m4b"));
    }

    #[test]
    fn is_aac_codec_false_for_non_aac() {
        // Act/Assert (`aliases_test.go:20-24`)
        assert!(!is_aac_codec("mp3"));
        assert!(!is_aac_codec("opus"));
        assert!(!is_aac_codec("flac"));
        assert!(!is_aac_codec("ogg"));
    }

    #[test]
    fn is_aac_codec_false_for_empty() {
        // Act/Assert (`aliases_test.go:27-29`)
        assert!(!is_aac_codec(""));
    }

    #[test]
    fn container_aliases_resolve() {
        // Arrange
        let containers = vec!["aac".to_string()];
        // Act/Assert: the decider_test alias cases
        assert!(matches_container("m4a", &containers));
        let mp4 = vec!["mp4".to_string()];
        assert!(matches_container("m4a", &mp4));
        let ogg = vec!["ogg".to_string()];
        assert!(matches_container("opus", &ogg));
    }

    #[test]
    fn codec_aliases_resolve() {
        // Arrange
        let adts = vec!["adts".to_string()];
        // Act/Assert
        assert!(matches_codec("aac", &adts));
        assert!(matches_codec("adts", &["aac".to_string()]));
    }
}
