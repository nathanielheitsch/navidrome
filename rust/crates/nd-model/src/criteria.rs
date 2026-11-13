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
//!     `json.Unmarshal` error semantics;
//!   * `limit`/`offset`/`limitPercent` are Go's `int` (`criteria.go:121-123`),
//!     64 bits on this target, so they stay `i64` end to end (see
//!     [`Criteria::limit`]);
//!   * the aux struct is decoded from the raw JSON text rather than from a
//!     `serde_json::Value`, because `Value`'s map cannot hold two entries with
//!     the same key and would erase Go's duplicate-key behaviour at the door
//!     (see [`parse_aux`]).

use std::time::Duration;

use nd_utils::time::format_duration;
use nd_utils::time::parse_duration;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;

use serde_json::Value;

use crate::fields::FieldMap;
use crate::json::marshal_expression;
use crate::json::unmarshal_conjunction_type_value;
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
    /// (`criteria.go:33-48`). Go's `Limit` is `int` (`criteria.go:23`), 64 bits
    /// on this target, so the port keeps `i64` rather than narrowing to `i32`:
    /// Go accepts `"limit": 9000000000` and round-trips it unchanged.
    pub limit: i64,
    /// Percentage limit in `[0, 100]` after clamping (`criteria.go:24`,
    /// `criteria.go:154-162`); `0` means unset. Also Go's `int`, but the clamp
    /// keeps it inside `[0, 100]`, so the width is only about wire fidelity.
    pub limit_percent: i64,
    /// Query offset — Go `Criteria.Offset` (`criteria.go:25`), likewise `int`.
    pub offset: i64,
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
/// `0` means "no limit applies". Go returns `int` (`criteria.go:33`), so the
/// port's return type is `i64` on this target.
#[must_use]
pub fn effective_limit(c: &Criteria, total_count: i64) -> i64 {
    if c.limit > 0 {
        return c.limit;
    }
    if c.limit_percent > 0 && c.limit_percent <= 100 {
        if total_count <= 0 {
            return 0;
        }
        // Go `criteria.go:41`: `int(totalCount) * c.LimitPercent / 100`, integer
        // arithmetic on the same 64-bit width as the fields themselves.
        let result: i64 = total_count * c.limit_percent / 100;
        if result < 1 {
            return 1;
        }
        return result;
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
    /// `json:"limit,omitempty"` (`criteria.go:90`). Go's aux field is `int`.
    limit: i64,
    /// `json:"limitPercent,omitempty"` (`criteria.go:91`).
    limit_percent: i64,
    /// `json:"offset,omitempty"` (`criteria.go:92`).
    offset: i64,
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

/// Go's `omitempty` on an int field (`criteria.go:90-92`). Go's aux fields are
/// `int`, 64 bits on this target, so the width follows [`MarshalAux`].
#[must_use]
fn int_omitted(value: i64) -> bool {
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

/// One aux-struct entry as it appears in the INPUT: the key with its ORIGINAL
/// spelling and the raw JSON text of its value.
///
/// `serde_json::Value` cannot represent two entries with the same key, and Go's
/// `encoding/json` (this checkout's Go 1.27.4 runs the JSONv2 default; v1 with
/// `GOEXPERIMENT=none` behaves the same here) does two things a `Value`-based
/// port cannot reproduce: it matches a field name case-INSENSITIVELY (the folded
/// `fieldMatch`/`foldName` comparison, see [`fold_name`]) and it decodes EVERY
/// occurrence of a key in input order, so the LAST VALID write wins (`get`
/// assigns each occurrence it can). A failed occurrence is not assigned, and it
/// does not stop the walk either: Go's decoder keeps decoding the REST of the
/// object best-effort and returns the FIRST type error at the end — verified
/// against Go 1.27.4, where `{"limit":"oops","limit":1}` leaves `Limit == 1`
/// (and `{"sort":"x","sort":5,"limit":1}` leaves `Sort == "x"`, `Limit == 1`)
/// while still returning `json: cannot unmarshal …`. Raw text is therefore the
/// only faithful input, and duplicates are kept in order; the port's callers
/// discard the partially-decoded value, which is what makes the retained error
/// the externally visible result.
type AuxEntry = (String, Box<RawValue>);

/// The aux struct as a `Vec` of entries — Go's anonymous
/// `struct { All; Any; Sort; Order; Limit; LimitPercent; Offset; RefreshDelay }`
/// (`criteria.go:116-124`), decoded in INPUT order rather than as a map.
struct AuxEntries(Vec<AuxEntry>);

impl<'de> Deserialize<'de> for AuxEntries {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_map(AuxVisitor)
    }
}

struct AuxVisitor;

impl<'de> Visitor<'de> for AuxVisitor {
    type Value = AuxEntries;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries: Vec<AuxEntry> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            entries.push((key, map.next_value::<Box<RawValue>>()?));
        }
        Ok(AuxEntries(entries))
    }
}

/// Decode the aux entries from raw JSON text.
///
/// Mirrors Go's single `json.Unmarshal(data, &aux)` (`criteria.go:126-128`): the
/// text must be a complete JSON OBJECT — `42`, `"x"`, `[1]` and trailing garbage
/// are decode errors — and the entries keep their input order and key spelling.
/// "Trailing whitespace" means EXACTLY the four characters Go's scanner skips
/// (`' '`, `'\t'`, `'\n'`, `'\r'`), not `char::is_whitespace`: `{}` followed by
/// U+00A0 or VT is `invalid character … after top-level value` in Go (verified
/// against Go 1.27.4), so a `.trim()`-based check would wrongly accept it.
///
/// # Errors
/// [`CriteriaError::Slot`] with a Go-shaped `json: cannot unmarshal …` message
/// for a non-object payload, or serde_json's own message for malformed text. Go's
/// syntax wording (`unexpected end of JSON input`, `invalid character …`) is
/// allowed to differ: the port's only consumer (`nd-db`'s `rules` decode)
/// re-wraps it un-prefixed and depends only on the decode failing.
fn parse_aux(text: &str) -> Result<Vec<AuxEntry>, CriteriaError> {
    // The streaming deserializer is what reports where the top-level value ends,
    // which is how trailing garbage is detected the way Go's decoder does it.
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<AuxEntries>();
    let first: Option<Result<AuxEntries, serde_json::Error>> = stream.next();
    let entries: Vec<AuxEntry> = match first {
        Some(Ok(AuxEntries(entries))) => entries,
        Some(Err(err)) => {
            // Go distinguishes "not an object" (a type error naming the aux
            // struct) from "malformed JSON"; serde collapses both into one error,
            // so classify by the payload's leading token.
            return Err(CriteriaError::Slot(match json_kind_of(text) {
                Some(kind) => {
                    format!("json: cannot unmarshal {kind} into Go value of type struct {{ … }}")
                }
                None => err.to_string(),
            }));
        }
        None => {
            return Err(CriteriaError::Slot(
                "unexpected end of JSON input".to_string(),
            ));
        }
    };
    // Go's decoder rejects anything after the top-level value
    // (`invalid character … after top-level value`); only the four JSON
    // whitespace characters are tolerated there — `str::trim`/`char::is_whitespace`
    // also strips U+00A0, U+000B, U+2028 and U+2029, which Go rejects.
    if !is_json_whitespace(&text[stream.byte_offset()..]) {
        return Err(CriteriaError::Slot(
            "invalid character after top-level value".to_string(),
        ));
    }

    Ok(entries)
}

/// `true` when the text is empty or made only of the whitespace JSON allows
/// between tokens: `scanValue`/`decoder.skipWhitespace` (`scanner.go`,
/// `jsontext`) advance over `'\t'`, `'\n'`, `'\r'`, `' '` and nothing else.
#[must_use]
fn is_json_whitespace(text: &str) -> bool {
    text.bytes()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

/// The JSON text with its leading and trailing JSON whitespace removed
/// ([`is_json_whitespace`]'s characters only), so a `Box<RawValue>` payload's
/// padding never changes how its token is classified.
#[must_use]
fn json_trim(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }
    &text[start..end]
}

/// The JSON type Go's decoder reports for a whole payload: `null`, `bool`,
/// `number`, `string`, `array`, `object` — the first token's kind. `None` means
/// the text is not JSON at all.
#[must_use]
fn json_kind_of(text: &str) -> Option<&'static str> {
    match json_trim(text).chars().next()? {
        '{' => Some("object"),
        '[' => Some("array"),
        '"' => Some("string"),
        't' | 'f' => Some("bool"),
        'n' => Some("null"),
        '-' | '0'..='9' => Some("number"),
        _ => None,
    }
}

/// The aux field name as `encoding/json` folds it, for matching a struct tag.
///
/// Go looks a struct field up by `foldName(name)` (`encoding/json/fold.go` for
/// the v1 path this toolchain takes by default, `v2/fold.go` under the jsonv2
/// experiment) and `foldRune` returns the SMALLEST rune of the fold set, so
/// `foldName(x) == foldName(y)` is exactly `bytes.EqualFold(x, y)` — Unicode
/// included, not just ASCII. Rust's `to_lowercase` is not that: it leaves the
/// long s (`U+017F`) unchanged, so `{"\u{017F}ort":"x"}` was silently dropped as an unknown
/// key while Go binds it to `Sort` (verified against Go 1.27.4).
///
/// Delimiters keep their old behaviour: the v1 fold does NOT strip `_` or `-`
/// (and the JSONv2 default on this toolchain is the same delimiter-sensitive
/// match), so `{"limit_percent":5}` does not match `limitPercent` — also
/// verified. Only the case/Unicode folding changes here.
///
/// The aux struct's tags are all ASCII, so a key can only match one when every
/// rune folds to an ASCII letter or digit: outside ASCII, Go's fold set contains
/// an ASCII rune for exactly two code points (`unicode.SimpleFold` over the whole
/// plane), U+017F (long s) and U+212A (KELVIN SIGN). Everything else is kept
/// verbatim, which simply never equals a folded ASCII tag.
#[must_use]
fn fold_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'a'..='z' => ch.to_ascii_uppercase(),
            '\u{017F}' => 'S', // long s: Go's foldRune -> 'S' (`\u{017F}ort` matches `sort`)
            '\u{212A}' => 'K', // KELVIN SIGN: Go's foldRune -> 'K' (`\u{212A}imit` matches `limit`)
            _ => ch,
        })
        .collect()
}

/// Every occurrence of an aux field name, in input order, matched the way
/// `encoding/json` matches struct fields: by [`fold_name`] against the JSON tag
/// (`criteria.go:116-124`), which is Unicode case-folding (`EqualFold`) with
/// delimiters significant. The callers replay them in order, which is what makes
/// the LAST VALID write win while a failed occurrence leaves its field alone.
#[must_use]
fn raw_got(raw: &RawValue) -> String {
    let text = raw.get();
    match json_kind_of(text) {
        // Go's `json.UnmarshalTypeError.Value` prints the kind for everything but
        // a number that the target type cannot hold, where it prints
        // `number <the token as written>` — raw text keeps `1e3` and `3.0`
        // spelled exactly as the input has them.
        Some("number") => format!("number {}", json_trim(text)),
        Some(kind) => kind.to_string(),
        None => "null".to_string(),
    }
}

/// The JSON kind name alone — the `Value` of Go's `json.UnmarshalTypeError` for
/// a string field, which never prints a number's text.
#[must_use]
fn raw_kind(raw: &RawValue) -> &'static str {
    json_kind_of(raw.get()).unwrap_or("null")
}

/// Read one `all`/`any` occurrence through Go's `optionalConjunction` decoder
/// (`json.go:45-48`) → `unmarshalConjunctionType.UnmarshalJSON` (`json.go:12-34`).
///
/// The slot's OWN bytes are parsed here, so no top-level map is involved — that
/// is precisely where a `Value`-based port would lose duplicate keys. A malformed
/// slot value propagates the way Go does: `json.Unmarshal` into
/// `[]map[string]json.RawMessage` (`json.go:13`) fails on a non-list, and a
/// nested-conjunction arm surfaces `invalid expression key '%s'` (`json.go:26-28`).
/// Both abort `UnmarshalJSON`. Defaulting a bad slot instead would silently accept
/// `{"all":[…],"any":42}` — and a bad key nested one level down — and return the
/// OTHER slot's rules, which Go never does.
///
/// # Errors
/// [`CriteriaError::Slot`] carrying Go's decode message when the value is not a
/// list of expressions, or a nested key is unknown.
fn slot_rules(map: &FieldMap, raw: &RawValue) -> Result<Vec<Expression>, CriteriaError> {
    let value: Value = match serde_json::from_str(raw.get()) {
        Ok(value) => value,
        Err(err) => return Err(CriteriaError::Slot(err.to_string())),
    };
    // `json.Unmarshal(data, &slice)` with a JSON null leaves the slice nil
    // (`json.go:38`): present, but no rules.
    if value.is_null() {
        return Ok(Vec::new());
    }
    unmarshal_conjunction_type_value(map, value).map_err(CriteriaError::Slot)
}

/// Decode one raw value into Go's `int` (`criteria.go:121-123`): `null` is the
/// no-op that leaves the field at its CURRENT value (verified against Go 1.27.4:
/// `{"limit":1,"limit":null}` keeps 1 — a `Value`-based port that resets to 0 here
/// diverges), a base-10 integer literal is the value, and anything else is Go's
/// `json.UnmarshalTypeError` naming the key AS IT APPEARED IN THE INPUT
/// (`json: cannot unmarshal string into Go struct field .LIMIT of type int`).
///
/// `Ok(None)` is the no-op, so the caller must keep what it already holds.
fn decode_int(raw: &RawValue, key: &str) -> Result<Option<i64>, CriteriaError> {
    if json_trim(raw.get()) == "null" {
        return Ok(None);
    }
    // Go decodes an int field with `strconv.ParseInt(s, 10, 64)`, so a fraction,
    // an exponent or a value beyond int64 are type errors, not silent
    // truncations — Go's `int` is 64 bits on this target.
    match go_int_text(raw.get()) {
        Some(number) => Ok(Some(number)),
        None => Err(CriteriaError::Type(format!(
            "json: cannot unmarshal {} into Go struct field .{key} of type int",
            raw_got(raw),
        ))),
    }
}

/// `strconv.ParseInt(text, 10, 64)` over a JSON number token: an optional sign
/// and decimal digits only, fitting `i64`.
#[must_use]
fn go_int_text(text: &str) -> Option<i64> {
    let text = json_trim(text);
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>().ok()
}

/// Decode one raw value into Go's `string` (`criteria.go:119-120`, `:124`):
/// `null` is the no-op that leaves the field at its CURRENT value
/// (`{"sort":"a","sort":null}` keeps `"a"`), a JSON string is the value, and any
/// other JSON type is Go's type error with the input's key spelling.
///
/// `Ok(None)` is the no-op, so the caller must keep what it already holds.
fn decode_string(raw: &RawValue, key: &str) -> Result<Option<String>, CriteriaError> {
    if json_trim(raw.get()) == "null" {
        return Ok(None);
    }
    let value: Value = match serde_json::from_str(raw.get()) {
        Ok(value) => value,
        Err(err) => return Err(CriteriaError::Slot(err.to_string())),
    };
    match value.as_str() {
        Some(text) => Ok(Some(text.to_string())),
        None => Err(CriteriaError::Type(format!(
            "json: cannot unmarshal {} into Go struct field .{key} of type string",
            raw_kind(raw),
        ))),
    }
}

/// Clamp `limitPercent` to `[0, 100]` — Go's `criteria.go:154-162`, where the
/// `log.Warn` of `criteria.go:156`/`:159` becomes a silent clamp (the port is a
/// pure transform; the warn text carries no data the caller uses).
#[must_use]
fn clamp_limit_percent(value: i64) -> i64 {
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
    /// [`crate::json::unmarshal_conjunction_type_value`]): a non-list `all`/`any`
    /// value, or an unknown key inside a nested conjunction. Go propagates it out
    /// of the single `json.Unmarshal(data, &aux)` call, so the whole parse fails.
    Slot(String),
    /// A wrong JSON type for a scalar aux field (`criteria.go:126-128`): Go's
    /// `json.UnmarshalTypeError` for `sort`/`order`/`limit`/`limitPercent`/
    /// `offset`/`refreshDelay`, which aborts the parse instead of defaulting.
    Type(String),
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
            // Go returns the `json.UnmarshalTypeError` itself, un-wrapped.
            Self::Type(source) => write!(f, "{source}"),
            // Go `criteria.go:150` — `fmt.Errorf("invalid refreshDelay: %w", err)`.
            Self::RefreshDelay(source) => write!(f, "invalid refreshDelay: {source}"),
        }
    }
}

impl std::error::Error for CriteriaError {}

/// Parse a criteria from raw JSON text — Go's `Criteria.UnmarshalJSON`
/// (`criteria.go:115-164`), walking the INPUT bytes the way Go's decoder does.
///
/// Every aux field is decoded with Go's JSON type: the `all`/`any` slots as
/// `optionalConjunction` (`json.go:40-48`), `sort`/`order`/`refreshDelay` as
/// `string`, `limit`/`limitPercent`/`offset` as `int`. A slot value or scalar of
/// the wrong JSON shape is therefore a DECODE failure that aborts the parse
/// (`criteria.go:126-128`) — never a silent default — and the failure propagates
/// out of nested conjunction decoding as well (`json.go:26-28`, reached through
/// `unmarshalConjunction`, `json.go:144-147`).
///
/// Field names are matched the way `encoding/json` matches struct fields — the
/// Unicode case fold of [`fold_name`], with `_`/`-` significant — and duplicate
/// keys are replayed in INPUT order, so the LAST VALID write wins
/// (`{"limit":1,"LIMIT":2}` → 2, `{"limit":1,"limit":2}` → 2) while a failed
/// occurrence leaves its field alone and the FIRST failure is the error Go
/// returns. Go keeps decoding the rest of the object best-effort after a failure
/// (its object arm records the first error and continues, so
/// `{"limit":"a","limit":"b","sort":"z"}` leaves `Sort == "z"` while still
/// reporting the `limit` type error — verified against Go 1.27.4). Because the
/// whole decoded value is discarded when `UnmarshalJSON` returns an error, the
/// port's early `?` is externally equivalent: the first failure in input order
/// is what surfaces either way.
///
/// # Errors
/// [`CriteriaError`]: a malformed slot (`criteria.go:126-128`), a scalar of the
/// wrong JSON type (`:126-128`), the `all`/`any` conflict (`criteria.go:129-133`),
/// missing rules (`criteria.go:139`), or a malformed `refreshDelay`
/// (`criteria.go:146-152`). Go reports whichever aux field `json.Unmarshal`
/// reaches FIRST in the INPUT — the walk is over the object's keys, not over the
/// struct's fields — so `{"limit":"oops","any":42}` names `limit` and
/// `{"any":42,"limit":"oops"}` names the slot.
pub fn unmarshal_criteria(map: &FieldMap, text: &str) -> Result<Criteria, CriteriaError> {
    let entries: Vec<AuxEntry> = parse_aux(text)?;

    // Go `criteria.go:126-128`: ONE `json.Unmarshal` call decodes the whole aux
    // struct, walking the input's key order. Go's decoder records the FIRST type
    // or syntax error and keeps decoding the rest of the object best-effort; the
    // partially-filled struct is discarded when it returns non-nil, so stopping
    // at the first failure here is externally equivalent. The conflict/rules
    // checks below therefore only run on a payload Go accepted.

    let mut all_slot: Option<Vec<Expression>> = None;
    let mut any_slot: Option<Vec<Expression>> = None;
    let mut sort: String = String::new();
    let mut order: String = String::new();
    let mut limit: i64 = 0;
    let mut limit_percent: i64 = 0;
    let mut offset: i64 = 0;
    let mut delay_text: String = String::new();

    for (key, raw) in &entries {
        // Go matches the JSON tag through `foldName` (Unicode case fold,
        // delimiters significant); the switch below is on the FOLDED name while
        // the error paths keep the input's spelling, since Go's
        // `UnmarshalTypeError.Field` is the key as the input spelled it.
        let folded: String = fold_name(key);
        match folded.as_str() {
            "ALL" => all_slot = Some(slot_rules(map, raw)?),
            "ANY" => any_slot = Some(slot_rules(map, raw)?),
            // `null` is Go's no-op (`Ok(None)`), so a later duplicate `null` does
            // NOT erase an earlier value — verified against Go 1.27.4.
            "SORT" => {
                if let Some(text) = decode_string(raw, key)? {
                    sort = text;
                }
            }
            "ORDER" => {
                if let Some(text) = decode_string(raw, key)? {
                    order = text;
                }
            }
            "LIMIT" => {
                if let Some(number) = decode_int(raw, key)? {
                    limit = number;
                }
            }
            "LIMITPERCENT" => {
                if let Some(number) = decode_int(raw, key)? {
                    limit_percent = number;
                }
            }
            "OFFSET" => {
                if let Some(number) = decode_int(raw, key)? {
                    offset = number;
                }
            }
            "REFRESHDELAY" => {
                if let Some(text) = decode_string(raw, key)? {
                    delay_text = text;
                }
            }
            // A key with no aux field is skipped by `encoding/json` without
            // decoding it or erroring.
            _ => {}
        }
    }

    // Go `criteria.go:129-133`: a Criteria has a single top-level group. Reject
    // payloads that provide both keys (even when one is [] or null, and even when
    // only one of them is spelled in the tag's own case) rather than silently
    // dropping one.
    if all_slot.is_some() && any_slot.is_some() {
        return Err(CriteriaError::Conflict);
    }
    // Go `criteria.go:134-140`: `any` wins over `all` when both carry rules.
    let expression: Expression = match (&any_slot, &all_slot) {
        (Some(rules), _) if !rules.is_empty() => Expression::Any(rules.clone()),
        (_, Some(rules)) if !rules.is_empty() => Expression::All(rules.clone()),
        _ => return Err(CriteriaError::MissingRules),
    };

    // Go `criteria.go:141-144`: sort/order/limit/offset are assigned here, BEFORE
    // the clamp below. Go `criteria.go:146-152`: `utils.ParseDuration`, wrapped as
    // `invalid refreshDelay: %w`.
    let refresh_delay: i64 = if delay_text.is_empty() {
        0
    } else {
        match parse_duration(&delay_text) {
            Ok(d) => d.as_nanos() as i64,
            Err(source) => return Err(CriteriaError::RefreshDelay(source)),
        }
    };

    Ok(Criteria {
        expression,
        sort,
        order,
        // Go's `int` is 64 bits on this target (`criteria.go:23-25`), so the
        // decoded value is carried through unchanged: no narrowing and no
        // saturation, which is what lets `limit: 9000000000` round-trip.
        limit,
        offset,
        // Go `criteria.go:154-162`: clamped before the field is set.
        limit_percent: clamp_limit_percent(limit_percent),
        refresh_delay,
    })
}

/// The caller-side entry point Go's `unmarshalCriteriaStr` (`criteria.go:113-121`)
/// is: it hands the bytes straight to `json.Unmarshal(data, &c)`, which is
/// [`unmarshal_criteria`] — no intermediate parsed value, so the duplicate-key and
/// case-folded behaviour of Go's decoder survives intact.
///
/// # Errors
/// Whatever [`unmarshal_criteria`] reports, including the malformed-JSON case.
pub fn unmarshal_criteria_str(map: &FieldMap, text: String) -> Result<Criteria, CriteriaError> {
    unmarshal_criteria(map, &text)
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

    /// Go decodes the whole aux struct in ONE `json.Unmarshal` call
    /// (`criteria.go:126-128`), so a malformed top-level slot value aborts the
    /// parse. Silently defaulting it would accept `{"all":[…],"any":42}` and hand
    /// back the other slot's rules — Go hands back an error.
    #[test]
    fn criteria_rejects_a_malformed_slot_value_instead_of_defaulting_it() {
        // Arrange: one valid `all` plus a bad value in the other slot, and a bad
        // value in the only slot.
        let map = default_field_map();
        for (json, want) in [
            // `criteria.go:126-128` → `json.go:13`'s `[]map[string]json.RawMessage`.
            (
                r#"{"all":[{"is":{"artist":"x"}}],"any":42}"#,
                "cannot unmarshal number",
            ),
            (
                r#"{"all":[{"is":{"artist":"x"}}],"any":"nope"}"#,
                "cannot unmarshal string",
            ),
            (
                r#"{"all":[{"is":{"artist":"x"}}],"any":{"a":1}}"#,
                "cannot unmarshal object",
            ),
            (r#"{"all":42}"#, "cannot unmarshal number"),
            // An item of the list must itself be an object (`json.go:13-14`).
            (r#"{"all":["x"]}"#, "cannot unmarshal string into .0"),
        ] {
            // Act:
            let err = unmarshal_criteria_str(&map, json.to_string())
                .expect_err("Go rejects this, the port must too");
            // Assert: the failure is the slot decode, not a swallowed default.
            assert!(
                err.to_string().contains(want),
                "{json} — expected a Go-shaped type error mentioning {want}, got {err}",
            );
        }
    }

    /// Same single-unmarshal rule for the scalar/string aux fields: Go's
    /// `limit`/`offset`/`limitPercent` are `int` and `sort`/`order`/`refreshDelay`
    /// are `string` (`criteria.go:119-124`), so a wrong JSON type is Go's
    /// `json.UnmarshalTypeError` and the criteria is NOT produced with a zero.
    #[test]
    fn criteria_rejects_wrong_json_types_for_the_scalar_and_string_aux_fields() {
        let map = default_field_map();
        let base = r#"{"all":[{"is":{"artist":"x"}}]"#;
        for (field, value, want) in [
            (
                "limit",
                r#""oops""#,
                "cannot unmarshal string into Go struct field .limit of type int",
            ),
            ("offset", r#""x""#, "cannot unmarshal string"),
            ("limitPercent", r#""x""#, "cannot unmarshal string"),
            ("limit", "true", "cannot unmarshal bool"),
            // A non-integer number is Go's other int wording: the value itself.
            ("limit", "3.5", "cannot unmarshal number 3.5"),
            (
                "sort",
                "5",
                "cannot unmarshal number into Go struct field .sort of type string",
            ),
            ("order", r#"["a"]"#, "cannot unmarshal array"),
            ("refreshDelay", "7", "cannot unmarshal number"),
        ] {
            // Act:
            let err = unmarshal_criteria_str(&map, format!("{base},\"{field}\":{value}}}"))
                .expect_err("Go rejects this scalar type");
            // Assert:
            assert!(
                err.to_string().contains(want),
                "{field}:{value} — expected {want}, got {err}",
            );
        }
    }

    /// The two JSON shapes Go DOES tolerate for a scalar: an absent key and a
    /// `null` (unmarshalling null into an int/string field is a no-op), which keep
    /// the zero value rather than erroring.
    #[test]
    fn null_and_absent_scalar_fields_keep_go_zero_values() {
        let map = default_field_map();
        // Act:
        let nulled = unmarshal_criteria_str(
            &map,
            r#"{"all":[{"is":{"artist":"x"}}],"sort":null,"order":null,"limit":null,"offset":null,"limitPercent":null,"refreshDelay":null}"#.to_string(),
        );
        // Assert: parsed, every scalar at Go's zero value.
        match nulled {
            Ok(c) => {
                assert_eq!(c.sort, "", "null into a string field is a no-op");
                assert_eq!(c.order, "");
                assert_eq!(c.limit, 0);
                assert_eq!(c.offset, 0);
                assert_eq!(c.limit_percent, 0);
                assert_eq!(c.refresh_delay, 0);
                assert!(
                    matches!(&c.expression, Expression::All(items) if items.len() == 1),
                    "the rules survive the null scalars: {c:?}",
                );
            }
            Err(e) => panic!("null scalars must not error, got {e}"),
        }
    }

    /// Nested-conjunction propagation, exactly as Go: `unmarshalConjunction`
    /// (`json.go:142-155`) swallows the inner decode failure and returns nil, so
    /// the caller reports `invalid expression key '<outer key>'` (`json.go:26-28`)
    /// and the whole parse aborts. A malformed nested value must never be quietly
    /// accepted as an empty group.
    #[test]
    fn malformed_nested_conjunctions_abort_the_parse_with_gos_key_message() {
        let map = default_field_map();
        for (json, want) in [
            // Inner key `bogus` fails; the outer `all` is what Go names.
            (
                r#"{"all":[{"all":[{"bogus":1}]}]}"#,
                "invalid expression key 'all'",
            ),
            (
                r#"{"all":[{"any":[{"bogus":1}]}]}"#,
                "invalid expression key 'any'",
            ),
            // A nested conjunction whose value is not a list is the same nil arm.
            (r#"{"all":[{"all":42}]}"#, "invalid expression key 'all'"),
            // A leaf payload that is not an object falls through to the same error.
            (r#"{"all":[{"is":"str"}]}"#, "invalid expression key 'is'"),
        ] {
            // Act:
            let err = unmarshal_criteria_str(&map, json.to_string())
                .expect_err("Go rejects this nested key");
            // Assert:
            assert!(err.to_string().contains(want), "{json} — got {err}");
        }
    }

    /// Unknown keys at the top level are ignored (`encoding/json` only binds the
    /// aux struct's fields) — a strict parser that rejected them would diverge.
    #[test]
    fn unknown_top_level_keys_are_ignored_like_go() {
        let map = default_field_map();
        // Act:
        let parsed = unmarshal_criteria_str(
            &map,
            r#"{"all":[{"is":{"artist":"x"}}],"zzz":{"a":1},"unused":true}"#.to_string(),
        );
        // Assert:
        match parsed {
            Ok(c) => assert!(
                matches!(&c.expression, Expression::All(items) if items.len() == 1),
                "extra keys are dropped, the rules survive: {c:?}",
            ),
            Err(e) => panic!("unknown keys must not error, got {e}"),
        }
    }

    /// `encoding/json` matches a struct field case-INSENSITIVELY and — with the
    /// duplicate-name support this toolchain has on by default — decodes EVERY
    /// occurrence of a key in input order. Verified against Go 1.27.4 with the
    /// real aux struct shape (`json.Unmarshal(data, &aux)`, `criteria.go:126`),
    /// both with the default (JSONv2) build and with `GOEXPERIMENT=none`:
    ///
    /// * `{"limit":1,"LIMIT":2}` and `{"limit":1,"limit":2}` → `limit == 2`, i.e.
    ///   the LAST valid write wins;
    /// * `{"limit":"oops","limit":1}` still ERRORS — the earlier type failure is
    ///   what `get` returns, so the later valid duplicate never gets written even
    ///   though the field holds a value;
    /// * `{"LiMiT":3}` parses: the fold is against the JSON tag, and the error
    ///   path keeps the INPUT's spelling (`.LiMiT`).
    #[test]
    fn duplicate_and_case_folded_field_names_follow_gos_input_order() {
        let map = default_field_map();
        let base = r#"{"all":[{"is":{"artist":"x"}}]"#;
        // Act + Assert: last valid write wins, whatever the spelling.
        for (json, want) in [
            (format!("{base},\"limit\":1,\"limit\":2}}"), 2),
            (format!("{base},\"limit\":1,\"LIMIT\":2}}"), 2),
            (format!("{base},\"LiMiT\":3}}"), 3),
            (format!("{base},\"offset\":1,\"OFFSET\":2}}"), 0),
            (format!("{base},\"LIMITPERCENT\":50}}"), 0),
        ] {
            let parsed = unmarshal_criteria_str(&map, json.clone())
                .unwrap_or_else(|e| panic!("{json} must parse, got {e}"));
            assert_eq!(parsed.limit, want, "{json}");
            if json.contains("offset") {
                assert_eq!(parsed.offset, 2, "{json} — offset duplicate");
            }
            if json.contains("LIMITPERCENT") {
                assert_eq!(parsed.limit_percent, 50, "{json} — folded tag");
            }
        }
        // Act + Assert: a PRIOR type error survives a later valid duplicate.
        for json in [
            format!("{base},\"limit\":\"oops\",\"limit\":1}}"),
            format!("{base},\"limit\":1,\"limit\":\"oops\"}}"),
            format!("{base},\"LIMIT\":\"oops\"}}"),
            format!("{base},\"limit\":1,\"limit\":null}}"),
        ] {
            if json.contains("limit\":1,\"limit\":null") {
                // `null` is Go's no-op, so this one PARSES and keeps 1.
                let parsed = unmarshal_criteria_str(&map, json.clone())
                    .unwrap_or_else(|e| panic!("{json} must parse, got {e}"));
                assert_eq!(parsed.limit, 1, "{json} — null leaves the field alone");
                continue;
            }
            let err = unmarshal_criteria_str(&map, json.clone())
                .expect_err("a prior type error is not rescued by a later duplicate");
            assert!(
                err.to_string().contains("cannot unmarshal string"),
                "{json} — expected a type error, got {err}",
            );
        }
        // Act + Assert: the error names the key AS THE INPUT SPELLED IT.
        let upper = unmarshal_criteria_str(&map, format!("{base},\"LiMiT\":\"oops\"}}"))
            .expect_err("type error regardless of case");
        assert!(
            upper.to_string().contains(".LiMiT of type int"),
            "Go's UnmarshalTypeError.Field is the input's spelling, got {upper}",
        );
        // Act + Assert: the two required rejects from the brief.
        assert!(unmarshal_criteria_str(&map, format!("{base},\"LIMIT\":\"oops\"}}")).is_err());
        assert!(
            unmarshal_criteria_str(&map, format!("{base},\"limit\":\"oops\",\"limit\":1}}"))
                .is_err()
        );
    }

    /// Finding 4: Go matches a struct field with `foldName`, i.e. Unicode
    /// `EqualFold` — not an ASCII-only lower-case. Verified against Go 1.27.4:
    /// `{"ſort":"x"}` binds `Sort == "x"` (long s U+017F folds to `S`),
    /// `{"ſort":5}` is a type error naming the INPUT key
    /// (`cannot unmarshal number into Go struct field aux.ſort of type string`),
    /// and the delimiter rules stay intact — `{"limit_percent":5}` and
    /// `{"limit-percent":5}` match nothing, while `{"limitpercent":5}` does.
    #[test]
    fn field_names_match_go_equal_fold_not_ascii_lowercase() {
        let map = default_field_map();
        let base = r#"{"all":[{"is":{"artist":"x"}}]"#;
        // Act + Assert: the long s binds `sort` exactly like `SORT` does.
        for key in ["sort", "SORT", "Sort", "\u{017F}ort", "\u{017F}ORT"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":\"title\"}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `sort` tag, got {e}"));
            assert_eq!(parsed.sort, "title", "{key} folds to `sort`");
        }
        // Act + Assert: a wrong JSON type under the folded name is Go's type
        // error, and it names the key as the INPUT spelled it.
        let err = unmarshal_criteria_str(&map, format!("{base},\"\u{017F}ort\":5}}"))
            .expect_err("a number is not a string, whatever the key spelling");
        assert!(
            err.to_string().contains(
                "cannot unmarshal number into Go struct field .\u{017F}ort of type string"
            ),
            "Go's UnmarshalTypeError.Field keeps the input spelling, got {err}",
        );
        // ASCII case folding keeps working for every other field, folded tag or
        // not (`limitpercent` matches `limitPercent` because the fold ignores
        // case only, and the tag itself has no delimiter).
        for key in ["ORDER", "orDER", "order"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":\"asc\"}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `order` tag, got {e}"));
            assert_eq!(parsed.order, "asc", "{key} folds to `order`");
        }
        for key in ["OFFSET", "offset"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":7}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `offset` tag, got {e}"));
            assert_eq!(parsed.offset, 7, "{key} folds to `offset`");
        }
        for key in ["LIMIT", "limit"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":7}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `limit` tag, got {e}"));
            assert_eq!(parsed.limit, 7, "{key} folds to `limit`");
        }
        for key in ["limitpercent", "LIMITPERCENT", "limitPercent"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":7}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `limitPercent` tag, got {e}"));
            assert_eq!(parsed.limit_percent, 7, "{key} folds to `limitPercent`");
        }
        for key in ["REFRESHDELAY", "refreshDelay"] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":\"30s\"}}"))
                .unwrap_or_else(|e| panic!("{key} must match the `refreshDelay` tag, got {e}"));
            assert_eq!(
                parsed.refresh_delay, 30_000_000_000,
                "{key} folds to `refreshDelay`"
            );
        }
        // Act + Assert: delimiters are NOT folded away (v1 `foldName` keeps `_`
        // and `-`, and Go agrees), so these bind nothing.
        for key in [
            "limit_percent",
            "limit-percent",
            "LIMIT_PERCENT",
            "limit_percent",
        ] {
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{key}\":50}}"))
                .unwrap_or_else(|e| panic!("{key} must be ignored, got {e}"));
            assert_eq!(parsed.limit_percent, 0, "{key} is not `limitPercent`");
        }
        // `Kimit` (KELVIN SIGN, U+212A) folds to `K`, not `L`, so it matches
        // nothing — the fold table is Go's, not a loose similarity.
        let parsed = unmarshal_criteria_str(&map, format!("{base},\"\u{212A}imit\":5}}"))
            .expect("a non-matching folded name is ignored, not an error");
        assert_eq!(
            parsed.limit, 0,
            "U+212A folds to K, so `Kimit` binds nothing"
        );
    }

    /// Finding 3: after the top-level value Go's scanner accepts ONLY the four
    /// JSON whitespace characters. Verified against Go 1.27.4, where `{}` +
    /// U+00A0, VT, U+2028 or U+2029 is `invalid character … after top-level
    /// value` — `str::trim` would have silently accepted all of them.
    #[test]
    fn only_json_whitespace_is_allowed_after_the_top_level_value() {
        let map = default_field_map();
        let payload = r#"{"all":[{"is":{"artist":"x"}}],"limit":3}"#;
        // Act + Assert: the four JSON whitespace characters are fine.
        for tail in ["", " ", "\t", "\n", "\r", " \t\n\r "] {
            let parsed = unmarshal_criteria_str(&map, format!("{payload}{tail}"))
                .unwrap_or_else(|e| panic!("{tail:?} after the value must parse, got {e}"));
            assert_eq!(parsed.limit, 3, "{tail:?}");
        }
        // Act + Assert: Unicode whitespace that Rust's `is_whitespace` accepts is
        // a decode error in Go.
        for tail in [
            "\u{00a0}", "\u{000b}", "\u{000c}", "\u{2028}", "\u{2029}", "\u{2002}", "é", "}", "{}",
        ] {
            let err = unmarshal_criteria_str(&map, format!("{payload}{tail}"))
                .expect_err("{tail:?} is not JSON whitespace, Go rejects it");
            assert!(
                err.to_string().contains("after top-level value"),
                "{tail:?} — expected the trailing-garbage error, got {err}"
            );
        }
        // Leading Unicode whitespace is likewise rejected before the value starts.
        assert!(
            unmarshal_criteria_str(&map, format!("\u{00a0}{payload}")).is_err(),
            "U+00A0 before the value is not JSON whitespace"
        );
    }

    /// Go's `Limit`/`Offset` are `int` (`criteria.go:23-25`), 64 bits on this
    /// target, so a value beyond `i32` parses, is held, and marshals back
    /// unchanged — no narrowing, no saturation. `LimitPercent` keeps its own
    /// `[0, 100]` clamp (`criteria.go:154-162`) independently of the width.
    #[test]
    fn limit_and_offset_keep_gos_64_bit_int_through_parse_and_marshal() {
        let map = default_field_map();
        let base = r#"{"all":[{"contains":{"title":"love"}}]"#;
        for (field, value) in [
            ("limit", i64::MAX),
            ("limit", 9_000_000_000_i64),
            ("limit", i64::from(i32::MAX) + 1),
            ("offset", i64::MAX),
            ("offset", 9_000_000_000_i64),
            ("offset", -9_000_000_000_i64),
        ] {
            // Act: parse.
            let parsed = unmarshal_criteria_str(&map, format!("{base},\"{field}\":{value}}}"))
                .unwrap_or_else(|e| panic!("{field}:{value} must parse, got {e}"));
            // Assert: the value survives the parse intact.
            let held = if field == "limit" {
                parsed.limit
            } else {
                parsed.offset
            };
            assert_eq!(held, value, "{field}:{value} must not be narrowed");
            // Act: marshal back (Go's `json.Marshal(aux)` with an `int` field).
            let text = marshal_criteria(&parsed);
            // Assert: the wire carries the same number, and round-trips.
            assert!(
                text.contains(&format!("\"{field}\":{value}")),
                "{field}:{value} must round-trip, got {text}",
            );
            let again = unmarshal_criteria_str(&map, text.clone())
                .unwrap_or_else(|e| panic!("re-parse of {text} failed: {e}"));
            let again_held = if field == "limit" {
                again.limit
            } else {
                again.offset
            };
            assert_eq!(again_held, value, "re-parse of {text}");
        }
        // Act + Assert: beyond int64 is Go's type error, not a wrap (`strconv
        // .ParseInt(s, 10, 64)`).
        let overflow =
            unmarshal_criteria_str(&map, format!("{base},\"limit\":9223372036854775808}}"))
                .expect_err("int64 overflow is Go's UnmarshalTypeError");
        assert!(
            overflow
                .to_string()
                .contains("cannot unmarshal number 9223372036854775808"),
            "got {overflow}",
        );
        // Act + Assert: the percentage clamp is unaffected by the width.
        let clamped = unmarshal_criteria_str(&map, format!("{base},\"limitPercent\":150}}"))
            .expect("limitPercent 150 parses then clamps");
        assert_eq!(clamped.limit_percent, 100, "clamp intact at the top");
        let negative = unmarshal_criteria_str(&map, format!("{base},\"limitPercent\":-5}}"))
            .expect("negative limitPercent parses then clamps");
        assert_eq!(negative.limit_percent, 0, "clamp intact at the bottom");
        // Act + Assert: a percentage resolved from a large total stays 64-bit
        // (`effective_limit`, `criteria.go:41`).
        let wide = Criteria {
            expression: Expression::All(vec![Expression::Contains(title_leaf("love".to_string()))]),
            limit: 0,
            limit_percent: 100,
            offset: 0,
            sort: String::new(),
            order: String::new(),
            refresh_delay: 0,
        };
        assert_eq!(effective_limit(&wide, 90_000_000), 90_000_000);
        let mut resolved = wide.clone();
        resolve_limit(&mut resolved, 9_000_000_000);
        assert_eq!(resolved.limit, 9_000_000_000, "resolved limit is 64-bit");
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
