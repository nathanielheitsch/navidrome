//! JSON (de)serialisation of criteria expressions — port of
//! `model/criteria/json.go` (187 LOC).
//!
//! Go hangs `UnmarshalJSON` on two anonymous helper types (`json.go:10`
//! `unmarshalConjunctionType`, `json.go:40` `optionalConjunction`) and lets
//! `encoding/json` drive the dispatch. Rust has no custom-decoder hook on the
//! closed [`crate::operators::Expression`] enum, so each Go decoder becomes a
//! plain fn over an already-parsed [`Value`] and `optionalConjunction`'s "did
//! the key appear at all" memory (`json.go:45-48`) becomes the explicit
//! [`OptionalConjunction`] struct that `crate::criteria`'s top-level check
//! (`criteria.go:131`) needs.
//!
//! Wire asymmetries carried over verbatim:
//!   * the operator *key* is lower-cased on parse (`json.go:21`) but marshalled
//!     in canonical camelCase (`operators.go` `MarshalJSON`, spelled
//!     [`crate::operators::expression_key`]);
//!   * field *keys* are never case-folded (`json.go:21` folds only the operator
//!     key; see the registry note in `crate::fields`).

use serde_json::Value;

use crate::fields::lookup_field;
use crate::fields::FieldMap;
use crate::operators::Expression;
use crate::operators::Fields;

/// Coerce a criteria value to a bool — Go's `ToBool` (`json.go:113-132`),
/// returned as the `(value, ok)` tuple that is this port's spelling of Go's
/// two-value return (house precedent: [`crate::fields::lookup_field`]).
///
/// A JSON bool, a `strconv.ParseBool`-parseable string, or a JSON number that is
/// exactly 0 or 1 coerce; anything else (other numbers, `null`, arrays, objects)
/// reports `ok == false` so the caller can leave the value untouched.
#[must_use]
pub fn to_bool(value: Value) -> (bool, bool) {
    match value {
        Value::Bool(b) => (b, true),
        Value::String(s) => match parse_bool_string(s) {
            Some(b) => (b, true),
            None => (false, false),
        },
        Value::Number(_) => match value.as_f64() {
            // Go `json.go:124-129`: a float64 that is exactly 1 or 0.
            Some(1.0) => (true, true),
            Some(0.0) => (false, true),
            _ => (false, false),
        },
        _ => (false, false),
    }
}

/// Go's `strconv.ParseBool` (`json.go:121`): the accepted string set with its
/// case variants; `None` is the `err != nil` case.
#[must_use]
fn parse_bool_string(s: String) -> Option<bool> {
    // strconv.ParseBool truth list, case-insensitive (Go's accepted set).
    let lower = s.to_lowercase();
    if lower == "1"
        || lower == "t"
        || lower == "true"
        || lower == "y"
        || lower == "yes"
        || lower == "on"
    {
        return Some(true);
    }
    if lower == "0"
        || lower == "f"
        || lower == "false"
        || lower == "n"
        || lower == "no"
        || lower == "off"
    {
        return Some(false);
    }
    None
}

/// Leave non-boolean values untouched so they flow through to their own
/// validation — Go's `normalizeBoolValue` (`json.go:134-140`).
#[must_use]
pub fn normalize_bool_value(value: Value) -> Value {
    match to_bool(value.clone()) {
        (b, true) => Value::Bool(b),
        (_, false) => value,
    }
}

/// The payload keys, taken out first so the normalisation loops below can hold
/// a mutable borrow of the map.
#[must_use]
fn field_keys(payload: &Fields) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in payload.keys() {
        keys.push(key.to_string());
    }
    keys
}

/// Normalize every value of a leaf payload — Go's `normalizeAllBoolFields`
/// (`json.go:98-102`), used by `isMissing`/`isPresent` (`json.go:89`,
/// `json.go:92`) where the field name is irrelevant.
pub fn normalize_all_bool_fields(payload: &mut Fields) {
    for key in field_keys(payload) {
        if let Some(value) = payload.get(&key) {
            let normalized = normalize_bool_value(value.clone());
            payload.insert(key, normalized);
        }
    }
}

/// Normalize only the values of fields the registry flags `Boolean` — Go's
/// `normalizeBoolFields` (`json.go:104-111`; the per-key `LookupField` is
/// `json.go:106`, whose package-global registry becomes this `map` parameter,
/// the same threading as [`crate::fields`] and [`crate::sort`]).
pub fn normalize_bool_fields(map: &FieldMap, payload: &mut Fields) {
    for key in field_keys(payload) {
        let (info, _, ok) = lookup_field(map, key.to_string());
        if ok && info.boolean {
            if let Some(value) = payload.get(&key) {
                let normalized = normalize_bool_value(value.clone());
                payload.insert(key, normalized);
            }
        }
    }
}

/// Re-parent a parsed JSON object as a leaf payload — the `make(map[string]any)`
/// unmarshal of Go's `unmarshalExpression` (`json.go:51-55`). A JSON value that
/// is not an object yields `None`, which is what lets the caller fall back to
/// the conjunction arm (`json.go:23-25`).
#[must_use]
pub fn payload_of_object(value: &Value) -> Option<Fields> {
    match value.as_object() {
        Some(map) => {
            let mut payload: Fields = Fields::new();
            for (key, entry) in map.iter() {
                payload.insert(key.to_string(), entry.clone());
            }
            Some(payload)
        }
        None => None,
    }
}

/// The operator-key dispatch — Go's `unmarshalExpression` (`json.go:50-96`).
/// `op_name` is expected already lower-cased (the caller folds it at
/// `json.go:21`); `None` means "not a known operator" (`json.go:95`).
#[must_use]
pub fn unmarshal_expression(
    map: &FieldMap,
    op_name: String,
    raw_value: Value,
) -> Option<Expression> {
    let mut values: Fields = payload_of_object(&raw_value)?;
    // `json.go:56` — normalization runs before the operator switch.
    normalize_bool_fields(map, &mut values);
    match op_name.as_str() {
        "is" => Some(Expression::Is(values)),
        "isnot" => Some(Expression::IsNot(values)),
        "gt" => Some(Expression::Gt(values)),
        "lt" => Some(Expression::Lt(values)),
        "contains" => Some(Expression::Contains(values)),
        "notcontains" => Some(Expression::NotContains(values)),
        "startswith" => Some(Expression::StartsWith(values)),
        "endswith" => Some(Expression::EndsWith(values)),
        "intherange" => Some(Expression::InTheRange(values)),
        "before" => Some(Expression::Before(values)),
        "after" => Some(Expression::After(values)),
        "inthelast" => Some(Expression::InTheLast(values)),
        "notinthelast" => Some(Expression::NotInTheLast(values)),
        "inplaylist" => Some(Expression::InPlaylist(values)),
        "notinplaylist" => Some(Expression::NotInPlaylist(values)),
        "ismissing" => {
            // `json.go:89-90`
            normalize_all_bool_fields(&mut values);
            Some(Expression::IsMissing(values))
        }
        "ispresent" => {
            // `json.go:91-92`
            normalize_all_bool_fields(&mut values);
            Some(Expression::IsPresent(values))
        }
        _ => None,
    }
}

/// The JSON type name Go's decoder prints for a value — the `%s` of
/// `json.UnmarshalTypeError.Value`: `null`, `bool`, `number`, `string`,
/// `array`, `object`.
#[must_use]
pub(crate) fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Go's `unmarshalConjunctionType.UnmarshalJSON` (`json.go:12-34`) over an
/// already-parsed [`Value`]: a list of single-key objects, each key lower-cased
/// (`json.go:21`) and tried as an operator first (`json.go:22`), then as a
/// nested conjunction (`json.go:24`). A key that matches neither is an error
/// (`json.go:26-28`), and so is a value that is not a list of objects
/// (Go's `json.Unmarshal` failure, `json.go:14`).
///
/// # Errors
/// [`String`] carrying `invalid expression key '%s'` (`json.go:27`) or the
/// Go-shaped `json: cannot unmarshal …` message Go's `json.Unmarshal` raises
/// for a non-list value / a non-object item (`json.go:14`). Errors here are
/// what makes `{"any":42}` fail the whole criteria parse instead of silently
/// defaulting the slot.
///
pub fn unmarshal_conjunction_type_value(
    map: &FieldMap,
    raw_value: Value,
) -> Result<Vec<Expression>, String> {
    // encoding/json leaves a nil slice unchanged when the JSON value is null.
    let items: Vec<Value> = match raw_value.as_array() {
        Some(items) => items.clone(),
        None if raw_value.is_null() => Vec::new(),
        None => {
            return Err(format!(
                "json: cannot unmarshal {} into Go value of type []map[string]json.RawMessage",
                json_kind(&raw_value),
            ));
        }
    };
    let mut expressions: Vec<Expression> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        // Go decodes null into a map as a nil map; ranging over it yields no expressions.
        let Some(object) = item.as_object() else {
            if item.is_null() {
                continue;
            }
            return Err(format!(
                "json: cannot unmarshal {} into .{index} of type map[string]json.RawMessage",
                json_kind(item),
            ));
        };
        for (key, value) in object.iter() {
            let lower: String = key.to_lowercase();
            let expr: Option<Expression> =
                match unmarshal_expression(map, lower.clone(), value.clone()) {
                    Some(found) => Some(found),
                    None => unmarshal_conjunction(map, lower.clone(), value.clone()),
                };
            match expr {
                Some(found) => expressions.push(found),
                None => {
                    return Err(format!("invalid expression key '{}'", lower));
                }
            }
        }
    }
    Ok(expressions)
}

/// The conjunction-key dispatch — Go's `unmarshalConjunction`
/// (`json.go:142-155`): the raw value must itself be a list of expressions
/// (`json.go:144-147`), otherwise `None`, which is what makes the caller report
/// `invalid expression key` (`json.go:26-28`).
#[must_use]
pub fn unmarshal_conjunction(
    map: &FieldMap,
    conj_name: String,
    raw_value: Value,
) -> Option<Expression> {
    match unmarshal_conjunction_type_value(map, raw_value) {
        Ok(items) => match conj_name.as_str() {
            "any" => Some(Expression::Any(items)),
            "all" => Some(Expression::All(items)),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Go's `optionalConjunction` (`json.go:40-48`): the top-level `all`/`any`
/// value plus whether the key appeared at all. `encoding/json` calls
/// `UnmarshalJSON` even for a JSON `null` (`json.go:36-39`), so `present` is
/// true for `[]` and `null` alike; only an absent key leaves it false.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptionalConjunction {
    /// Whether the key was present in the JSON object (`json.go:41`).
    pub present: bool,
    /// The decoded rules — empty when the value was `[]` or `null` (`json.go:42`).
    pub rules: Vec<Expression>,
}

/// Read one top-level `all`/`any` slot out of a parsed JSON object — the
/// `optionalConjunction` decoder (`json.go:45-48`).
///
/// # Errors
/// The [`unmarshal_conjunction_type_value`] error when the key is present but
/// its value is not a list of expressions (Go propagates the same error through
/// `json.Unmarshal`, `json.go:47`).
pub fn unmarshal_optional_conjunction(
    map: &FieldMap,
    object: &Value,
    key: String,
) -> Result<OptionalConjunction, String> {
    match object.get(key.as_str()) {
        Some(value) => {
            let rules: Vec<Expression> = match value {
                // `json.Unmarshal(null, &slice)` yields an empty slice (`json.go:38`).
                Value::Null => Vec::new(),
                other => unmarshal_conjunction_type_value(map, other.clone())?,
            };
            Ok(OptionalConjunction {
                present: true,
                rules,
            })
        }
        None => Ok(OptionalConjunction::default()),
    }
}

/// A JSON value as compact JSON text — the `json.Marshal(v)` of Go's
/// `marshalExpression` inner value (`json.go:164-169`). serde_json's object map
/// is sorted, matching Go's map marshalling.
#[must_use]
pub fn value_json(value: Value) -> String {
    match serde_json::to_string(&value) {
        Ok(text) => text,
        // Cannot happen for a `Value` (no maps with non-string keys), kept
        // total so callers stay match-based.
        Err(_) => "null".to_string(),
    }
}

/// The single-entry leaf — Go's `marshalExpression` (`json.go:157-174`).
///
/// # Errors
/// [`String`] when the payload's length is not 1 (`json.go:158-160`), mirroring
/// Go's `fmt.Errorf`, or when the inner value fails to marshal (`json.go:164`).
pub fn marshal_expression_leaf(name: String, value: &Fields) -> Result<String, String> {
    if value.len() != 1 {
        return Err(format!(
            "invalid {} expression length {} for values {}",
            name,
            value.len(),
            value_json(Value::Object(value.clone().into_iter().collect())),
        ));
    }
    let inner: String = match serde_json::to_string(value) {
        Ok(text) => text,
        Err(e) => return Err(e.to_string()),
    };
    Ok(format!("{{\"{}\":{}}}", name, inner))
}

/// Go's `marshalConjunction` (`json.go:176-186`): the aux struct with only the
/// matching key set (`any` when `name == "any"`, otherwise `all`), so the other
/// key is omitted by `json:"…,omitempty"` (`json.go:178-179`) and an empty list
/// marshals as `{}`.
///
/// # Errors
/// Propagated from the elements' own marshalling (`json.go:186`
/// `json.Marshal(aux)` recursing into each expression's `MarshalJSON`).
pub fn marshal_conjunction(name: String, conj: &[Expression]) -> Result<String, String> {
    if conj.is_empty() {
        return Ok("{}".to_string());
    }
    let mut items: Vec<String> = Vec::new();
    for expr in conj {
        items.push(marshal_expression(expr)?);
    }
    let key: String = match name.as_str() {
        "any" => "any".to_string(),
        _ => "all".to_string(),
    };
    Ok(format!("{{\"{}\":[{}]}}", key, items.join(",")))
}

/// `json.Marshal` of one expression — Go dispatches through the type's own
/// `MarshalJSON`: the conjunctions delegate to `marshalConjunction`
/// (`operators.go:16`/`:31`) and the leaves to `marshalExpression`
/// (`operators.go:42`/…), keyed here by
/// [`crate::operators::expression_key`].
///
/// # Errors
/// Propagated from the leaf/conjunction helpers.
pub fn marshal_expression(expr: &Expression) -> Result<String, String> {
    let name: String = crate::operators::expression_key(expr.clone());
    match expr {
        Expression::All(items) => marshal_conjunction("all".to_string(), items),
        Expression::Any(items) => marshal_conjunction("any".to_string(), items),
        other => match crate::operators::fields(other.clone()) {
            Some(values) => marshal_expression_leaf(name, &values),
            // Unreachable once the enum is closed: every non-conjunction variant
            // carries a `Fields` payload (`operators.go` `fields()` methods).
            None => Ok("{}".to_string()),
        },
    }
}

#[cfg(test)]
mod json_test {
    use super::*;
    use crate::fields::default_field_map;

    /// A single-entry string leaf payload (Go's `Contains{"title": "love"}`).
    fn title_leaf(text: String) -> Fields {
        let mut leaf: Fields = Fields::new();
        leaf.insert("title".to_string(), Value::String(text));
        leaf
    }

    #[test]
    fn to_bool_accepts_bools_parseable_strings_and_zero_or_one() {
        // Arrange: the four arms of Go's `ToBool` (`json.go:116-131`).
        // Act + Assert: bool arm (`json.go:118-119`).
        let (b, ok) = to_bool(Value::Bool(false));
        assert!(ok, "json.go:118 — a real bool coerces");
        assert!(!b, "json.go:119");
        // Assert: string arm (`json.go:120-122`, `strconv.ParseBool`).
        let (yes, yes_ok) = to_bool(Value::String("YES".to_string()));
        assert!(yes_ok, "json.go:121 — ParseBool accepts YES");
        assert!(yes, "json.go:122");
        let (no, no_ok) = to_bool(Value::String("False".to_string()));
        assert!(no_ok, "json.go:121 — ParseBool accepts False");
        assert!(!no, "json.go:122");
        let (_, bad_ok) = to_bool(Value::String("maybe".to_string()));
        assert!(!bad_ok, "json.go:131 — an unparseable string is ok=false");
        // Assert: number arm (`json.go:123-129`, exactly 0 or 1).
        let (one, one_ok) = to_bool(serde_json::from_str("1").unwrap_or(Value::Null));
        assert!(one_ok, "json.go:125 — 1 coerces to true");
        assert!(one, "json.go:125");
        let (zero, zero_ok) = to_bool(serde_json::from_str("0").unwrap_or(Value::Null));
        assert!(zero_ok, "json.go:128 — 0 coerces to false");
        assert!(!zero, "json.go:128");
        let (_, two_ok) = to_bool(serde_json::from_str("2").unwrap_or(Value::Null));
        assert!(!two_ok, "json.go:131 — 2 is not a bool");
        // Assert: fallthrough arm (`json.go:131`).
        let (_, null_ok) = to_bool(Value::Null);
        assert!(!null_ok, "json.go:131 — nil is not a bool");
    }

    #[test]
    fn unmarshal_expression_dispatches_lower_cased_operator_keys() {
        // Arrange: the registry Go reads for `normalizeBoolFields` (`json.go:106`).
        let map = default_field_map();
        // Act:
        let parsed = serde_json::from_str::<Value>(r#"{"title": "love"}"#).unwrap_or(Value::Null);
        let expr = unmarshal_expression(&map, "is".to_string(), parsed);
        let upper = serde_json::from_str::<Value>(r#"{"title": "love"}"#).unwrap_or(Value::Null);
        let upper_expr = unmarshal_expression(&map, "IS".to_string(), upper);
        // Assert: the dispatch is by the (already folded) op name; the caller
        // lower-cases it (`json.go:21`), so an unfolded name finds nothing.
        match expr {
            Some(Expression::Is(values)) => {
                assert_eq!(values, title_leaf("love".to_string()), "json.go:59");
            }
            other => panic!("json.go:59 — expected Is, got {other:?}"),
        }
        assert!(
            upper_expr.is_none(),
            "json.go:95 — the caller folds the key, unmarshalExpression does not"
        );
    }

    #[test]
    fn unmarshal_expression_normalizes_boolean_fields_before_dispatch() {
        // Arrange: `loved` is a boolean field (`fields.go:38`), and
        // `json.go:105-110` normalizes its value before the switch.
        let map = default_field_map();
        // Act:
        let parsed = serde_json::from_str::<Value>(r#"{"loved": "true"}"#).unwrap_or(Value::Null);
        let expr = unmarshal_expression(&map, "is".to_string(), parsed);
        // Assert: the string became a real bool (`json.go:108`).
        match expr {
            Some(Expression::Is(values)) => {
                assert_eq!(values.get("loved"), Some(&Value::Bool(true)), "json.go:108");
            }
            other => panic!("json.go:108 — expected Is with a bool, got {other:?}"),
        }
    }

    #[test]
    fn ismissing_and_ispresent_normalize_every_value() {
        // Arrange: `json.go:89-92` — `isMissing`/`isPresent` normalize all
        // fields, not just boolean-registered ones.
        let map = default_field_map();
        // Act:
        let parsed = serde_json::from_str::<Value>(r#"{"title": "on"}"#).unwrap_or(Value::Null);
        let expr = unmarshal_expression(&map, "ismissing".to_string(), parsed);
        // Assert: `title` is not registered boolean, yet `on` coerced.
        match expr {
            Some(Expression::IsMissing(values)) => {
                assert_eq!(values.get("title"), Some(&Value::Bool(true)), "json.go:100");
            }
            other => panic!("json.go:90 — expected IsMissing, got {other:?}"),
        }
    }

    #[test]
    fn conjunction_values_parse_and_reject_unknown_keys() {
        // Arrange: the registry and a nested conjunction (`json.go:24`).
        let map = default_field_map();
        // Act:
        let nested = serde_json::from_str::<Value>(r#"[{"all": [{"is": {"loved": true}}]}]"#)
            .unwrap_or(Value::Null);
        let expr = unmarshal_conjunction(&map, "any".to_string(), nested);
        // Assert: the nested `all` is reached through the conjunction arm.
        match expr {
            Some(Expression::Any(items)) => {
                assert_eq!(items.len(), 1, "json.go:150 — one child expression");
                match items.first() {
                    Some(Expression::All(children)) => {
                        assert_eq!(children.len(), 1, "json.go:152");
                    }
                    other => panic!("json.go:152 — expected All, got {other:?}"),
                }
            }
            other => panic!("json.go:150 — expected Any, got {other:?}"),
        }
        // Act: an unknown operator key and an unknown conjunction key.
        let bogus =
            serde_json::from_str::<Value>(r#"[{"bogus": {"title": "x"}}]"#).unwrap_or(Value::Null);
        let err = unmarshal_conjunction_type_value(&map, bogus)
            .expect_err("json.go:27 — an unmatched key is an error");
        // Assert:
        assert!(
            err.contains("invalid expression key 'bogus'"),
            "json.go:27 — got {err}"
        );
    }

    #[test]
    fn nested_null_conjunctions_and_null_elements_match_go_json() {
        let map = default_field_map();
        for text in [
            r#"{"all":[{"any":null}]}"#,
            r#"{"all":[null,{"is":{"artist":"x"}}]}"#,
        ] {
            let parsed = crate::criteria::unmarshal_criteria_str(&map, text.to_string());
            assert!(
                parsed.is_ok(),
                "Go encoding/json null slice/map behavior: {text}: {parsed:?}"
            );
        }
    }

    #[test]
    fn optional_conjunction_remember_which_keys_were_present() {
        // Arrange: `json.go:36-48` — `present` is true for `[]` and `null`
        // alike, and false only for an absent key.
        let map = default_field_map();
        let empty = serde_json::from_str::<Value>(r#"{"any": []}"#).unwrap_or(Value::Null);
        let null = serde_json::from_str::<Value>(r#"{"any": null}"#).unwrap_or(Value::Null);
        let absent = serde_json::from_str::<Value>(r#"{}"#).unwrap_or(Value::Null);
        // Act:
        let empty_slot =
            unmarshal_optional_conjunction(&map, &empty, "any".to_string()).unwrap_or_default();
        let null_slot =
            unmarshal_optional_conjunction(&map, &null, "any".to_string()).unwrap_or_default();
        let absent_slot =
            unmarshal_optional_conjunction(&map, &absent, "any".to_string()).unwrap_or_default();
        // Assert:
        assert!(empty_slot.present, "json.go:46 — the key appeared");
        assert!(
            empty_slot.rules.is_empty(),
            "json.go:42 — an empty list has no rules"
        );
        assert!(
            null_slot.present,
            "json.go:38 — UnmarshalJSON runs even for null"
        );
        assert!(null_slot.rules.is_empty(), "json.go:38");
        assert!(
            !absent_slot.present,
            "json.go:40 — an absent key leaves present false"
        );
    }

    #[test]
    fn marshal_expression_leaf_renders_one_entry_and_rejects_others() {
        // Arrange: Go's `marshalExpression` (`json.go:157-174`).
        // Act:
        let one = marshal_expression_leaf("contains".to_string(), &title_leaf("love".to_string()));
        let mut two: Fields = Fields::new();
        two.insert("title".to_string(), Value::String("love".to_string()));
        two.insert("album".to_string(), Value::String("best of".to_string()));
        let two_err = marshal_expression_leaf("contains".to_string(), &two)
            .expect_err("json.go:158 — a two-entry payload is an error");
        // Assert:
        match one {
            Ok(text) => {
                assert_eq!(
                    text, "{\"contains\":{\"title\":\"love\"}}",
                    "json.go:162-172 — the canonical camelCase key wraps the map",
                );
            }
            Err(e) => panic!("json.go:174 — expected a wire form, got {e}"),
        }
        assert!(
            two_err.contains("invalid contains expression length 2"),
            "json.go:159 — got {two_err}"
        );
    }

    #[test]
    fn marshal_conjunction_wraps_children_and_empty_is_empty_object() {
        // Arrange: Go's `marshalConjunction` (`json.go:176-186`).
        let child: Expression = Expression::Contains(title_leaf("love".to_string()));
        // Act:
        let all = marshal_conjunction("all".to_string(), std::slice::from_ref(&child))
            .unwrap_or_default();
        let any = marshal_conjunction("any".to_string(), std::slice::from_ref(&child))
            .unwrap_or_default();
        let empty = marshal_conjunction("all".to_string(), &[]).unwrap_or_default();
        // Assert: only the matching key is present (`json.go:178-185`).
        assert_eq!(
            all, "{\"all\":[{\"contains\":{\"title\":\"love\"}}]}",
            "json.go:183-185",
        );
        assert_eq!(
            any, "{\"any\":[{\"contains\":{\"title\":\"love\"}}]}",
            "json.go:181-182",
        );
        assert_eq!(empty, "{}", "json.go:177-180 — both keys omitted");
    }
}
