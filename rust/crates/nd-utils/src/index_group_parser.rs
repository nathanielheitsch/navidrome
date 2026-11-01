//! Index-group specification parsing — port of `utils/index_group_parser.go`.
//!
//! The specification is a space-separated list of index entries. Normally
//! each entry is a single character, but multiple characters are allowed; the
//! entry `The` links to all files and folders starting with `The`. An entry
//! may also use a group in parentheses: `A-E(ABCDE)` displays as `A-E` and
//! links to files starting with `A`, `B`, `C`, `D` or `E`. Files not covered
//! by an entry fall under `#`.

use std::collections::HashMap;

/// A parsed index-group specification: character (or multi-character key) →
/// displayed group.
pub type IndexGroups = HashMap<String, String>;

/// Parses an index-group specification string (`utils.ParseIndexGroups`).
#[must_use]
pub fn parse_index_groups(spec: &str) -> IndexGroups {
    let mut parsed = IndexGroups::new();
    for g in spec.split(' ') {
        if let Some((display, group)) = match_index_group(g) {
            for c in group.chars() {
                parsed.insert(c.to_string(), display.clone());
            }
        } else {
            parsed.insert(g.to_string(), g.to_string());
        }
    }
    parsed
}

/// Matches the `(.+)\((.+)\)` pattern of Go's `indexGroupsRx`.
fn match_index_group(g: &str) -> Option<(String, String)> {
    let open = g.find('(')?;
    let close = g.rfind(')')?;
    if close < open || open == 0 || close != g.len() - 1 {
        return None;
    }
    let display = &g[..open];
    let group = &g[open + 1..close];
    if display.is_empty() || group.is_empty() {
        return None;
    }
    Some((display.to_string(), group.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_simple_entries_return_the_entries() {
        let parsed = parse_index_groups("A The");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed["A"], "A");
        assert_eq!(parsed["The"], "The");
    }

    #[test]
    fn an_entry_with_a_group_parses_the_groups() {
        let parsed = parse_index_groups("A-C(ABC) Z");
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed["A"], "A-C");
        assert_eq!(parsed["B"], "A-C");
        assert_eq!(parsed["C"], "A-C");
        assert_eq!(parsed["Z"], "Z");
    }

    #[test]
    fn correctly_parses_utf8() {
        let parsed = parse_index_groups("UTF8(宇A海)");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed["宇"], "UTF8");
        assert_eq!(parsed["A"], "UTF8");
        assert_eq!(parsed["海"], "UTF8");
    }
}
