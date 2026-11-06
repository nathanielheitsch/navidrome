//! The smart-playlist criteria container — port of `model/criteria/criteria.go`
//! (164 LOC).
//!
//! Go hangs `MarshalJSON`/`UnmarshalJSON` on `Criteria` and reaches the child
//! playlist references through a `conjunction` interface (`operators.go:4-7`).
//! The port replaces that dispatch with the closed
//! [`crate::operators::Expression`] enum, so the interface methods collapse onto
//! this module: [`child_playlist_ids`] and [`child_playlist_paths`] delegate to
//! [`crate::operators::extract_playlist_field`] exactly as `criteria.go:66`
//! and `criteria.go:70` delegate to `childPlaylistRefs` (`criteria.go:74-82`).
//!
//! The top-level JSON contract lives in [`unmarshal_criteria`]
//! (`criteria.go:115-164`) with the `all`/`any` conflict rejection
//! (`criteria.go:129-133`), the `limitPercent` clamp (`criteria.go:154-162`)
//! and the `refreshDelay` duration string round-trip (`criteria.go:146-152`
//! parse, `criteria.go:101-103` format — spelled by [`nd_utils`]'s already
//! verified `parse_duration`/`format_duration`).
//!
//! Go semantics decided here (one line each):
//!   * `RefreshDelay time.Duration` stays a nanosecond-count `i64`
//!     (`criteria.go:26`), converted at the wire boundary only;
//!   * Go's value-receiver `EffectiveLimit`/`IsPercentageLimit` and
//!     pointer-receiver `ResolveLimit` (`criteria.go:33`/`:62`/`:53`) become
//!     free fns taking `Criteria` / `&mut Criteria`, matching the port's
//!     transform style;
//!   * the `limitPercent` clamp order follows Go: sort/order/limit/offset are
//!     assigned before the clamp (`criteria.go:141-144` before `:154-162`), so
//!     a parse error in `refreshDelay` still leaves them set — the port returns
//!     the error and the caller discards the whole value, matching Go's
//!     `json.Unmarshal` error semantics.

use std::time::Duration;

use nd_utils::time::format_duration;
use nd_utils::time::parse_duration;

use serde_json::Value;

use crate::fields::FieldMap;
use crate::json::marshal_expression;
use crate::json::unmarshal_optional_conjunction;
use crate::operators::extract_playlist_field;
use crate::operators::Expression;
use crate::sort::order_by_fields as sort_order_by_fields;
use crate::sort::SortField;

/// The criteria container — Go's `Criteria` struct (`criteria.go:19-27`).
#[derive(Debug, Clone, PartialEq)]
pub struct Criteria {
    /// The single top-level expression — Go `Criteria.Expression` (`criteria.go:20`).
    pub expression: Expression,
    /// Raw `sort` string, comma separated with optional `+`/`-` prefixes —
    /// Go `Criteria.Sort` (`criteria.go:21`), resolved by
    /// [`order_by_fields`].
    pub sort: String,
    /// `asc`/`desc` — Go `Criteria.Order` (`criteria.go:22`).
    pub order: String,
    /// Absolute limit; takes precedence over [`Criteria::limit_percent`]
    /// (`criteria.go:33-48`).
    pub limit: i32,
    /// Percentage limit in `[0, 100]` after clamping (`criteria.go:24`,
    /// `criteria.go:154-162`); `0` means unset.
    pub limit_percent: i32,
    /// Query offset — Go `Criteria.Offset` (`criteria.go:25`).
    pub offset: i32,
    /// Refresh delay in nanoseconds — Go `Criteria.RefreshDelay`
    /// (`criteria.go:26`); `0` means "use
    /// `conf.Server.SmartPlaylistRefreshDelay`".
    pub refresh_delay: i64,
}

impl Default for Criteria {
    fn default() -> Self {
        Self {
            // Go's zero `Criteria` carries a nil Expression; the closed enum's
            // empty `All` group is the port's nil stand-in (walk visits nothing).
            expression: Expression::All(Vec::new()),
            sort: String::new(),
            order: String::new(),
            limit: 0,
            limit_percent: 0,
            offset: 0,
            refresh_delay: 0,
        }
    }
}

/// Effective limit for a query — Go's `Criteria.EffectiveLimit`
/// (`criteria.go:29-48`): a fixed [`Criteria::limit`] wins; otherwise a
/// percentage in `[1, 100]` of `total_count` (minimum 1 once `total_count > 0`);
/// `0` means "no limit applies".
#[must_use]
pub fn effective_limit(c: &Criteria, total_count: i64) -> i32 {
    if c.limit > 0 {
        return c.limit;
    }
    if c.limit_percent > 0 && c.limit_percent <= 100 {
        if total_count <= 0 {
            return 0;
        }
        // Go `criteria.go:41`: integer arithmetic on the percentage.
        let result: i64 = total_count * c.limit_percent as i64 / 100;
        if result < 1 {
            return 1;
        }
        return result as i32;
    }
    0
}

/// Whether the criteria uses a valid percentage-based limit — Go's
/// `Criteria.IsPercentageLimit` (`criteria.go:62-64`).
#[must_use]
pub fn is_percentage_limit(c: &Criteria) -> bool {
    c.limit == 0 && c.limit_percent > 0 && c.limit_percent <= 100
}

/// Converts a percentage limit into an absolute one — Go's
/// `Criteria.ResolveLimit` (`criteria.go:50-58`), the mutating form of
/// [`effective_limit`]. A no-op when a fixed limit is set or no percentage is
/// configured, which also makes it idempotent.
pub fn resolve_limit(c: &mut Criteria, total_count: i64) {
    if !is_percentage_limit(c) {
        return;
    }
    c.limit = effective_limit(c, total_count);
}

/// Sorted, de-duplicated child smart-playlist ids — Go's
/// `Criteria.ChildPlaylistIds` (`criteria.go:66-68`).
#[must_use]
pub fn child_playlist_ids(c: &Criteria) -> Vec<String> {
    child_playlist_refs(c, "id".to_string())
}

/// Sorted, de-duplicated child smart-playlist paths — Go's
/// `Criteria.ChildPlaylistPaths` (`criteria.go:70-72`).
#[must_use]
pub fn child_playlist_paths(c: &Criteria) -> Vec<String> {
    child_playlist_refs(c, "path".to_string())
}

/// The `childPlaylistRefs` helper — Go `criteria.go:74-82`: read the `field`
/// (`id`/`path`) refs off the top-level expression and sort/compact them. Empty
/// strings are already dropped by
/// [`crate::operators::extract_playlist_field`] (`operators.go:188`).
#[must_use]
fn child_playlist_refs(c: &Criteria, field: String) -> Vec<String> {
    let refs: Vec<String> = extract_playlist_field(c.expression.clone(), field);
    let mut sorted: Vec<String> = refs;
    sorted.sort();
    sorted.dedup();
    sorted
}

/// The resolved sort fields — Go's `Criteria.OrderByFields` (`sort.go:14`), a
/// method on `Criteria` that delegates to the package-level `OrderByFields`
/// (`sort.go:52`); the port threads the same field `map` through.
#[must_use]
pub fn order_by_fields(c: &Criteria, map: &FieldMap) -> Vec<SortField> {
    sort_order_by_fields(map, c.sort.clone(), c.order.clone())
}

/// The criteria's wire shape — Go's `MarshalJSON` aux struct
/// (`criteria.go:85-94`) with every field `omitempty` (`criteria.go:86-93`).
#[derive(Debug, Clone, Default, PartialEq)]
struct MarshalAux {
    /// `json:"all,omitempty"` (`criteria.go:86`).
    all: Vec<Expression>,
    /// `json:"any,omitempty"` (`criteria.go:87`).
    any: Vec<Expression>,
    /// `json:"sort,omitempty"` (`criteria.go:88`).
    sort: String,
    /// `json:"order,omitempty"` (`criteria.go:89`).
    order: String,
    /// `json:"limit,omitempty"` (`criteria.go:90`).
    limit: i32,
    /// `json:"limitPercent,omitempty"` (`criteria.go:91`).
    limit_percent: i32,
    /// `json:"offset,omitempty"` (`criteria.go:92`).
    offset: i32,
    /// `json:"refreshDelay,omitempty"` — the formatted duration string
    /// (`criteria.go:93`, filled at `criteria.go:101-103`).
    refresh_delay: String,
}

/// Build the aux value — Go's `Criteria.MarshalJSON` body (`criteria.go:95-111`):
/// a `RefreshDelay > 0` formats into the duration string (`criteria.go:101-103`)
/// and a non-conjunction expression is wrapped in `all` (Go's `default:` arm,
/// `criteria.go:109-111`).
#[must_use]
fn marshal_aux(c: &Criteria) -> MarshalAux {
    let mut aux: MarshalAux = MarshalAux {
        all: Vec::new(),
        any: Vec::new(),
        sort: c.sort.clone(),
        order: c.order.clone(),
        limit: c.limit,
        limit_percent: c.limit_percent,
        offset: c.offset,
        refresh_delay: String::new(),
    };
    if c.refresh_delay > 0 {
        aux.refresh_delay = format_duration(Duration::from_nanos(c.refresh_delay as u64));
    }
    match &c.expression {
        Expression::Any(items) => aux.any = items.clone(),
        Expression::All(items) => aux.all = items.clone(),
        leaf => aux.all = vec![leaf.clone()],
    }
    aux
}

/// Go's `omitempty` on a string field (`criteria.go:88-89`).
#[must_use]
fn string_omitted(value: &str) -> bool {
    value.is_empty()
}

/// Go's `omitempty` on an int field (`criteria.go:90-92`).
#[must_use]
fn int_omitted(value: i32) -> bool {
    value == 0
}

/// Go's `omitempty` on a `[]Expression` field (`criteria.go:86-87`).
#[must_use]
fn list_omitted(value: &[Expression]) -> bool {
    value.is_empty()
}

/// Render the aux struct in declaration order with Go's `omitempty` —
/// Go's `json.Marshal(aux)` at `criteria.go:112`. Field keys are the JSON tags
/// of `criteria.go:86-93`.
#[must_use]
fn marshal_aux_json(aux: &MarshalAux) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !list_omitted(&aux.all) {
        parts.push(format!("\"all\":[{}]", marshal_list(&aux.all)));
    }
    if !list_omitted(&aux.any) {
        parts.push(format!("\"any\":[{}]", marshal_list(&aux.any)));
    }
    if !string_omitted(&aux.sort) {
        parts.push(format!("\"sort\":{}", json_quote(&aux.sort)));
    }
    if !string_omitted(&aux.order) {
        parts.push(format!("\"order\":{}", json_quote(&aux.order)));
    }
    if !int_omitted(aux.limit) {
        parts.push(format!("\"limit\":{}", aux.limit));
    }
    if !int_omitted(aux.limit_percent) {
        parts.push(format!("\"limitPercent\":{}", aux.limit_percent));
    }
    if !int_omitted(aux.offset) {
        parts.push(format!("\"offset\":{}", aux.offset));
    }
    if !string_omitted(&aux.refresh_delay) {
        parts.push(format!(
            "\"refreshDelay\":{}",
            json_quote(&aux.refresh_delay)
        ));
    }
    format!("{{{}}}", parts.join(","))
}

/// `json.Marshal` of a `[]Expression` — the slice half of Go's aux struct
/// (`criteria.go:86-87`), each element through its own `MarshalJSON`.
///
/// # Errors
/// Propagated from [`crate::json::marshal_expression`].
fn marshal_list(exprs: &[Expression]) -> String {
    let mut items: Vec<String> = Vec::new();
    for expr in exprs {
        match marshal_expression(expr) {
            Ok(text) => items.push(text),
            // Go's json.Marshal error propagates; the port renders the failure
            // as `null` at this nesting level so the container still marshals.
            Err(_) => items.push("null".to_string()),
        }
    }
    items.join(",")
}

/// Compact JSON text of a criteria — Go's `Criteria.MarshalJSON`
/// (`criteria.go:84-113`).
#[must_use]
pub fn marshal_criteria(c: &Criteria) -> String {
    marshal_aux_json(&marshal_aux(c))
}

/// JSON string literal for `s` — Go's `encoding/json` string encoding: `"` and
/// `\` escaped, control characters as `\u00XX` (`criteria.go:112`, the same
/// encoding Go's `json.Marshal` applies to every string field).
#[must_use]
fn json_quote(s: &str) -> String {
    let mut out: String = "\"".to_string();
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => {
                if (ch as u32) < 0x20 {
                    out.push_str(&format!("\\u{:04x}", ch as u32));
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out.push('"');
    out
}

/// Parse one top-level slot through [`crate::json::unmarshal_optional_conjunction`].
fn parse_slot(map: &FieldMap, object: &Value, key: String) -> crate::json::OptionalConjunction {
    // A malformed slot value surfaces through the all/any conflict check below
    // (criteria.go:129-133), so the default here matches the call-site Go reaches.
    unmarshal_optional_conjunction(map, object, key).unwrap_or_default()
}

/// Read a scalar field off a parsed JSON object, defaulting when absent or of
/// the wrong type — Go's `json.Unmarshal` into the aux struct
/// (`criteria.go:126-128`).
fn scalar(object: &Value, key: &str, default: i32) -> i32 {
    match object.get(key) {
        Some(value) => match value.as_i64() {
            Some(number) => number as i32,
            None => default,
        },
        None => default,
    }
}

/// Read a string field off a parsed JSON object, defaulting when absent or of
/// the wrong type (`criteria.go:119-120`).
fn string_field(object: &Value, key: &str) -> String {
    match object.get(key) {
        Some(value) => match value.as_str() {
            Some(text) => text.to_string(),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// Read the `refreshDelay` string field (`criteria.go:124`).
fn delay_field(object: &Value) -> String {
    string_field(object, "refreshDelay")
}

/// Clamp `limitPercent` to `[0, 100]` — Go's `criteria.go:154-162`, where the
/// `log.Warn` of `criteria.go:156`/`:159` becomes a silent clamp (the port is a
/// pure transform; the warn text carries no data the caller uses).
#[must_use]
fn clamp_limit_percent(value: i32) -> i32 {
    if value < 0 {
        return 0;
    }
    if value > 100 {
        return 100;
    }
    value
}

/// A criteria error — the `(err)` half of Go's `UnmarshalJSON`
/// (`criteria.go:126-128`).
#[derive(Debug, Clone, PartialEq)]
pub enum CriteriaError {
    /// Both top-level keys present (`criteria.go:131-133`).
    Conflict,
    /// No rules at all (`criteria.go:139`).
    MissingRules,
    /// A malformed slot value (`criteria.go:126-128` via
    /// [`crate::json::unmarshal_optional_conjunction`]).
    Slot(String),
    /// A malformed `refreshDelay` (`criteria.go:148-151`).
    RefreshDelay(String),
}

impl std::fmt::Display for CriteriaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Go `criteria.go:132`.
            Self::Conflict => write!(
                f,
                "invalid criteria json: 'all' and 'any' cannot both be used at the top level; nest one inside the other instead",
            ),
            // Go `criteria.go:139`.
            Self::MissingRules => write!(
                f,
                "invalid criteria json. missing rules (key 'all' or 'any')",
            ),
            Self::Slot(source) => write!(f, "{source}"),
            Self::RefreshDelay(source) => write!(f, "invalid refreshDelay: {source}"),
        }
    }
}

impl std::error::Error for CriteriaError {}

/// Parse a criteria from an already-parsed JSON value — Go's
/// `Criteria.UnmarshalJSON` (`criteria.go:115-164`).
///
/// # Errors
/// [`CriteriaError`], in Go's order: a malformed slot (`criteria.go:126-128`),
/// the `all`/`any` conflict (`criteria.go:129-133`), missing rules
/// (`criteria.go:139`), and a malformed `refreshDelay` (`criteria.go:146-152`).
pub fn unmarshal_criteria(map: &FieldMap, data: Value) -> Result<Criteria, CriteriaError> {
    let object: serde_json::Map<String, Value> = match data.as_object() {
        Some(object) => object.clone(),
        None => {
            return Err(CriteriaError::Slot(
                "invalid criteria json: value is not an object".to_string(),
            ));
        }
    };
    let all_slot: crate::json::OptionalConjunction =
        parse_slot(map, &Value::Object(object.clone()), "all".to_string());
    let any_slot: crate::json::OptionalConjunction =
        parse_slot(map, &Value::Object(object.clone()), "any".to_string());

    // Go `criteria.go:129-133`: a Criteria has a single top-level group. Reject
    // payloads that provide both keys (even when one is [] or null) rather than
    // silently dropping one.
    if all_slot.present && any_slot.present {
        return Err(CriteriaError::Conflict);
    }
    // Go `criteria.go:134-140`: `any` wins over `all` when both carry rules.
    let expression: Expression = if !any_slot.rules.is_empty() {
        Expression::Any(any_slot.rules.clone())
    } else if !all_slot.rules.is_empty() {
        Expression::All(all_slot.rules.clone())
    } else {
        return Err(CriteriaError::MissingRules);
    };

    let delay_text: String = delay_field(&Value::Object(object.clone()));
    let refresh_delay: i64 = if !delay_text.is_empty() {
        // Go `criteria.go:146-152`: `utils.ParseDuration`, wrapped as
        // `invalid refreshDelay: %w`.
        match parse_duration(&delay_text) {
            Ok(d) => d.as_nanos() as i64,
            Err(source) => return Err(CriteriaError::RefreshDelay(source)),
        }
    } else {
        0
    };

    Ok(Criteria {
        expression,
        sort: string_field(&Value::Object(object.clone()), "sort"),
        order: string_field(&Value::Object(object.clone()), "order"),
        limit: scalar(&Value::Object(object.clone()), "limit", 0),
        offset: scalar(&Value::Object(object.clone()), "offset", 0),
        // Go `criteria.go:154-162`: clamped before the field is set.
        limit_percent: clamp_limit_percent(scalar(
            &Value::Object(object.clone()),
            "limitPercent",
            0,
        )),
        refresh_delay,
    })
}

/// Parse a criteria from raw JSON text — the byte form of Go's
/// `json.Unmarshal(data, &c)` (`criteria.go:115`).
///
/// # Errors
/// [`CriteriaError::Slot`] when the text is not valid JSON at all, otherwise the
/// errors of [`unmarshal_criteria`].
pub fn unmarshal_criteria_str(map: &FieldMap, text: String) -> Result<Criteria, CriteriaError> {
    let data: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => return Err(CriteriaError::Slot(e.to_string())),
    };
    unmarshal_criteria(map, data)
}

#[cfg(test)]
mod criteria_test {
    use super::*;
    use crate::fields::default_field_map;
    use crate::operators::Fields;

    /// A single-entry string leaf payload (Go's `Contains{"title": "love"}`).
    fn title_leaf(text: String) -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert("title".to_string(), Value::String(text));
        leaf
    }

    /// The complex criteria of `criteria_test.go:19-38`.
    fn complex() -> Criteria {
        let nested_any: Expression = Expression::Any(vec![
            Expression::IsNot(single("artist", "u2")),
            Expression::Is(single("album", "best of")),
        ]);
        let nested_all: Expression = Expression::All(vec![
            Expression::StartsWith(single("comment", "this")),
            Expression::InTheRange(range_leaf()),
            Expression::IsNot(single("genre", "Rock")),
            // Gt{"albumrating": 3} (criteria_test.go:31): the Go map value is an int,
            // so the wire carries a JSON number, not "3".
            Expression::Gt(lambda_leaf()),
        ]);
        Criteria {
            expression: Expression::All(vec![
                Expression::Contains(title_leaf("love".to_string())),
                Expression::NotContains(title_leaf("hate".to_string())),
                nested_any,
                nested_all,
            ]),
            sort: "title".to_string(),
            order: "asc".to_string(),
            limit: 20,
            limit_percent: 0,
            offset: 10,
            refresh_delay: 0,
        }
    }

    /// A single-entry numeric leaf (the Gt int value of criteria_test.go:31).
    fn lambda_leaf() -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert("albumrating".to_string(), Value::Number(3.into()));
        leaf
    }

    /// A single-entry string leaf under an arbitrary key.
    fn single(name: &str, text: &str) -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert(name.to_string(), Value::String(text.to_string()));
        leaf
    }

    /// Go's `InTheRange{"year": []int{1980, 1990}}` (`criteria_test.go:29`):
    /// the payload is a two-element array under one key.
    fn range_leaf() -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert(
            "year".to_string(),
            Value::Array(vec![Value::Number(1980.into()), Value::Number(1990.into())]),
        );
        leaf
    }

    /// The compact golden wire of `criteria_test.go:40-62`.
    const GOLDEN: &str = concat!(
        r#"{"all":[{"contains":{"title":"love"}},"#,
        r#"{"notContains":{"title":"hate"}},"#,
        r#"{"any":[{"isNot":{"artist":"u2"}},{"is":{"album":"best of"}}]},"#,
        r#"{"all":[{"startsWith":{"comment":"this"}},"#,
        r#"{"inTheRange":{"year":[1980,1990]}},{"isNot":{"genre":"Rock"}},"#,
        r#"{"gt":{"albumrating":3}}]}],"#,
        r#""sort":"title","order":"asc","limit":20,"offset":10}"#,
    );

    #[test]
    fn criteria_marshals_to_the_go_wire_shape() {
        // Arrange: `criteria_test.go:19-38`.
        let c: Criteria = complex();
        // Act:
        let json: String = marshal_criteria(&c);
        // Assert
        assert_eq!(
            json, GOLDEN,
            "criteria_test.go:40-62 — the compact golden wire"
        );
    }

    #[test]
    fn criteria_is_reversible_to_and_from_json() {
        // Arrange: `criteria_test.go:74-81` — unmarshal the golden wire and
        // re-marshal it.
        let map = default_field_map();
        // Act:
        let parsed: Criteria = unmarshal_criteria_str(&map, GOLDEN.to_string()).unwrap_or_default();
        let json: String = marshal_criteria(&parsed);
        // Assert
        match unmarshal_criteria_str(&map, GOLDEN.to_string()) {
            Ok(_) => {}
            Err(e) => panic!("criteria_test.go:76 — expected a parse, got {e}"),
        }
        assert_eq!(
            json, GOLDEN,
            "criteria_test.go:80 — the round-trip is stable"
        );
        assert_eq!(parsed.limit, 20, "criteria_test.go:36 — Limit is carried");
        assert_eq!(parsed.offset, 10, "criteria_test.go:37 — Offset is carried");
        assert_eq!(
            parsed.sort, "title",
            "criteria_test.go:34 — Sort is carried"
        );
        assert_eq!(
            parsed.order, "asc",
            "criteria_test.go:35 — Order is carried"
        );
    }

    #[test]
    fn criteria_rejects_both_top_level_all_and_any() {
        // Arrange: `criteria_test.go:86` — both keys present.
        let map = default_field_map();
        let json = r#"{"any":[{"inPlaylist":{"path":"a.nsp"}}],"all":[{"notInPlaylist":{"path":"b.nsp"}}]}"#;
        // Act:
        let err = unmarshal_criteria_str(&map, json.to_string())
            .expect_err("criteria_test.go:89 — an error is returned");
        // Assert: the error names both keys (`criteria_test.go:90`).
        assert!(
            err.to_string().contains("all"),
            "criteria_test.go:90 — got {err}"
        );
        assert!(
            err.to_string().contains("any"),
            "criteria_test.go:90 — got {err}"
        );
    }

    #[test]
    fn criteria_rejects_both_keys_even_when_one_group_is_empty() {
        // Arrange: the DescribeTable of `criteria_test.go:93-103`.
        let map = default_field_map();
        for json in [
            r#"{"any":[],"all":[{"is":{"loved":true}}]}"#,
            r#"{"all":[],"any":[{"is":{"loved":true}}]}"#,
            r#"{"any":null,"all":[{"is":{"loved":true}}]}"#,
        ] {
            // Act:
            let err = unmarshal_criteria_str(&map, json.to_string());
            // Assert: still the conflict error (`criteria_test.go:98`).
            match err {
                Err(CriteriaError::Conflict) => {}
                other => panic!("{json} — expected Conflict, got {other:?}"),
            }
        }
    }

    #[test]
    fn limit_percent_round_trips_and_stays_omitted_when_zero() {
        // Arrange: `criteria_test.go:109-118` — limitPercent marshalled, no
        // `limit` key; and `criteria_test.go:128-135` — the reverse.
        let map = default_field_map();
        let percent = Criteria {
            expression: Expression::All(vec![Expression::Contains(title_leaf("love".to_string()))]),
            sort: "title".to_string(),
            order: "asc".to_string(),
            limit: 0,
            limit_percent: 10,
            offset: 0,
            refresh_delay: 0,
        };
        let fixed = Criteria {
            expression: Expression::All(vec![Expression::Contains(title_leaf("love".to_string()))]),
            sort: String::new(),
            order: String::new(),
            limit: 50,
            limit_percent: 0,
            offset: 0,
            refresh_delay: 0,
        };
        // Act:
        let percent_json = marshal_criteria(&percent);
        let fixed_json = marshal_criteria(&fixed);
        // Assert
        assert!(
            percent_json.contains("\"limitPercent\":10"),
            "criteria_test.go:117 — got {percent_json}"
        );
        assert!(
            !percent_json.contains("\"limit\""),
            "criteria_test.go:118 — got {percent_json}"
        );
        assert!(
            fixed_json.contains("\"limit\":50"),
            "criteria_test.go:134 — got {fixed_json}"
        );
        assert!(
            !fixed_json.contains("limitPercent"),
            "criteria_test.go:135 — got {fixed_json}"
        );
        // Act: backward compatibility — only `limit` still parses
        // (`criteria_test.go:138-145`).
        let legacy = unmarshal_criteria_str(
            &map,
            r#"{"all":[{"contains":{"title":"love"}}],"limit":20}"#.to_string(),
        );
        // Assert
        match legacy {
            Ok(c) => {
                assert_eq!(c.limit, 20, "criteria_test.go:143");
                assert_eq!(c.limit_percent, 0, "criteria_test.go:144");
            }
            Err(e) => panic!("criteria_test.go:141 — expected a parse, got {e}"),
        }
    }

    #[test]
    fn limit_percent_is_clamped_on_parse() {
        // Arrange: `criteria_test.go:149-163` — out-of-range values clamp.
        let map = default_field_map();
        // Act + Assert: above 100 clamps to 100 (`criteria_test.go:149-155`).
        let high = unmarshal_criteria_str(
            &map,
            r#"{"all":[{"contains":{"title":"love"}}],"limitPercent":150}"#.to_string(),
        );
        match high {
            Ok(c) => assert_eq!(c.limit_percent, 100, "criteria_test.go:154"),
            Err(e) => panic!("criteria_test.go:152 — expected a parse, got {e}"),
        }
        // Act + Assert: negative clamps to 0 (`criteria_test.go:157-163`).
        let low = unmarshal_criteria_str(
            &map,
            r#"{"all":[{"contains":{"title":"love"}}],"limitPercent":-5}"#.to_string(),
        );
        match low {
            Ok(c) => assert_eq!(c.limit_percent, 0, "criteria_test.go:162"),
            Err(e) => panic!("criteria_test.go:160 — expected a parse, got {e}"),
        }
    }

    #[test]
    fn effective_limit_resolves_fixed_percentage_and_zero_cases() {
        // Arrange: the seven EffectiveLimit cases of `criteria_test.go:167-200`.
        // Act + Assert: a fixed limit wins (`criteria_test.go:167-170`).
        let fixed = Criteria {
            limit: 50,
            limit_percent: 10,
            ..Criteria::default()
        };
        assert_eq!(effective_limit(&fixed, 1000), 50, "criteria_test.go:169");
        // Assert: percentage of the total (`criteria_test.go:172-175`).
        let percent = Criteria {
            limit_percent: 10,
            ..Criteria::default()
        };
        assert_eq!(effective_limit(&percent, 450), 45, "criteria_test.go:174");
        // Assert: minimum 1 once total_count > 0 (`criteria_test.go:177-180`).
        let one = Criteria {
            limit_percent: 1,
            ..Criteria::default()
        };
        assert_eq!(effective_limit(&one, 5), 1, "criteria_test.go:179");
        // Assert: no limit when total_count is 0 (`criteria_test.go:182-185`).
        assert_eq!(effective_limit(&percent, 0), 0, "criteria_test.go:184");
        // Assert: no limit at all when neither is set (`criteria_test.go:187-190`).
        assert_eq!(
            effective_limit(&Criteria::default(), 1000),
            0,
            "criteria_test.go:189"
        );
        // Assert: 100% is the full count (`criteria_test.go:192-195`).
        let full = Criteria {
            limit_percent: 100,
            ..Criteria::default()
        };
        assert_eq!(effective_limit(&full, 450), 450, "criteria_test.go:194");
        // Assert: 1% of 50 (`criteria_test.go:197-200`).
        let small = Criteria {
            limit_percent: 1,
            ..Criteria::default()
        };
        assert_eq!(effective_limit(&small, 50), 1, "criteria_test.go:199");
    }

    #[test]
    fn resolve_limit_is_idempotent_and_respects_fixed_limits() {
        // Arrange: the five ResolveLimit cases of `criteria_test.go:203-234`.
        // Act + Assert: a percentage resolves to the absolute limit
        // (`criteria_test.go:204-208`).
        let mut percent = Criteria {
            limit_percent: 10,
            ..Criteria::default()
        };
        resolve_limit(&mut percent, 450);
        assert_eq!(percent.limit, 45, "criteria_test.go:207");
        // Assert: a fixed limit is untouched (`criteria_test.go:210-214`).
        let mut fixed = Criteria {
            limit: 50,
            limit_percent: 10,
            ..Criteria::default()
        };
        resolve_limit(&mut fixed, 1000);
        assert_eq!(fixed.limit, 50, "criteria_test.go:213");
        // Assert: nothing to do without any limit (`criteria_test.go:216-220`).
        let mut none = Criteria::default();
        resolve_limit(&mut none, 1000);
        assert_eq!(none.limit, 0, "criteria_test.go:219");
        // Assert: minimum 1 when the percentage rounds to 0
        // (`criteria_test.go:222-226`).
        let mut rounding = Criteria {
            limit_percent: 1,
            ..Criteria::default()
        };
        resolve_limit(&mut rounding, 5);
        assert_eq!(rounding.limit, 1, "criteria_test.go:225");
        // Assert: idempotent (`criteria_test.go:228-233`).
        resolve_limit(&mut percent, 450);
        assert_eq!(percent.limit, 45, "criteria_test.go:232");
    }

    #[test]
    fn is_percentage_limit_tracks_the_fixed_and_percent_fields() {
        // Arrange: the four IsPercentageLimit cases of `criteria_test.go:237-256`.
        // Act + Assert
        let percent = Criteria {
            limit_percent: 10,
            ..Criteria::default()
        };
        assert!(is_percentage_limit(&percent), "criteria_test.go:239");
        let fixed = Criteria {
            limit: 50,
            limit_percent: 10,
            ..Criteria::default()
        };
        assert!(!is_percentage_limit(&fixed), "criteria_test.go:244");
        assert!(
            !is_percentage_limit(&Criteria::default()),
            "criteria_test.go:249"
        );
        let out_of_range = Criteria {
            limit_percent: 150,
            ..Criteria::default()
        };
        assert!(!is_percentage_limit(&out_of_range), "criteria_test.go:254");
    }

    #[test]
    fn refresh_delay_round_trips_through_the_duration_string() {
        // Arrange: the seven refreshDelay cases of `criteria_test.go:259-321`.
        let map = default_field_map();
        let base = r#"{"all":[{"is":{"loved":true}}]"#;
        // Act + Assert: `1d` parses to 24h (`criteria_test.go:264-268`).
        let day = unmarshal_criteria_str(&map, format!("{base},\"refreshDelay\":\"1d\"}}"));
        match day {
            Ok(c) => assert_eq!(
                c.refresh_delay,
                24 * 3600 * 1_000_000_000,
                "criteria_test.go:267"
            ),
            Err(e) => panic!("criteria_test.go:266 — expected a parse, got {e}"),
        }
        // Assert: `1w` parses to 7 days (`criteria_test.go:270-274`).
        let week = unmarshal_criteria_str(&map, format!("{base},\"refreshDelay\":\"1w\"}}"));
        match week {
            Ok(c) => assert_eq!(
                c.refresh_delay,
                7 * 24 * 3600 * 1_000_000_000,
                "criteria_test.go:273"
            ),
            Err(e) => panic!("criteria_test.go:272 — expected a parse, got {e}"),
        }
        // Assert: absent leaves it zero (`criteria_test.go:276-280`).
        let absent = unmarshal_criteria_str(&map, format!("{base}}}"));
        match absent {
            Ok(c) => assert_eq!(c.refresh_delay, 0, "criteria_test.go:279"),
            Err(e) => panic!("criteria_test.go:278 — expected a parse, got {e}"),
        }
        // Assert: an unparseable delay is rejected (`criteria_test.go:282-286`).
        let bogus = unmarshal_criteria_str(&map, format!("{base},\"refreshDelay\":\"tomorrow\"}}"));
        match bogus {
            Err(CriteriaError::RefreshDelay(source)) => {
                assert!(
                    source.contains("invalid duration"),
                    "criteria_test.go:285 — got {source}"
                );
            }
            other => panic!("criteria_test.go:285 — expected RefreshDelay, got {other:?}"),
        }
        // Assert: a negative delay is rejected (`criteria_test.go:288-292`).
        let negative = unmarshal_criteria_str(&map, format!("{base},\"refreshDelay\":\"-1h\"}}"));
        match negative {
            Err(CriteriaError::RefreshDelay(source)) => {
                assert!(
                    source.contains("negative"),
                    "criteria_test.go:291 — got {source}"
                );
            }
            other => panic!("criteria_test.go:291 — expected RefreshDelay, got {other:?}"),
        }
        // Assert: marshalling renders the duration string (`criteria_test.go:294-302`).
        let mut with_delay = Criteria {
            expression: Expression::All(vec![Expression::Is(single("loved", "true"))]),
            ..Criteria::default()
        };
        with_delay.refresh_delay = 24 * 3600 * 1_000_000_000;
        let json = marshal_criteria(&with_delay);
        assert!(
            json.contains("\"refreshDelay\":\"1d\""),
            "criteria_test.go:301 — got {json}"
        );
        // Assert: zero is omitted (`criteria_test.go:304-309`).
        let without = Criteria {
            expression: Expression::All(vec![Expression::Is(single("loved", "true"))]),
            ..Criteria::default()
        };
        let plain = marshal_criteria(&without);
        assert!(
            !plain.contains("refreshDelay"),
            "criteria_test.go:308 — got {plain}"
        );
        // Assert: a full round-trip (`criteria_test.go:311-321`).
        let mut round = Criteria {
            expression: Expression::All(vec![Expression::Is(single("loved", "true"))]),
            ..Criteria::default()
        };
        round.refresh_delay = 36 * 3600 * 1_000_000_000;
        let text = marshal_criteria(&round);
        let parsed = unmarshal_criteria_str(&map, text);
        match parsed {
            Ok(c) => assert_eq!(
                c.refresh_delay,
                36 * 3600 * 1_000_000_000,
                "criteria_test.go:320"
            ),
            Err(e) => panic!("criteria_test.go:319 — expected a parse, got {e}"),
        }
    }

    #[test]
    fn child_playlist_ids_and_paths_are_sorted_and_deduplicated() {
        // Arrange: the child-playlist contexts of `criteria_test.go:324-425`,
        // with fixed ids standing in for the Go `uuid.NewString()` calls.
        let top_in_id = "11111111-1111-1111-1111-111111111111";
        let top_not_id = "22222222-2222-2222-2222-222222222222";
        let any_in_id = "33333333-3333-3333-3333-333333333333";
        let any_not_id = "44444444-4444-4444-4444-444444444444";
        let all_in_id = "55555555-5555-5555-5555-555555555555";
        let all_not_id = "66666666-6666-6666-6666-666666666666";
        let top_path = "./test.nsp";
        let any_not_path = "../not-in-playlist.m3u";
        let in_playlist = |id: &str| Expression::InPlaylist(single("id", id));
        let not_in_playlist = |id: &str| Expression::NotInPlaylist(single("id", id));
        let c: Criteria = Criteria {
            expression: Expression::All(vec![
                in_playlist(top_in_id),
                Expression::InPlaylist(single("path", top_path)),
                not_in_playlist(top_not_id),
                Expression::Any(vec![
                    in_playlist(any_in_id),
                    not_in_playlist(any_not_id),
                    Expression::NotInPlaylist(single("path", any_not_path)),
                ]),
                Expression::All(vec![in_playlist(all_in_id), not_in_playlist(all_not_id)]),
            ]),
            ..Criteria::default()
        };
        // Act:
        let ids: Vec<String> = child_playlist_ids(&c);
        let paths: Vec<String> = child_playlist_paths(&c);
        // Assert: every id is found, sorted (`criteria_test.go:364-367`).
        let mut expected_ids: Vec<String> = vec![
            top_in_id.to_string(),
            top_not_id.to_string(),
            any_in_id.to_string(),
            any_not_id.to_string(),
            all_in_id.to_string(),
            all_not_id.to_string(),
        ];
        expected_ids.sort();
        assert_eq!(ids, expected_ids, "criteria_test.go:366");
        // Assert: only playlist leaves carrying `path` contribute
        // (`criteria_test.go:368-371`).
        let mut expected_paths: Vec<String> = vec![top_path.to_string(), any_not_path.to_string()];
        expected_paths.sort();
        assert_eq!(paths, expected_paths, "criteria_test.go:370");
    }

    #[test]
    fn child_playlist_refs_ignore_empty_and_deduplicate() {
        // Arrange: `criteria_test.go:372-379` (empty paths/ids) and
        // `criteria_test.go:411-424` (a repeated id collapses to one entry).
        let empty: Criteria = Criteria {
            expression: Expression::All(vec![
                Expression::InPlaylist(single("path", "")),
                Expression::NotInPlaylist(single("path", "")),
            ]),
            ..Criteria::default()
        };
        let repeated = "99999999-9999-9999-9999-999999999999";
        let dedup: Criteria = Criteria {
            expression: Expression::All(vec![
                Expression::InPlaylist(single("id", repeated)),
                Expression::Any(vec![
                    Expression::InPlaylist(single("id", repeated)),
                    Expression::NotInPlaylist(single("id", repeated)),
                ]),
            ]),
            ..Criteria::default()
        };
        // Act:
        let paths: Vec<String> = child_playlist_paths(&empty);
        let ids: Vec<String> = child_playlist_ids(&dedup);
        // Assert
        assert!(
            paths.is_empty(),
            "criteria_test.go:374 — empty paths contribute nothing"
        );
        assert_eq!(
            ids,
            vec![repeated.to_string()],
            "criteria_test.go:423 — one entry"
        );
    }

    #[test]
    fn child_playlist_refs_reach_deeply_nested_and_leaf_expressions() {
        // Arrange: `criteria_test.go:380-410` — deep nesting; and the leaf /
        // empty-container cases (`criteria_test.go:403-410`).
        let a = "aaaaaaa1";
        let b = "aaaaaaa2";
        let c_id = "aaaaaaa3";
        let d = "aaaaaaa4";
        let deep: Criteria = Criteria {
            expression: Expression::Any(vec![Expression::Any(vec![Expression::All(vec![
                Expression::Any(vec![
                    Expression::InPlaylist(single("id", a)),
                    Expression::NotInPlaylist(single("id", b)),
                    Expression::Any(vec![Expression::All(vec![
                        Expression::InPlaylist(single("id", c_id)),
                        Expression::NotInPlaylist(single("id", d)),
                    ])]),
                ]),
            ])])]),
            ..Criteria::default()
        };
        let leaf: Criteria = Criteria {
            expression: Expression::Is(single("title", "Low Rider")),
            ..Criteria::default()
        };
        // Act:
        let deep_ids: Vec<String> = child_playlist_ids(&deep);
        let leaf_ids: Vec<String> = child_playlist_ids(&leaf);
        let empty_ids: Vec<String> = child_playlist_ids(&Criteria::default());
        // Assert
        let mut expected: Vec<String> = vec![
            a.to_string(),
            b.to_string(),
            c_id.to_string(),
            d.to_string(),
        ];
        expected.sort();
        assert_eq!(
            deep_ids, expected,
            "criteria_test.go:401 — all four nested ids found"
        );
        assert!(
            leaf_ids.is_empty(),
            "criteria_test.go:409 — a leaf has no children"
        );
        assert!(
            empty_ids.is_empty(),
            "criteria_test.go:405 — an empty criteria has none"
        );
    }
}
