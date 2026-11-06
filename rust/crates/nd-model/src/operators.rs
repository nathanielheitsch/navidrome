//! The criteria expression dispatch — port of `model/criteria/operators.go`
//! (192 LOC) plus `model/criteria/walk.go` (37 LOC).
//!
//! Go models the dispatch as an interface with one tiny type per operator
//! (`operators.go:39` `type Is map[string]any`, `:9`/`:25` the two
//! conjunctions) and hangs `MarshalJSON`/`fields()`/`ChildPlaylist*()` on each
//! one. Every leaf operator carries the *same* untyped payload — a
//! `map[string]any` — so the Rust port collapses the 17 leaf types into one
//! enum whose leaf variants share the [`Fields`] payload and differ only by
//! wire key (`expression_key`). `Eq` (`operators.go:40`) and `After`
//! (`operators.go:80`) are Go type aliases, so `is`/`gt`-shaped payloads stay
//! identical here too.
//!
//! Walk (`walk.go:7`) visits self first, then the children of the two
//! conjunctions depth-first, and short-circuits on the first visitor error.
//! Go's `default:` arm (`walk.go:30` `unknown criteria expression type %T`) is
//! unreachable once the enum is closed, so `walk_test.go:59-:63` has no port.

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
#[must_use]
pub fn expression_key(expr: Expression) -> String {
    match expr {
        Expression::All(_) => "all".to_string(),
        Expression::Any(_) => "any".to_string(),
        Expression::Is(_) => "is".to_string(),
        Expression::IsNot(_) => "isNot".to_string(),
        Expression::Gt(_) => "gt".to_string(),
        Expression::Lt(_) => "lt".to_string(),
        Expression::Before(_) => "before".to_string(),
        Expression::After(_) => "after".to_string(),
        Expression::Contains(_) => "contains".to_string(),
        Expression::NotContains(_) => "notContains".to_string(),
        Expression::StartsWith(_) => "startsWith".to_string(),
        Expression::EndsWith(_) => "endsWith".to_string(),
        Expression::InTheRange(_) => "inTheRange".to_string(),
        Expression::InTheLast(_) => "inTheLast".to_string(),
        Expression::NotInTheLast(_) => "notInTheLast".to_string(),
        Expression::InPlaylist(_) => "inPlaylist".to_string(),
        Expression::NotInPlaylist(_) => "notInPlaylist".to_string(),
        Expression::IsMissing(_) => "isMissing".to_string(),
        Expression::IsPresent(_) => "isPresent".to_string(),
    }
}

/// Go's `fields()` method on every operator (`operators.go:14`/`:29` return
/// `nil` for the conjunctions; the 17 leaf types return their map, e.g.
/// `operators.go:46`). `walk.go:35` `Fields(expr)` is exactly this call, so the
/// port keeps one fn.
#[must_use]
pub fn fields(expr: Expression) -> Option<Fields> {
    match expr {
        Expression::All(_) => None,
        Expression::Any(_) => None,
        Expression::Is(values) => Some(values),
        Expression::IsNot(values) => Some(values),
        Expression::Gt(values) => Some(values),
        Expression::Lt(values) => Some(values),
        Expression::Before(values) => Some(values),
        Expression::After(values) => Some(values),
        Expression::Contains(values) => Some(values),
        Expression::NotContains(values) => Some(values),
        Expression::StartsWith(values) => Some(values),
        Expression::EndsWith(values) => Some(values),
        Expression::InTheRange(values) => Some(values),
        Expression::InTheLast(values) => Some(values),
        Expression::NotInTheLast(values) => Some(values),
        Expression::InPlaylist(values) => Some(values),
        Expression::NotInPlaylist(values) => Some(values),
        Expression::IsMissing(values) => Some(values),
        Expression::IsPresent(values) => Some(values),
    }
}

/// Go's `extractPlaylistField` (`operators.go:176-:194`): recurse through the
/// conjunctions and read `field` (`id`/`path`) off the playlist leaves, skipping
/// empty strings.
#[must_use]
pub fn extract_playlist_field(expr: Expression, field: String) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    match expr {
        Expression::All(items) => {
            for item in items {
                values.extend(extract_playlist_field(item.clone(), field.clone()));
            }
        }
        Expression::Any(items) => {
            for item in items {
                values.extend(extract_playlist_field(item.clone(), field.clone()));
            }
        }
        Expression::InPlaylist(payload) => {
            if let Some(value) = playlist_ref(&payload, field) {
                if !value.is_empty() {
                    values.push(value);
                }
            }
        }
        Expression::NotInPlaylist(payload) => {
            if let Some(value) = playlist_ref(&payload, field) {
                if !value.is_empty() {
                    values.push(value);
                }
            }
        }
        _ => {}
    }
    values
}

/// The `field` slot of a playlist leaf rendered as a string — Go's typed cast
/// `rule.fields()[field].(string)` (`operators.go:187`): an absent key or a
/// non-string JSON value both yield `None`, and the caller skips the leaf.
#[must_use]
fn playlist_ref(payload: &Fields, field: String) -> Option<String> {
    match payload.get(&field) {
        Some(value) => value.as_str().map(|text| text.to_string()),
        None => None,
    }
}

/// Go's `Walk` (`walk.go:7-:33`): visit `expr`, then recurse into the children
/// of `all`/`any` in order, stopping at the first error. The visitor is Go's
/// `func(Expression) error` (`walk.go:5` `type Visitor`); `None` means no error
/// and `Some(message)` carries the error through unchanged. Go's `expr == nil`
/// guard (`walk.go:8-:10`) has no Rust equivalent once the enum is closed.
pub fn walk(expr: Expression, visit: &dyn Fn(&Expression) -> Option<String>) -> Option<String> {
    if let Some(error) = visit(&expr) {
        return Some(error);
    }
    match expr {
        Expression::All(items) => {
            for item in items {
                if let Some(error) = walk(item, visit) {
                    return Some(error);
                }
            }
        }
        Expression::Any(items) => {
            for item in items {
                if let Some(error) = walk(item, visit) {
                    return Some(error);
                }
            }
        }
        _ => return None,
    }
    None
}

#[cfg(test)]
mod operators_test {
    use super::*;

    /// A single-entry string leaf payload (Go's `Contains{"title": "love"}` literal).
    fn one_string_field(name: String, text: String) -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert(name, Value::String(text.to_string()));
        leaf
    }

    /// A leaf whose slot holds a JSON boolean — the non-string case of the Go
    /// `.(string)` typed cast (`operators.go:187`).
    fn bool_field(name: String) -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert(name, Value::Bool(true));
        leaf
    }

    #[test]
    fn expression_key_is_canonical_camel_case() {
        // Arrange: one expression per operator type, mirroring the 19
        // `MarshalJSON` bodies of `model/criteria/operators.go`.
        let leaf: Fields = one_string_field("title".to_string(), "love".to_string());
        let exprs: Vec<Expression> = vec![
            Expression::All(Vec::new()),
            Expression::Any(Vec::new()),
            Expression::Is(leaf.clone()),
            Expression::IsNot(leaf.clone()),
            Expression::Gt(leaf.clone()),
            Expression::Lt(leaf.clone()),
            Expression::Before(leaf.clone()),
            Expression::After(leaf.clone()),
            Expression::Contains(leaf.clone()),
            Expression::NotContains(leaf.clone()),
            Expression::StartsWith(leaf.clone()),
            Expression::EndsWith(leaf.clone()),
            Expression::InTheRange(leaf.clone()),
            Expression::InTheLast(leaf.clone()),
            Expression::NotInTheLast(leaf.clone()),
            Expression::InPlaylist(leaf.clone()),
            Expression::NotInPlaylist(leaf.clone()),
            Expression::IsMissing(leaf.clone()),
            Expression::IsPresent(leaf.clone()),
        ];
        // Act:
        let mut keys: Vec<String> = Vec::new();
        for expr in exprs {
            keys.push(expression_key(expr.clone()));
        }
        // Assert
        assert_eq!(
            keys,
            vec![
                "all",
                "any",
                "is",
                "isNot",
                "gt",
                "lt",
                "before",
                "after",
                "contains",
                "notContains",
                "startsWith",
                "endsWith",
                "inTheRange",
                "inTheLast",
                "notInTheLast",
                "inPlaylist",
                "notInPlaylist",
                "isMissing",
                "isPresent",
            ],
            "operators.go — MarshalJSON keys stay camelCase; json.go:26 lower-cases only on parse",
        );
    }

    #[test]
    fn walk_visits_self_before_children_in_pre_order() {
        // Port of `model/criteria/walk_test.go:15-:38`.
        // Arrange:
        let title: Fields = one_string_field("title".to_string(), "love".to_string());
        let album: Fields = one_string_field("album".to_string(), "best of".to_string());
        let expr: Expression = Expression::All(vec![
            Expression::Contains(title.clone()),
            Expression::Any(vec![
                Expression::Is(album.clone()),
                Expression::Gt(album.clone()),
            ]),
        ]);
        let visited: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        // Act:
        let error: Option<String> = walk(expr, &|item: &Expression| -> Option<String> {
            visited.borrow_mut().push(expression_key(item.clone()));
            None
        });
        // Assert
        assert_eq!(error, None, "walk_test.go:30 — the visitor never errors");
        assert_eq!(
            *visited.borrow(),
            vec![
                "all".to_string(),
                "contains".to_string(),
                "any".to_string(),
                "is".to_string(),
                "gt".to_string(),
            ],
            "walk_test.go:31-:37 — All, Contains, Any, Is, Gt in pre-order",
        );
    }

    #[test]
    fn walk_stops_at_the_first_visitor_error() {
        // Port of `model/criteria/walk_test.go:40-:48`, plus the short-circuit
        // of `walk.go:17-:19`: once a child errors, later siblings are not
        // visited, so the first error wins.
        // Arrange:
        let leaf: Fields = one_string_field("title".to_string(), "love".to_string());
        let expr: Expression = Expression::All(vec![
            Expression::Is(leaf.clone()),
            Expression::IsNot(leaf.clone()),
            Expression::Gt(leaf.clone()),
        ]);
        // Act:
        let error: Option<String> = walk(expr, &|item: &Expression| -> Option<String> {
            match expression_key(item.clone()).as_str() {
                "isNot" => Some("stop".to_string()),
                _ => None,
            }
        });
        // Assert
        assert_eq!(
            error,
            Some("stop".to_string()),
            "walk.go:17 — the first error short-circuits"
        );
    }

    #[test]
    fn fields_returns_leaf_maps_and_none_for_groups() {
        // Port of `model/criteria/walk_test.go:50-:57` (the Go `nil` return of
        // `operators.go:14`/`:29` becomes `None`).
        // Arrange:
        let title: Fields = one_string_field("title".to_string(), "love".to_string());
        let date: Fields = one_string_field("date".to_string(), "2020-01-01".to_string());
        // Act:
        let contains_fields: Option<Fields> = fields(Expression::Contains(title.clone()));
        let after_fields: Option<Fields> = fields(Expression::After(date.clone()));
        let group_fields: Option<Fields> =
            fields(Expression::All(vec![Expression::Contains(title.clone())]));
        // Assert
        assert_eq!(
            contains_fields,
            Some(title.clone()),
            "walk_test.go:51 — leaf fields() is the map"
        );
        assert_eq!(after_fields, Some(date.clone()), "walk_test.go:52");
        assert_eq!(
            group_fields, None,
            "walk_test.go:56 — group fields() is nil",
        );
    }

    #[test]
    fn extract_playlist_field_recurses_and_skips_empty_and_non_string_leaves() {
        // `operators.go:176-:194`: `all`/`any` recurse, only `inPlaylist`/
        // `notInPlaylist` contribute, the `.(string)` cast drops non-string
        // slots, and the `value != ""` guard drops empty ones. Dedupe lives in
        // `criteria.go:74-:82`, so repeats survive here (see criteria.rs).
        // Arrange:
        let shared: Fields = one_string_field("id".to_string(), "pl-1".to_string());
        let repeated: Fields = one_string_field("id".to_string(), "pl-1".to_string());
        let empty: Fields = one_string_field("id".to_string(), "".to_string());
        let bool_leaf: Fields = bool_field("id".to_string());
        let path_only: Fields = one_string_field("path".to_string(), "./child.nsp".to_string());
        let expr: Expression = Expression::All(vec![
            Expression::InPlaylist(shared.clone()),
            Expression::Any(vec![
                Expression::InPlaylist(repeated.clone()),
                Expression::NotInPlaylist(empty.clone()),
                Expression::NotInPlaylist(bool_leaf.clone()),
                Expression::InPlaylist(path_only.clone()),
                Expression::Contains(shared.clone()),
            ]),
        ]);
        // Act:
        let ids: Vec<String> = extract_playlist_field(expr.clone(), "id".to_string());
        let paths: Vec<String> = extract_playlist_field(expr, "path".to_string());
        // Assert
        assert_eq!(
            ids,
            vec!["pl-1", "pl-1"],
            "operators.go:188 — repeats kept, empty and non-string leaves skipped",
        );
        assert_eq!(
            paths,
            vec!["./child.nsp"],
            "criteria_test.go:411 — only playlist leaves match",
        );
    }
}
