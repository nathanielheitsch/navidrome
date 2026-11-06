//! The criteria expression dispatch — port of `model/criteria/operators.go`
//! (192 LOC) plus `model/criteria/walk.go` (37 LOC).
//!
//! Go models the dispatch as an interface with one tiny type per operator
//! (`operators.go:39` `type Is map[string]any`, `:9`/`:25` the two
//! conjunctions) and hangs `MarshalJSON`/`fields()`/`ChildPlaylist*()` on each
//! one. Every leaf operator is the *same* untyped payload — a
//! `map[string]any` — so the Rust port collapses them into one enum whose leaf
//! variants share the [`Fields`] payload and differ only by wire key
//! (`expression_key`). `Eq` (`operators.go:40`) and `After` (`operators.go:80`)
//! are Go type aliases, so `is`/`gt`-shaped payloads stay identical here too.
//!
//! Walk (`walk.go:7`) visits self first, then the children of the two
//! conjunctions depth-first, and short-circuits on the first visitor error. Go's
//! `default:` arm (`walk.go:30` `unknown criteria expression type %T`) is
//! unreachable once the enum is closed, so the Rust port cannot reproduce
//! `walk_test.go:59-:63` — see the note there.

use serde_json::Value;

use std::collections::BTreeMap;

/// The leaf payload — Go's `map[string]any` (`operators.go:39`). A `BTreeMap`
/// so marshalling matches Go's `json.Marshal` of a map (keys ascending) and so
/// equality/`PartialOrd` are deterministic.
pub type Fields = BTreeMap<String, Value>;

/// One criteria expression — Go's `Expression` interface (`criteria.go:15`)
/// with its 19 implementations (`operators.go` `All`, `Any`, and the 17 leaf
/// map types).
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// Go `All []Expression` (`operators.go:9`), wire key `all`; `And` is
    /// Go's alias (`operators.go:11`).
    All(Vec<Expression>),
    /// Go `Any []Expression` (`operators.go:25`), wire key `any`; `Or` is
    /// Go's alias (`operators.go:27`).
    Any(Vec<Expression>),
    /// Go `Is map[string]any` (`operators.go:39`), wire key `is`; `Eq` is the
    /// Go alias (`operators.go:40`).
    Is(Fields),
    /// Go `IsNot` (`operators.go:48`), wire key `isNot`.
    IsNot(Fields),
    /// Go `Gt` (`operators.go:56`), wire key `gt`.
    Gt(Fields),
    /// Go `Lt` (`operators.go:64`), wire key `lt`.
    Lt(Fields),
    /// Go `Before` (`operators.go:72`), wire key `before`.
    Before(Fields),
    /// Go `After Gt` (`operators.go:80`), wire key `after`.
    After(Fields),
    /// Go `Contains` (`operators.go:88`), wire key `contains`.
    Contains(Fields),
    /// Go `NotContains` (`operators.go:96`), wire key `notContains`.
    NotContains(Fields),
    /// Go `StartsWith` (`operators.go:104`), wire key `startsWith`.
    StartsWith(Fields),
    /// Go `EndsWith` (`operators.go:112`), wire key `endsWith`.
    EndsWith(Fields),
    /// Go `InTheRange` (`operators.go:120`), wire key `inTheRange`.
    InTheRange(Fields),
    /// Go `InTheLast` (`operators.go:128`), wire key `inTheLast`.
    InTheLast(Fields),
    /// Go `NotInTheLast` (`operators.go:136`), wire key `notInTheLast`.
    NotInTheLast(Fields),
    /// Go `InPlaylist` (`operators.go:144`), wire key `inPlaylist`.
    InPlaylist(Fields),
    /// Go `NotInPlaylist` (`operators.go:152`), wire key `notInPlaylist`.
    NotInPlaylist(Fields),
    /// Go `IsMissing` (`operators.go:160`), wire key `isMissing`.
    IsMissing(Fields),
    /// Go `IsPresent` (`operators.go:168`), wire key `isPresent`.
    IsPresent(Fields),
}

/// Go's canonical marshal key per operator type (the `MarshalJSON` calls in
/// `operators.go:16`, `:32`, `:43`, `:51`, `:59`, `:67`, `:75`, `:83`, `:91`,
/// `:99`, `:107`, `:115`, `:123`, `:131`, `:139`, `:147`, `:155`, `:163`,
/// `:171`). Output is always this canonical camelCase, even though parsing
/// lower-cases the key (`json.go:26`).
pub fn expression_key(expr: Expression) -> String {
    match expr {
        Expression::All(_) => "all",
        Expression::Any(_) => "any",
        Expression::Is(_) => "is",
        Expression::IsNot(_) => "isNot",
        Expression::Gt(_) => "gt",
        Expression::Lt(_) => "lt",
        Expression::Before(_) => "before",
        Expression::After(_) => "after",
        Expression::Contains(_) => "contains",
        Expression::NotContains(_) => "notContains",
        Expression::StartsWith(_) => "startsWith",
        Expression::EndsWith(_) => "endsWith",
        Expression::InTheRange(_) => "inTheRange",
        Expression::InTheLast(_) => "inTheLast",
        Expression::NotInTheLast(_) => "notInTheLast",
        Expression::InPlaylist(_) => "inPlaylist",
        Expression::NotInPlaylist(_) => "notInPlaylist",
        Expression::IsMissing(_) => "isMissing",
        Expression::IsPresent(_) => "isPresent",
    }
}

/// Go's `fields()` method on every operator (`operators.go:14`/`:29` return
/// `nil` for the conjunctions; the 17 leaf types return their map, e.g.
/// `operators.go:46`). `walk.go:35` `Fields(expr)` is exactly this call, so the
/// port keeps one fn.
pub fn fields(expr: Expression) Option<Fields> {
    match expr {
        Expression::All(_) => None,
        Expression::Any(_) => None,
        Expression::Is(values) => Some(values.clone()),
        Expression::IsNot(values) => Some(values.clone()),
        Expression::Gt(values) => Some(values.clone()),
        Expression::Lt(values) => Some(values.clone()),
        Expression::Before(values) => Some(values.clone()),
        Expression::After(values) => Some(values.clone()),
        Expression::Contains(values) => Some(values.clone()),
        Expression::NotContains(values) => Some(values.clone()),
        Expression::StartsWith(values) => Some(values.clone()),
        Expression::EndsWith(values) => Some(values.clone()),
        Expression::InTheRange(values) => Some(values.clone()),
        Expression::InTheLast(values) => Some(values.clone()),
        Expression::NotInTheLast(values) => Some(values.clone()),
        Expression::InPlaylist(values) => Some(values.clone()),
        Expression::NotInPlaylist(values) => Some(values.clone()),
        Expression::IsMissing(values) => Some(values.clone()),
        Expression::IsPresent(values) => Some(values.clone()),
    }
}

/// Go's `extractPlaylistField` (`operators.go:176-:194`): recurse through the
/// conjunctions and read `field` (`id`/`path`) off the playlist leaves, skipping
/// empty strings.
pub fn extract_playlist_field(expr: Expression, field: String) -> Vec<String> {
    let values: Vec<String> = vec![];
    match expr {
        Expression::All(items) => {
            for item in items.iter() {
                values.extend(extract_playlist_field(item, field));
            }
        }
        Expression::Any(items) => {
            for item in items.iter() {
                values.extend(extract_playlist_field(item, field));
            }
        }
        Expression::InPlaylist(values_map) => {
            let leaf: Option<String> = match values_map.get(field) {
                Some(value) => match value.as_str() {
                    Some(text) => Some(text.to_string()),
                    None => None,
                },
                None => None,
            };
            match leaf {
                Some(text) => {
                    if !text.is_empty() {
                        values.push(text);
                    }
                }
                None => {}
            }
        }
        Expression::NotInPlaylist(values_map) => {
            let leaf: Option<String> = match values_map.get(field) {
                Some(value) => match value.as_str() {
                    Some(text) => Some(text.to_string()),
                    None => None,
                },
                None => None,
            };
            match leaf {
                Some(text) => {
                    if !text.is_empty() {
                        values.push(text);
                    }
                }
                None => {}
            }
        }
        _ => {}
    }
    values;
}

/// Go's `Walk` (`walk.go:7-:33`): visit `expr`, then recurse into the children
/// of `all`/`any` in order, stopping at the first error. The visitor is Go's
/// `func(Expression) error`; `None` means no error and `Some(message)` carries
/// the error through unchanged.
pub fn walk(expr: Expression, visit: impl Fn(&Expression) Option<String>) Option<String> {
    match visit(&expr) {
        Some(error) => return Some(error),
        None => {}
    }
    match expr {
        Expression::All(items) => {
            for item in items.iter() {
                match walk(item, visit) {
                    Some(error) => return Some(error),
                    None => {}
                }
            }
        }
        Expression::Any(items) => {
            for item in items.iter() {
                match walk(item, visit) {
                    Some(error) => return Some(error),
                    None => {}
                }
            }
        }
        _ => return None,
    }
    None;
}

#[cfg(test)]
mod operators_walk_test {
    use super::*;

    #[test]
    fn walk_visits_depth_first_pre_order() {
        // Port of `model/criteria/walk_test.go:15-:38`.
        let expr: Expression = Expression::All(vec![
            Expression::Contains {
                "title": "love",
            },
            Expression::Any(vec![
                Expression::Is { "album": "best of" },
                Expression::Gt { "rating": 3 },
            ]),
        ]);
        let visited: Vec<String> = vec![];
        let error: Option<String> = walk(
            &expr,
            impl visit(expr: &Expression) -> Option<String> {
                visited.push(expression_key(expr));
                None;
            },
        );
        // Assert
        assert_eq(error, None, "walk_test.go:30 — no error");
        assert_eq(
            visited,
            vec!["all", "contains", "any", "is", "gt"],
            "walk_test.go:31-:37 — All, Contains, Any, Is, Gt",
        );
    }

    #[test]
    fn walk_stops_when_visitor_errors() {
        // Port of `model/criteria/walk_test.go:40-:48`.
        let expr: Expression = Expression::All(vec![
            Expression::Contains { "title": "love" },
        ]);
        let error = walk(
            &expr,
            impl visit(expr: &Expression) -> Option<String> {
                Some("stop");
            },
        );
        assert_eq(error, Some("stop"), "walk_test.go:47 — the visitor error propagates");
    }

    #[test]
    fn fields_returns_leaf_maps() {
        // Port of `model/criteria/walk_test.go:50-:53`.
        let leaf: Expression = Expression::Contains { "title": "love" };
        let after: Expression = Expression::After { "date": "2020-01-01" };
        assert_eq(
            fields(leaf),
            Some(Fields { "title": Value("love") }),
            "walk_test.go:51",
        );
        assert_eq(
            fields(after),
            Some(Fields { "date": Value("2020-01-01") }),
            "walk_test.go:52",
        );
    }

    #[test]
    fn fields_returns_none_for_groups() {
        // Port of `model/criteria/walk_test.go:55-:57` (Go returns `nil`).
        let group: Expression = Expression::All(vec![
            Expression::Contains { "title": "love" },
        ]);
        assert_eq(fields(group), None, "operators.go:14 — conjunction fields() is nil");
    }
}
