//! JSON column codecs for the remaining S-D hook columns.
//!
//! S-D1 prerequisite for the entity marshal seam (S-D2) and hydration
//! (S-D3): the raw JSON shapes the hook-bearing repositories marshal and
//! unmarshal for `album.discs`, `album.folder_ids`, and
//! `folder.image_files`, exactly as the Go repositories do:
//!
//! * `discs` — Go type `model.Discs` = `map[int]string`
//!   (`model/album.go:124`). `PostMapArgs`
//!   (`persistence/album_repository.go:90-93`) binds
//!   `json.Marshal(a.Album.Discs)`; `PostScan`
//!   (`:43-47`) unmarshals it back into `map[int]string` behind a
//!   `a.Discs != ""` guard. Go's `encoding/json` sorts integer map keys
//!   **as strings** — `{"1":"…","10":"…","2":"…"}`, not numeric order — so
//!   this codec stringifies keys before sorting (next-work.md §1b trap 2).
//! * `folder_ids` — Go `[]string`
//!   (`persistence/album_repository.go:59-64` reads, `:86-89` writes) via
//!   plain `json.Marshal`/`json.Unmarshal`.
//! * `image_files` — Go `[]string` with the special `PostMapArgs`
//!   (`persistence/folder_repository.go:48-58`): `ImageFiles == nil`
//!   stores the **literal `"[]"`**, never `json.Marshal(nil)` (which would
//!   be `null`); `PostScan` (`:40-44`) unmarshals into `[]string` behind
//!   a `!= ""` guard, where a JSON `null` decodes to a nil slice.
//!
//! Pinned against the real `encoding/json` by the Go oracle in
//! `tests/testdata/go_sql_json/` (README there): the golden is
//! byte-stable and the tests parse it, asserting object key order
//! positionally where Go's string-key sorting is the contract.
//!
//! **Escaping** follows the settled divergence (next-work.md §1b trap 3,
//! same decision as `sql_participations`): Go's `json.Marshal` escapes
//! `<`, `>`, `&`, U+2028, U+2029; `serde_json` does not. The columns are
//! internal, self-consistent, and every reader parses them, so the codec
//! emits serde's raw bytes — shape and round-trip fidelity, not
//! byte-identical escaping.
//!
//! **Error wrapping belongs to the entity hook, not here.** Go wraps with
//! `parsing album discs from db:` / `marshalling album folder_ids:` etc.
//! in the `PostScan`/`PostMapArgs` bodies; S-D3 adds those prefixes. This
//! module returns [`JsonColumnError`] carrying the raw serde source so
//! S-D3 can wrap with context useful at the hook site. Structural-error
//! wording deliberately differs from Go's (different encoders); callers
//! must not depend on text beyond what the hook adds.

use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// An error parsing or rendering one of the hook JSON columns.
///
/// `Raw` carries the raw `serde_json` source (malformed input or a
/// structural/type mismatch); S-D3's entity hook wraps it with the
/// Go-parity prefix (e.g. `parsing album discs from db`).
#[derive(Debug)]
pub enum JsonColumnError {
    /// The input is not valid JSON, or its shape/type does not match the
    /// column (non-integer discs key, non-string discs value, non-string
    /// list element, wrong top-level type). `msg` is the raw
    /// `serde_json` error text.
    Raw {
        /// Which column's codec hit the error — stable context for S-D3.
        column: &'static str,
        /// The raw serde_json error text.
        msg: String,
    },
}

impl fmt::Display for JsonColumnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw { column, msg } => write!(f, "{column}: {msg}"),
        }
    }
}

impl std::error::Error for JsonColumnError {}

impl JsonColumnError {
    fn raw(column: &'static str, err: serde_json::Error) -> Self {
        Self::Raw {
            column,
            msg: err.to_string(),
        }
    }
}

/// Ordered decode for Go's `map[int]string` column.
///
/// `serde_json::Value::Object` uses a key-sorted map in this build, which loses
/// source member order before numeric aliases are collapsed (`"1"` and `"01"`
/// both become integer key 1). Go's `encoding/json` processes object members in
/// source order, so the later member wins. A streaming `MapAccess` preserves
/// that contract while still storing the final result in a sorted Rust map.
struct DiscsColumn(Option<BTreeMap<i64, String>>);

impl<'de> Deserialize<'de> for DiscsColumn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DiscsVisitor;

        impl<'de> Visitor<'de> for DiscsVisitor {
            type Value = DiscsColumn;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a JSON object with integer keys and string/null values, or null")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(DiscsColumn(None))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(DiscsColumn(None))
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut discs = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Option<String>>()? {
                    let int_key = key.parse::<i64>().map_err(|_| {
                        M::Error::custom(format!("discs key must be an integer, got {key:?}"))
                    })?;
                    // Go decodes JSON null into a non-pointer string's zero
                    // value without error.
                    discs.insert(int_key, value.unwrap_or_default());
                }
                Ok(DiscsColumn(Some(discs)))
            }
        }

        deserializer.deserialize_any(DiscsVisitor)
    }
}

/// `json.Marshal(a.Album.Discs)` (`album_repository.go:90-93`): the
/// `discs` column. `None` (Go nil map) is `null`; an empty non-nil map is
/// `{}`. Keys are stringified **before** sorting, so the emitted order is
/// Go's string-key order (`1, 10, 2`), not numeric order.
#[must_use]
pub fn marshal_discs(discs: Option<&BTreeMap<i64, String>>) -> String {
    match discs {
        None => "null".to_string(),
        Some(map) => {
            // Build the object by inserting keys in string-sorted order:
            // serde_json's Map preserves insertion order, and sorting the
            // *stringified* keys reproduces Go's encoding/json order
            // exactly (a BTreeMap<i64, _> would emit 1, 2, 10).
            let mut object = serde_json::Map::new();
            let mut keys: Vec<&i64> = map.keys().collect();
            keys.sort_by_key(|k| k.to_string());
            for key in keys {
                object.insert(key.to_string(), Value::String(map[key].clone()));
            }
            Value::Object(object).to_string()
        }
    }
}

/// `json.Unmarshal(col, &a.Album.Discs)`
/// (`album_repository.go:43-47`): the `discs` column → `map[int]string`.
///
/// Preserves the nil-vs-empty distinction the API can represent:
/// `null` → `None`, `{}` → `Some(empty)`. A JSON `null` value becomes
/// `""` (Go's zero-value conversion into a non-pointer string field, no
/// error — pinned by the oracle's `read_discs_null_value` case). Hard
/// errors (no panics) on non-integer keys, any other non-string value, a
/// the empty string — Go's `PostScan` guard (`a.Discs != ""`) is what
/// keeps the empty string from reaching `json.Unmarshal`, and the
/// oracle pins that Go errors on it, so this codec does too (S-D3 keeps
/// the guard).
pub fn unmarshal_discs(data: &str) -> Result<Option<BTreeMap<i64, String>>, JsonColumnError> {
    serde_json::from_str::<DiscsColumn>(data)
        .map(|column| column.0)
        .map_err(|err| JsonColumnError::raw("discs", err))
}

/// `json.Marshal(a.Album.FolderIDs)` (`album_repository.go:86-89`): the
/// `folder_ids` column. `None` (Go nil slice) is `null`; an empty non-nil
/// slice is `[]`; element order is preserved.
#[must_use]
pub fn marshal_nullable_strings(ids: Option<&[String]>) -> String {
    match ids {
        None => "null".to_string(),
        Some(list) => {
            Value::Array(list.iter().map(|s| Value::String(s.clone())).collect()).to_string()
        }
    }
}

/// `json.Unmarshal(col, &ids)` (`album_repository.go:59-64`): the
/// `folder_ids` column → `[]string`.
///
/// Preserves nil-vs-empty: `null` → `None`, `[]` → `Some(empty)`.
/// Element order is kept. A JSON `null` element becomes `""` (Go's
/// zero-value conversion into a non-pointer string, no error); any other
/// non-string element, a wrong top-level type, and the empty string are
/// hard errors (Go's `!= ""` guard is what keeps the empty string away;
/// the oracle pins Go's error on it, so this codec does too).
pub fn unmarshal_nullable_strings(data: &str) -> Result<Option<Vec<String>>, JsonColumnError> {
    let value: Value =
        serde_json::from_str(data).map_err(|err| JsonColumnError::raw("folder_ids", err))?;
    let arr = match &value {
        Value::Array(arr) => Some(arr),
        Value::Null => None,
        _ => {
            return Err(JsonColumnError::raw(
                "folder_ids",
                serde_json::Error::custom("folder_ids column must be a JSON array or null"),
            ));
        }
    };
    match arr {
        None => Ok(None),
        Some(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                // Go converts a JSON `null` element into a non-pointer
                // string field as the zero value "" (no error) — pinned by
                // the oracle's `read_list_null_element` case.
                let s = v
                    .as_str()
                    .or_else(|| (v == &Value::Null).then_some(""))
                    .ok_or_else(|| {
                        JsonColumnError::raw(
                            "folder_ids",
                            serde_json::Error::custom(format!(
                                "folder_ids element {i} must be a string or null"
                            )),
                        )
                    })?;
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
    }
}

/// `f.Folder.ImageFiles == nil ? "[]" : json.Marshal(...)`
/// (`folder_repository.go:48-58`): the `image_files` column. **The write
/// side collapses nil to `[]`** — Go never stores `null` here, so this
/// codec never emits `null` for `None`. An empty non-nil slice also
/// yields `[]`; the two arms agree for empty.
///
/// Deliberate asymmetry with [`marshal_nullable_strings`]: that one
/// preserves Go's `json.Marshal(nil) = null` because album's `PostMapArgs`
/// has no special case; folder's does. Two distinct fns keep the contract
/// explicit rather than hiding it behind a flag.
#[must_use]
pub fn marshal_image_files(image_files: Option<&[String]>) -> String {
    match image_files {
        None => "[]".to_string(),
        Some(list) => {
            Value::Array(list.iter().map(|s| Value::String(s.clone())).collect()).to_string()
        }
    }
}

/// `json.Unmarshal(col, &f.Folder.ImageFiles)`
/// (`folder_repository.go:40-44`): the `image_files` column → `[]string`.
///
/// Go's `json.Unmarshal` of a JSON `null` into a `[]string` leaves the
/// slice **nil** with no error, and `[]` yields an empty non-nil slice —
/// so the read side preserves nil-vs-empty even though the write side
/// collapses it: `null` → `None`, `[]` → `Some(empty)`. (The column in
/// practice only ever holds `[]`-or-`[...]` because of the write side,
/// but the read side must accept `null` for parity). A JSON `null` element
/// becomes `""` (Go's zero-value conversion, no error); any other
/// non-string element, a wrong top-level type, and the empty string are
/// hard errors.
pub fn unmarshal_image_files(data: &str) -> Result<Option<Vec<String>>, JsonColumnError> {
    let value: Value =
        serde_json::from_str(data).map_err(|err| JsonColumnError::raw("image_files", err))?;
    let arr = match &value {
        Value::Array(arr) => Some(arr),
        Value::Null => None,
        _ => {
            return Err(JsonColumnError::raw(
                "image_files",
                serde_json::Error::custom("image_files column must be a JSON array or null"),
            ));
        }
    };
    match arr {
        None => Ok(None),
        Some(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                // Go converts a JSON `null` element into a non-pointer
                // string field as the zero value "" (no error) — pinned by
                // the oracle's `read_list_null_element` case.
                let s = v
                    .as_str()
                    .or_else(|| (v == &Value::Null).then_some(""))
                    .ok_or_else(|| {
                        JsonColumnError::raw(
                            "image_files",
                            serde_json::Error::custom(format!(
                                "image_files element {i} must be a string or null"
                            )),
                        )
                    })?;
                out.push(s.to_string());
            }
            Ok(Some(out))
        }
    }
}
