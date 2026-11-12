//! Port of `persistence/sql_participations.go` (146L) — the participant JSON
//! column and the participant filter SQL.
//!
//! Ported here: the participants JSON column representation
//! (`marshalParticipants`/`unmarshalParticipants`, `:50-76`) plus the flattened
//! form the `updateParticipants` `json_each` INSERT reads (`flatParticipant`,
//! `:19-24`; `:91-105`), and the filter SQL (`ParticipantIDFilter`/
//! `NotParticipantIDFilter`, `:21-38`). The row-mutation half
//! (`updateParticipants`, `:78-125`; `getParticipants`, `:127-146`) is ported as
//! the pure-SQL [`update_participants_sql`] pair; the artist hydration inside
//! `getParticipants` belongs to the row-mapper slice (it needs the artist
//! repository).
//!
//! [`participant_id_filter`]/[`not_participant_id_filter`] model
//! `ParticipantIDFilter`/`NotParticipantIDFilter` (`:26-35,43`): a semi-join
//! `<table>_artists` subquery — `json_tree` over the JSON is far slower at
//! scale (`:27`) — negated with `NOT IN` for the negative filter.
//!
//! Squirrel renders `Eq{artist_id}` as `artist_id = ?` and the role
//! `Eq`-slice as `role IN (?, …)` (`:38-41`); an empty-role slice is
//! `roles: &[&str]` port keeps the same `IN`-rendering specialization for
//! arity as Go's always-nonempty branch — see the tests.
//!
//! Deliberate divergences, pinned by `tests/participants_json.rs` against the
//! Go oracle in `tests/testdata/go_participants/` (README there):
//!
//! * **Entry order within a role is not pinned.** Go builds the column from a
//!   `map[Role][]participant`, whose list order derives from `model.Participants`
//!   map iteration (`participants.go:174-185` iterates a map), so it is not
//!   reproducible across runs of the Go binary. Per the next-work.md rule the
//!   Rust port persists no ordering inherited from either side: role keys are
//!   string-sorted on both sides (Go sorts map keys as strings; the Rust
//!   `BTreeMap` agrees), entry order is input order, and the oracle compares
//!   entries as multisets, never positions.
//! * **Escaping: shape and round-trip fidelity, not escaped bytes.** Go's
//!   `json.Marshal` escapes `<`, `>`, `&`, U+2028 and U+2029; `serde_json`
//!   does not. The column is internal, self-consistent, and every reader parses
//!   it (the `nd-mapping` goldens are parsed, never byte-compared), so serde's
//!   raw output is the settled decision — the oracle parses both sides and
//!   never byte-compares the marshalled string.
//!
//! Safety rule (see `sqlrepo.rs`): the `<table>` identifier is
//! caller-supplied per the Go API contract (`:26` "matches rows of table");
//! `artist_id` is a bound `?` value.

use crate::sqlrepo::Value;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::string::ToString;
use std::vec;

/// The `<table>` + `_id`/`_artists` suffixes of `participantIDFilter`
/// (`persistence/sql_participations.go:33-36`).
/// The `_artists` join table suffix (`participantIDFilter`'s FROM table,
/// `sql_participations.go:34`).
pub const ARTISTS_TABLE_SUFFIX: &str = "_artists";
/// The `_id` suffix of `<table>_id` (`participantIDFilter`'s selected
/// column, `sql_participations.go:33`).
pub const ID_COLUMN_SUFFIX: &str = "_id";

/// The `<table>.id` column `ParticipantIDFilter` matches
/// (`sql_participations.go:35`).
#[must_use]
pub fn participant_id_column(table: &str) -> String {
    format!("{table}.id")
}

/// The `SELECT <table>_id FROM <table>_artists WHERE artist_id = ?` subquery
/// of `participantIDFilter` (`sql_participations.go:33-36`), plus its bound
/// args; add the role filter with [`with_participant_roles`] when roles are
/// given (`:29-31`).
pub fn participant_ids_subquery(table: &str, artist_id: &str) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT {table}{ID_COLUMN_SUFFIX} FROM {table}{ARTISTS_TABLE_SUFFIX} WHERE artist_id = ?"
        ),
        vec![Value::from(artist_id)],
    )
}

/// Appends `AND role IN (?, …)` to a [`participant_ids_subquery`] shape
/// (`sql_participations.go:29-31`, nonempty roles branch), returning the new
/// fragment and full arg list.
///
/// # Panics
/// On an empty `roles` slice — Go guards `len(roles) > 0` before reaching the
/// `Eq` (`:29`); reaching the empty case here is a caller bug, and silently
/// dropping the role filter would widen the match.
#[must_use]
pub fn with_participant_roles(
    table: &str,
    artist_id: &str,
    roles: &[&str],
) -> (String, Vec<Value>) {
    assert!(!roles.is_empty(), "roles must not be empty (see :29-31)");
    let placeholders = vec!["?"; roles.len()].join(", ");
    let mut args = vec![Value::from(artist_id)];
    args.extend(roles.iter().map(|role| Value::from(*role)));
    (
        format!(
            "SELECT {table}{ID_COLUMN_SUFFIX} FROM {table}{ARTISTS_TABLE_SUFFIX} \
             WHERE artist_id = ? AND role IN ({placeholders})"
        ),
        args,
    )
}

/// `ParticipantIDFilter` (`sql_participations.go:21-24`): the positive
/// semi-join — `… AND <table>.id IN (<subquery>)`.
#[must_use]
pub fn participant_id_filter(table: &str, artist_id: &str, roles: &[&str]) -> (String, Vec<Value>) {
    finish_participant_filter(table, artist_id, roles, false)
}

/// `NotParticipantIDFilter` (`sql_participations.go:26-28`): the negated
/// semi-join — `… AND <table>.id NOT IN (<subquery>)`.
#[must_use]
pub fn not_participant_id_filter(
    table: &str,
    artist_id: &str,
    roles: &[&str],
) -> (String, Vec<Value>) {
    finish_participant_filter(table, artist_id, roles, true)
}

/// `participantIDFilter` (`sql_participations.go:32-38`): renders the subquery
/// with its optional role filter, then wraps it in
/// `expr + op + sql + ")"` — the full Go shape `table+".id"+op+sql+")"`
/// (`:36`) with `op = " IN ("` / `" NOT IN ("`.
fn finish_participant_filter(
    table: &str,
    artist_id: &str,
    roles: &[&str],
    negate: bool,
) -> (String, Vec<Value>) {
    let (subquery, args) = if roles.is_empty() {
        participant_ids_subquery(table, artist_id)
    } else {
        with_participant_roles(table, artist_id, roles)
    };
    let op = if negate { " NOT IN (" } else { " IN (" };
    let id_column = participant_id_column(table);
    (format!("{id_column}{op}{subquery})"), args)
}

/// Go's `AllRoles` (`model/participants.go:32-47`): the role keys a column
/// can legally carry. `Role.UnmarshalText` (`:62-69`) rejects any other key
/// (`invalid role: %s`), so unmarshalling validates against this list.
pub const ALL_ROLES: [&str; 14] = [
    "artist",
    "albumartist",
    "composer",
    "conductor",
    "lyricist",
    "arranger",
    "producer",
    "director",
    "engineer",
    "mixer",
    "remixer",
    "djmixer",
    "performer",
    "maincredit",
];

/// A participant credit key, as stored in the `participants` column.
///
/// `model.Role` (`model/participants.go:50-60`) is a struct around a private
/// string with `MarshalText`/`UnmarshalText`; `UnmarshalText` rejects keys
/// outside [`ALL_ROLES`] (`invalid role: <key>`). This type ports that
/// validation exactly, so both sides of a round-trip agree on which keys are
/// legal. nd-model does not define `Role` yet (that is the #06 slice; see
/// `rust/crates/nd-model/src/album.rs:6`), so the column representation stays
/// local to `nd-db` — the same precedent as `UserRecord`/`Tag`.
///
/// Ordering: `BTreeMap` keys by `Ord on String`, i.e. sorted **as strings** —
/// byte-for-byte the order Go's `encoding/json` emits for map keys
/// (verified: `{"1":"a","10":"j","2":"b"}`). A future `Role`-keyed
/// `BTreeMap` must therefore stringify before sorting, or key order drifts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Role(String);

impl Role {
    /// `Role{role}.String()` (`model/participants.go:54-56`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl FromStr for Role {
    type Err = String;

    /// `RoleFromString` (`model/participants.go:71-76`): a key outside
    /// [`ALL_ROLES`] is the invalid role — `UnmarshalText`'s error text
    /// (`:65`), `"invalid role: <key>"`, carried as the `Err`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if ALL_ROLES.contains(&s) {
            Ok(Self(s.to_string()))
        } else {
            Err(format!("invalid role: {s}"))
        }
    }
}
impl From<&str> for Role {
    fn from(role: &str) -> Self {
        Self(role.to_string())
    }
}
impl From<String> for Role {
    fn from(role: String) -> Self {
        Self(role)
    }
}
impl Serialize for Role {
    /// `Role.MarshalText` (`model/participants.go:58-60`): the bare role
    /// string.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Role {
    /// `Role.UnmarshalText` (`model/participants.go:62-69`): the key must be
    /// in [`ALL_ROLES`] or the parse fails with `invalid role: <key>` — a
    /// `#[serde(transparent)]` derive would silently accept any string.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(D::Error::custom)
    }
}

/// The `participants` JSON column: Go's `model.Participants`
/// (`map[Role]ParticipantList`, `model/participants.go:94`) in the shape
/// `marshalParticipants` (`persistence/sql_participations.go:50-59`) stores —
/// role (string-sorted) → ordered `{id, name, subRole?}` entries.
///
/// **Within-role order is input order and deliberately not pinned** — see the
/// module docs for the deliberate divergence (Go's order is map-iteration
/// order, not reproducible; the next-work.md rule persists no ordering from
/// either side).
pub type Participants = BTreeMap<Role, Vec<Participant>>;

/// One participant entry as stored in the column — Go's unexported
/// `participant` struct (`persistence/sql_participations.go:13-17`):
/// `{id, name, subRole?}`. `model.Participant` (`model/participants.go:78-81`)
/// embeds `Artist`; the marshal keeps only ID/Name/SubRole (`:54`), so the
/// column representation does too — the `getParticipants` hydration
/// (`:127-146`) re-fetches the rest from the `artist` table at read time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// The artist id (`model.Artist.ID`), the `artist` table join key.
    #[serde(default)]
    pub id: String,
    /// The artist name (`model.Artist.Name`).
    #[serde(default)]
    pub name: String,
    /// The sub-role (`model.Participant.SubRole`, omitempty in Go). Empty
    /// omits the key, so `""` and absent round-trip to the same entry.
    #[serde(default, rename = "subRole", skip_serializing_if = "str::is_empty")]
    pub sub_role: String,
}

/// Go's `flatParticipant` (`persistence/sql_participations.go:19-24`): one row
/// of the flat JSON array the `updateParticipants` `json_each` INSERT extracts
/// (`$.artist_id`, `$.role`, `$.sub_role`, `:109-121`). Derived from a
/// `Participant` + role pair at marshal time (`:91-100`), not a stored
/// representation — the `participants` column stays the role-keyed shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatParticipant {
    /// `artist_id` — the `artist` table join key (`:112,118`).
    pub artist_id: String,
    /// `role` — `role.String()` (`:113,96`).
    pub role: String,
    /// `sub_role` — omitempty; the INSERT `COALESCE`s a missing key to `''`
    /// (`:114`), so `None` stores an empty sub-role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_role: Option<String>,
}

/// The JSON the `updateParticipants` INSERT binds as its `json_each` argument
/// (`persistence/sql_participations.go:91-105`): every (role, participant)
/// pair flattened, in map-iteration order.
///
/// **Ordering:** Go iterates the `Participants` map (`:92`), so row order is
/// unspecified and never relied on — the INSERT is a per-row `ON CONFLICT
/// DO NOTHING` (`:120`) with no ordinal. This port emits role-sorted order
/// (within a role, input order), which is deterministic but carries the same
/// no-ordering-guarantee contract: consumers must not index into the array.
#[must_use]
pub fn marshal_flat_participants(participants: &Participants) -> String {
    let flat: Vec<FlatParticipant> = participants
        .iter()
        .flat_map(|(role, artists)| {
            artists.iter().map(|p| FlatParticipant {
                artist_id: p.id.clone(),
                role: role.to_string(),
                sub_role: (!p.sub_role.is_empty()).then(|| p.sub_role.clone()),
            })
        })
        .collect();
    serde_json::to_string(&flat).unwrap_or_else(|_| "[]".to_string())
}

/// `marshalParticipants` (`persistence/sql_participations.go:50-59`): the
/// `participants` JSON column — role → `{id, name, subRole?}` entries. Role
/// keys sort as strings (Go's `json.Marshal` map-key order; the `BTreeMap`
/// agrees — see [`Role`]); within-role order is input order (Go's is
/// map-iteration order, deliberately not pinned — module docs). Go ignores
/// `json.Marshal`'s error (`:57`), which cannot occur for this shape, so the
/// `unwrap_or_else` follows `sql_tags::marshal_tags`.
#[must_use]
pub fn marshal_participants(participants: &Participants) -> String {
    serde_json::to_string(participants).unwrap_or_else(|_| "{}".to_string())
}

/// `unmarshalParticipants` (`persistence/sql_participations.go:61-76`): the
/// `participants` column → [`Participants`]. Two-value return, this crate's
/// spelling of Go's, carrying the contractual `parsing participants:` error
/// prefix and `""` for no error. Only invalid-role text is byte-identical;
/// serde and encoding/json use different structural-error wording, and callers
/// must not depend on text beyond the prefix.
///
/// Go semantics carried over:
/// - **`""` is an error** — `json.Unmarshal` of empty input fails
///   (`unexpected end of JSON input`), and Go's callers treat that as a real
///   error (`sql_base_repository.go`). Go marshals `nil` `model.Participants`
///   as `"null"` and re-reads it as an **empty, non-nil** map — the
///   nil-vs-empty distinction collapses through the column; Rust does the same
///   (`"null"` → empty map, `""` → error).
/// - **Unknown role keys are an error**: `Role.UnmarshalText` rejects them
///   (`invalid role: <key>`), so a hand-written or legacy column with a bad
///   key fails to parse — the same hard failure Go gets (verified against the
///   real `model.Role` in the Go oracle).
/// - **Missing/absent fields zero-fill** (`""`) as Go's struct fields do;
///   a `null` list under a role stays an empty list, exactly as Go's
///   `slice.Map` over a nil slice returns an empty slice.
/// - **Duplicate keys** keep the last value (Go's map assignment; serde
///   agrees).
/// - **Entry order within a role is preserved** from the column — the only
///   order this column has, and nothing downstream may depend on it.
#[must_use]
pub fn unmarshal_participants(data: &str) -> (Participants, String) {
    match serde_json::from_str::<Option<BTreeMap<Role, Option<Vec<Participant>>>>>(data) {
        Ok(raw) => {
            // Go reads into `map[model.Role][]participant` (`:62-63`), and its
            // `null` handling (pinned by the oracle's `nil_participants` case):
            // a `null` column is an empty (non-nil) map — Go marshals `nil
            // model.Participants` as `null` — and a role's `null` list is an
            // empty list (Go's `slice.Map` over a nil slice, `:70-72`). The
            // `Option` wrappers accept both; `unwrap_or_default` gives Go's
            // non-nil empties.
            let parsed: Participants = raw
                .unwrap_or_default()
                .into_iter()
                .map(|(role, artists)| (role, artists.unwrap_or_default()))
                .collect();
            (parsed, String::new())
        }
        Err(error) => {
            // serde_json appends ` at line N column M` to custom errors; Go's
            // `%w` wrap does not — strip the suffix so the two spell the same
            // message.
            let message = format!("{error}");
            let stripped = message
                .rsplit_once(" at line ")
                .map(|(prefix, _)| prefix.to_string())
                .unwrap_or(message);
            (
                Participants::new(),
                format!("parsing participants: {stripped}"),
            )
        }
    }
}

/// The DELETE half of `updateParticipants` (`persistence/sql_participations.go:82-86`),
/// always issued before any insert so a re-saved item cannot keep stale
/// role associations (`:79-81`). Mirrors `sql_tags::delete_tags_sql`.
#[must_use]
pub fn delete_participants_sql(table: String, item_id: String) -> (String, Vec<Value>) {
    (
        format!("DELETE FROM {table}{ARTISTS_TABLE_SUFFIX} WHERE {table}{ID_COLUMN_SUFFIX} = ?"),
        vec![Value::from(item_id)],
    )
}

/// The INSERT half of `updateParticipants` (`:102-124`): the `json_each`
/// `INSERT … SELECT` over the flat participant array, `JOIN`ed to `artist` so
/// unknown ids create no row, `COALESCE`d sub-role to `''`, and `ON CONFLICT
/// (artist_id, <table>_id, role, sub_role) DO NOTHING` for repeat puts.
/// `None` when the participants map has no role keys — Go returns before
/// building the query (`len(participants) == 0`, `:87-89`); a map that holds
/// only empty lists still gets the (empty-array) INSERT, as Go's would.
#[must_use]
pub fn insert_participants_sql(
    table: String,
    item_id: String,
    participants: &Participants,
) -> Option<(String, Vec<Value>)> {
    if participants.is_empty() {
        return None;
    }
    let flat_json = marshal_flat_participants(participants);
    let scoped = format!("{table}{ID_COLUMN_SUFFIX}");
    // Go renders the same statement with tab indentation and `as` aliases
    // (sql_participations.go:109-121); whitespace is cosmetic, but keep the
    // structure recognizable in the logs that print bound statements.
    let sql = format!(
        "INSERT INTO {table}{ARTISTS_TABLE_SUFFIX} ({scoped}, artist_id, role, sub_role) \n\t\t\tSELECT ?, \n\t\t\t       json_extract(value, '$.artist_id') as artist_id, \n\t\t\t       json_extract(value, '$.role') as role, \n\t\t\t       COALESCE(json_extract(value, '$.sub_role'), '') as sub_role \n\t\t\tFROM json_each(?) \n\t\t\tJOIN artist ON artist.id = json_extract(value, '$.artist_id') \n\t\t\tON CONFLICT (artist_id, {scoped}, role, sub_role) DO NOTHING",
    );
    Some((sql, vec![Value::from(item_id), Value::from(flat_json)]))
}

/// `updateParticipants` (`:78-125`) as its two statements in one Put: the
/// delete always runs, the insert only when the item has participants.
/// Mirrors `sql_tags::update_tags_sql`. The `getParticipants` read half
/// (`:127-146`) needs the artist repository and belongs to the row-mapper
/// slice; the JSON column itself is what this pair keeps in sync.
#[must_use]
pub fn update_participants_sql(
    table: String,
    item_id: String,
    participants: &Participants,
) -> UpdateParticipants {
    UpdateParticipants {
        delete: delete_participants_sql(table.clone(), item_id.clone()),
        insert: insert_participants_sql(table, item_id, participants),
    }
}

/// The delete-then-insert pair `updateParticipants` issues.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateParticipants {
    /// `Delete(<table>_artists).Where(Eq{<table>_id})` (`:82-83`).
    pub delete: (String, Vec<Value>),
    /// The scoped INSERT, absent when the item has no participants (`:87-89`).
    pub insert: Option<(String, Vec<Value>)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "album";
    const ARTIST: &str = "art-1";

    #[test]
    fn marshal_participants_emits_go_shape_and_string_sorted_roles() {
        // Arrange: roles whose insertion order differs from string order, with and
        // without subRole (sql_participations.go:50-59).
        let mut participants: Participants = BTreeMap::new();
        participants.insert(
            Role::from("composer"),
            vec![Participant {
                id: "c-1".into(),
                name: "C".into(),
                sub_role: "".into(),
            }],
        );
        participants.insert(
            Role::from("artist"),
            vec![Participant {
                id: "a-1".into(),
                name: "A".into(),
                sub_role: "vocals".into(),
            }],
        );
        // Act:
        let json = marshal_participants(&participants);
        // Assert: role keys string-sorted (artist < composer); subRole omitted when
        // empty, present when set — Go's `participant` struct, :13-17.
        assert_eq!(
            json,
            r#"{"artist":[{"id":"a-1","name":"A","subRole":"vocals"}],"composer":[{"id":"c-1","name":"C"}]}"#
        );
    }

    #[test]
    fn marshal_participants_empty_and_omitempty_round_trip() {
        // Arrange: the empty map and an empty subRole.
        let empty: Participants = BTreeMap::new();
        // Act / Assert: Go's `json.Marshal` of an empty map is `{}` (nil is `null`,
        // see the unmarshal test); `""` subRole never appears in the bytes.
        assert_eq!(marshal_participants(&empty), "{}");
        let mut one: Participants = BTreeMap::new();
        one.insert(
            Role::from("artist"),
            vec![Participant {
                id: "a".into(),
                name: "n".into(),
                sub_role: "".into(),
            }],
        );
        let json = marshal_participants(&one);
        assert!(
            !json.contains("subRole"),
            "empty subRole must be omitted: {json}"
        );
        // An absent and an empty subRole re-marshal identically (omitempty).
        let (back, err) = unmarshal_participants(&json);
        assert_eq!(err, "");
        assert_eq!(marshal_participants(&back), json);
    }

    #[test]
    fn unmarshal_participants_rejects_empty_and_invalid_role() {
        // Arrange / Act / Assert: Go's `json.Unmarshal` fails on empty input
        // (verified against real Go: `unexpected end of JSON input`), wrapped with
        // the `parsing participants:` prefix (:65).
        let (participants, err) = unmarshal_participants("");
        assert!(participants.is_empty());
        assert!(err.starts_with("parsing participants: "), "got {err:?}");
        // An unknown role key fails exactly as Go's Role.UnmarshalText (:62-69).
        let (participants, err) = unmarshal_participants(r#"{"bogus":[{"id":"a","name":"n"}]}"#);
        assert!(participants.is_empty());
        assert_eq!(err, "parsing participants: invalid role: bogus");
    }

    #[test]
    fn unmarshal_participants_duplicate_role_key_keeps_last_value() {
        // Go's encoding/json assigns object members in input order when
        // decoding into a map, so a duplicate role key keeps the final list.
        // serde_json's BTreeMap decoder has the same semantics.
        let (participants, err) = unmarshal_participants(
            r#"{"artist":[{"id":"first","name":"First"}],"artist":[{"id":"last","name":"Last"}]}"#,
        );
        assert_eq!(err, "");
        let artists = participants
            .get(&Role::from("artist"))
            .expect("artist role exists");
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].id, "last");
        assert_eq!(artists[0].name, "Last");
    }

    #[test]
    fn unmarshal_participants_null_is_empty_not_error() {
        // Arrange: Go marshals `nil model.Participants` as `null` and reads it back
        // as an empty, non-nil map (verified against real Go). The
        // nil-vs-empty distinction therefore collapses through the column — pinned
        // here and in the Go oracle (`nil_vs_empty`).
        let (participants, err) = unmarshal_participants("null");
        // Assert:
        assert_eq!(err, "");
        assert!(participants.is_empty());
        assert_eq!(marshal_participants(&participants), "{}");
    }

    #[test]
    fn unmarshal_participants_null_list_and_missing_fields_zero_fill() {
        // Arrange: a role whose list is `null` (Go: `slice.Map` over a nil slice —
        // `utils/slice/slice.go:13` — returns an empty, non-nil slice) and an entry
        // missing `id` (Go's struct fields zero-fill).
        let data = r#"{"artist":null,"composer":[{"name":"n"}]}"#;
        let (participants, err) = unmarshal_participants(data);
        // Assert:
        assert_eq!(err, "");
        assert_eq!(participants.get(&Role::from("artist")), Some(&vec![]));
        let composer = participants.get(&Role::from("composer")).expect("composer");
        assert_eq!(composer[0].id, "");
        assert_eq!(composer[0].name, "n");
    }

    #[test]
    fn role_order_matches_go_string_key_order() {
        // Arrange: role keys where byte order and display would differ; the column
        // must key-sort exactly as Go's `encoding/json` sorts map keys as strings.
        let mut participants: Participants = BTreeMap::new();
        for role in [
            "remixer",
            "performer",
            "mixer",
            "djmixer",
            "conductor",
            "artist",
        ] {
            participants.insert(
                Role::from(role),
                vec![Participant {
                    id: role.into(),
                    name: "n".into(),
                    sub_role: "".into(),
                }],
            );
        }
        let json = marshal_participants(&participants);
        // Assert: sorted-as-strings order (artist, conductor, djmixer, mixer,
        // performer, remixer) — insertion order was reversed on purpose.
        assert_eq!(
            json,
            r#"{"artist":[{"id":"artist","name":"n"}],"conductor":[{"id":"conductor","name":"n"}],"djmixer":[{"id":"djmixer","name":"n"}],"mixer":[{"id":"mixer","name":"n"}],"performer":[{"id":"performer","name":"n"}],"remixer":[{"id":"remixer","name":"n"}]}"#
        );
    }

    #[test]
    fn within_role_entry_order_is_input_order_and_deliberately_unpinned() {
        // Go's within-role order is map-iteration order — not reproducible across
        // runs of the Go binary (participants.go:174-185 iterates a map), so per
        // the next-work.md rule the port pins input order and asserts nothing
        // downstream may sort-rely. Documented here, and by the oracle's
        // multiset comparison, as the deliberate divergence.
        let mut participants: Participants = BTreeMap::new();
        participants.insert(
            Role::from("artist"),
            vec![
                Participant {
                    id: "b".into(),
                    name: "B".into(),
                    sub_role: "".into(),
                },
                Participant {
                    id: "a".into(),
                    name: "A".into(),
                    sub_role: "".into(),
                },
            ],
        );
        // Act: marshal → unmarshal keeps the column's entry order.
        let (back, err) = unmarshal_participants(&marshal_participants(&participants));
        // Assert: identity, no invented sorting.
        assert_eq!(err, "");
        assert_eq!(back, participants);
    }

    #[test]
    fn marshal_flat_participants_derives_the_json_each_shape() {
        // Arrange: the flat form the INSERT's `json_each` reads
        // (sql_participations.go:19-24, :91-105).
        let mut participants: Participants = BTreeMap::new();
        participants.insert(
            Role::from("composer"),
            vec![Participant {
                id: "c".into(),
                name: "C".into(),
                sub_role: "".into(),
            }],
        );
        participants.insert(
            Role::from("artist"),
            vec![Participant {
                id: "a".into(),
                name: "A".into(),
                sub_role: "vocals".into(),
            }],
        );
        // Act:
        let flat = marshal_flat_participants(&participants);
        // Assert: role-sorted rows, sub_role absent when empty (the INSERT
        // COALESCEs it to ''), present when set. Name never appears — the join
        // table has no name column (:110).
        assert_eq!(
            flat,
            r#"[{"artist_id":"a","role":"artist","sub_role":"vocals"},{"artist_id":"c","role":"composer"}]"#
        );
        // Round-trip stability: flattening again is identical.
        assert_eq!(flat, marshal_flat_participants(&participants));
    }

    #[test]
    fn insert_participants_sql_none_for_empty_map_and_rendered_for_entries() {
        // Arrange / Act / Assert: Go returns before building the query at
        // `len(participants) == 0` (:87-89) — map length, so a map with only
        // empty lists still renders (with an empty-array bind).
        let empty: Participants = BTreeMap::new();
        assert!(insert_participants_sql("album".into(), "alb-1".into(), &empty).is_none());
        let mut only_empty: Participants = BTreeMap::new();
        only_empty.insert(Role::from("artist"), vec![]);
        let Some((sql, args)) =
            insert_participants_sql("album".into(), "alb-1".into(), &only_empty)
        else {
            panic!("a map with role keys must render, as Go's len() > 0 arm would");
        };
        assert!(sql.starts_with("INSERT INTO album_artists (album_id, artist_id, role, sub_role)"));
        assert!(sql.contains("FROM json_each(?)"));
        assert!(sql.contains("ON CONFLICT (artist_id, album_id, role, sub_role) DO NOTHING"));
        assert_eq!(
            args,
            vec![Value::Text("alb-1".into()), Value::Text("[]".into())]
        );
    }

    #[test]
    fn update_participants_sql_pairs_delete_and_maybe_insert() {
        // Arrange / Act: the two statements Go issues in one Put (:78-125).
        let pair = update_participants_sql("media_file".into(), "mf-1".into(), &BTreeMap::new());
        // Assert: the delete always renders; the insert only with entries.
        assert_eq!(
            pair.delete.0,
            "DELETE FROM media_file_artists WHERE media_file_id = ?"
        );
        assert_eq!(pair.delete.1, vec![Value::Text("mf-1".into())]);
        assert!(pair.insert.is_none());
    }
    #[test]
    fn participant_id_filter_renders_positive_semi_join() {
        // Arrange: ParticipantIDFilter with no roles (sql_participations.go:21-24).
        // Act:
        let (sql, args) = participant_id_filter(TABLE, ARTIST, &[]);
        // Assert:
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_artists WHERE artist_id = ?)"
        );
        assert_eq!(args, vec![Value::Text(ARTIST.to_string())]);
    }

    #[test]
    fn participant_id_filter_with_roles_renders_role_in() {
        // Arrange: ParticipantIDFilter with roles (sql_participations.go:29-31).
        // Act:
        let (sql, args) = participant_id_filter(TABLE, ARTIST, &["albumartist", "composer"]);
        // Assert:
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_artists \
             WHERE artist_id = ? AND role IN (?, ?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(ARTIST.to_string()),
                Value::Text("albumartist".to_string()),
                Value::Text("composer".to_string()),
            ]
        );
    }

    #[test]
    fn not_participant_id_filter_renders_negated_semi_join() {
        // Arrange: NotParticipantIDFilter with roles (sql_participations.go:26-28).
        // Act:
        let (sql, args) = not_participant_id_filter("media_file", ARTIST, &["lyricist"]);
        // Assert:
        assert_eq!(
            sql,
            "media_file.id NOT IN (SELECT media_file_id FROM media_file_artists \
             WHERE artist_id = ? AND role IN (?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(ARTIST.to_string()),
                Value::Text("lyricist".to_string()),
            ]
        );
    }

    #[test]
    fn with_participant_roles_panics_on_empty_roles() {
        // Arrange: the guard Go applies before reaching the role Eq (:29).
        let result = std::panic::catch_unwind(|| with_participant_roles(TABLE, ARTIST, &[]));
        // Assert:
        assert!(result.is_err());
    }
}
