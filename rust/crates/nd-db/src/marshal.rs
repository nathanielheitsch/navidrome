//! The shared sorted column/value unit and the faithful `PostMapArgs`
//! transformations for `album`, `media_file`, and `folder` (slice S-D2a).
//!
//! This is the **core + hook** half of the S-D write seam
//! (`docs/rust-port/hydrate-contract.md` §§4-6): columns and args become
//! **one unit** that is emitted together, never two parallel lists a caller
//! can rotate. The pre-S-D2a defect class (next-work.md §Current-state,
//! commit `8c89aeb9`) was exactly the parallel-list shape: the three
//! caller-column `put` renderers built `columns: Vec<String>` and
//! `values: Vec<Value>` independently and could desync. Here a
//! [`MarshalledRow`] is the only value a repo adapter receives; it holds the
//! INSERT pair (sorted, `id` last) and the UPDATE pair (filtered, `id` last)
//! derived from **the same** final map, so they cannot drift.
//!
//! ## The invariants (source of truth: Go + vendored squirrel)
//!
//! * **Columns and args are one unit.** [`ColumnMap`] is a
//!   `BTreeMap<String, Value>` — there is no constructor accepting an
//!   independent `Vec<String>` / `Vec<Value>`. squirrel `SetMap`
//!   (`squirrel@v1.5.4/insert.go:271-283`) sorts column names and emits
//!   values positionally against that order; a `BTreeMap` gives exactly that.
// * **`id` is forbidden in the base/final map.** Go's write funnel
//   (`persistence/helpers.go:20-42`) runs `structs.Map(entity)`, which
//   **does include `id` and `created_at`** — the map carries the
//   caller's row values including the id. `put` then adds a *fresh* id to
//   the map only when the id is empty (`sql_base_repository.go:637-641`),
//   and `filterUpdateValues` re-adds it for the update leg (`:606-609`).
//   The Rust seam is the **architectural equivalent**: the base map
//   carries only non-id row columns, and the repo adapters append the id
//   **last to both** the column list and the args (the `8c89aeb9`
//   invariant). [`MarshalledRow::new`] returns [`MarshalError::IdInMap]
//   if `id` is present — that is the unit's one-way guarantee that the id
//   never reaches the positional VALUES twice.
//! * **UPDATE drops `created_at` and `birth_time`**, which INSERT keeps —
//!   Go's `selectUpdateColumns`
//!   (`sql_base_repository.go:587-604`) deletes those two keys after the
//!   optional `cols_to_update` filter. An empty UPDATE set is
//!   [`MarshalError::EmptyUpdateSet`], not invalid SQL.
//! * **Optional `cols_to_update`** (`sql_base_repository.go:595-601`)
//!   restricts the UPDATE set only; INSERT is unaffected.
//!
//! ## S-D2a boundary — caller-supplied base maps (deliberate, documented)
//!
//! Go's write funnel is `structs.Map(entity)` **then** `PostMapArgs`
//! (`persistence/helpers.go:20-42`). The `structs.Map` half — the entity's
//! own flat row columns — is **not yet a Rust record**; S-D2b replaces the
//! `BTreeMap<String, Value>` base map passed to [`album_post_map_args`],
//! [`mediafile_post_map_args`], and [`folder_post_map_args`] with complete
//! repo-local records (`AlbumRecord`/`MediaFileRecord`/`FolderRecord`).
//! This slice deliberately stops at the base-map boundary: it proves the
//! marshalling *transformations* (PostMapArgs) and the sorted-unit rendering
//! against a caller-supplied base map. **Caller-supplied columns are NOT yet
//! eliminated.**
//!
//! ## PostMapArgs fidelity
//!
//! * [`album_post_map_args`] is `dbAlbum.PostMapArgs`
//!   (`persistence/album_repository.go:71-97`): `full_text`
//!   (Name, SortAlbumName, AlbumArtist, participant names, disc values,
//!   `albumversion`/`catalognumber` tag values), `search_participants`
//!   (deterministic normalized result — settled divergence, see
//!   [`crate::sql_feeders`]), `search_normalized` (Name, AlbumArtist),
//!   `tags` ([`crate::sql_tags::marshal_tags`]), `participants`
//!   ([`crate::sql_participations::marshal_participants`] plus the flat JSON
//!   the junction-table INSERT consumes), `folder_ids`
//!   ([`crate::sql_json::marshal_nullable_strings`, nil→`null`]), `discs`
//!   ([`crate::sql_json::marshal_discs`, nil→`null`]).
//! * [`mediafile_post_map_args`] is `dbMediaFile.PostMapArgs`
//!   (`persistence/mediafile_repository.go:69-79`): the 9 `full_text`
//!   fields (config-resolved `FullTitle`, Album, Artist, AlbumArtist,
//!   SortTitle, SortAlbumName, SortArtistName, SortAlbumArtistName,
//!   DiscSubtitle) + participant names, the three search columns via
//!   [`crate::sql_feeders::mediafile_fts_columns`], `tags`/`participants`
//!   as album.
//! * [`folder_post_map_args`] is `dbFolder.PostMapArgs`
//!   (`persistence/folder_repository.go:48-59`): `image_files` through
//!   [`crate::sql_json::marshal_image_files`] (nil→`"[]"` special arm).
//!
//! In every case derived keys **overwrite** same-named base keys, exactly
//! like Go's `args[key] = value` map assignment (the base map is consumed by
//! value and the returned [`ColumnMap`] is owned — the caller's map is never
//! mutated).
//!
//! ## Oracles (reused, not re-authored)
//!
//! The transformations compose landed, independently Go-pinned primitives —
//! no new oracle is created here:
//! * [`crate::sql_participations`] (+ `tests/participants_json.rs` + `go_participants`)
//!   for `marshal_participants` / `marshal_flat_participants`;
//! * [`crate::sql_feeders`] (+ `tests/search_feeders.rs` + `go_feeders`) for the
//!   three FTS columns;
//! * [`crate::sql_json`] (+ `tests/sql_json.rs` + `go_sql_json`) for `discs` /
//!   `folder_ids` / `image_files`;
//! * [`crate::sql_tags`] (its tests pin the Go `marshalTags` shape) for `tags`.
//!
//! Shared ordering/filtering invariants are unit-tested in this module;
//! actual write→read-back data flow is pinned in `tests/runtime_marshal.rs`.
//!
//! **Renderer strings are not the assertion.** The executed tests read the
//! stored row and junction tables back and fail on a field swap or order
//! rotation.

use std::collections::BTreeMap;

use crate::sql_json;
use crate::sql_participations::{marshal_participants, Participants};
use crate::sql_tags::{indexed_tag_ids, marshal_tags, DbTags};
use crate::sqlrepo::Value;

/// The base column map: one owned `BTreeMap` of column → value.
///
/// A `BTreeMap` is the unit itself — iterating it yields `(column, value)`
/// pairs in sorted column order, exactly the order squirrel `SetMap`
/// emits. There is no type here that holds columns and args separately.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnMap(BTreeMap<String, Value>);

impl ColumnMap {
    /// Wrap an owned sorted map. Callers build it from entity data (S-D2b)
    /// or, in this slice, from caller-supplied base columns (the S-D2a
    /// boundary — see module docs).
    #[must_use]
    pub fn new(map: BTreeMap<String, Value>) -> Self {
        Self(map)
    }

    /// Iterate `(column, value)` pairs in sorted column order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(c, v)| (c.as_str(), v))
    }

    /// The underlying sorted map (borrowed) — adapters re-derive legs from it.
    #[must_use]
    pub fn map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }

    /// Number of columns in the map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `true` when `column` is present (used for the `id` guard).
    #[must_use]
    pub fn contains(&self, column: &str) -> bool {
        self.0.contains_key(column)
    }

    /// Overwrite one column's value (PostMapArgs' `args[key] = value`).
    /// Consuming the base map by value and overwriting here keeps the caller's
    /// original untouched.
    fn insert(&mut self, column: impl Into<String>, value: Value) {
        self.0.insert(column.into(), value);
    }

    /// The sorted column names.
    fn columns(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    /// The positional values in sorted column order.
    fn values(&self) -> Vec<Value> {
        self.0.values().cloned().collect()
    }
}

/// Failures that are a caller contract violation, not a SQL error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MarshalError {
    /// `id` was present in the base/final map. The Go map *does* carry the
    /// id (`structs.Map` includes it), but the Rust seam takes it as a
    /// separate argument the repo adapter appends last
    /// (`sql_base_repository.go:637-641`); it must not also be in the map.
    #[error("id is forbidden in the marshalled map: the repo adapter appends it last")]
    IdInMap,

    /// The UPDATE column set is empty after dropping `created_at` /
    /// `birth_time` (and applying `cols_to_update`). Go's `filterUpdateValues`
    /// (`sql_base_repository.go:606-609`) would still re-add `id`, so Go renders
    /// `UPDATE … SET id = ? WHERE id = ?` — a no-op SET that runs; this seam
    /// deliberately omits that no-op `id` from SET (id binds only in WHERE), so
    /// nothing would be left to set. Error instead of emitting degenerate SQL.
    /// The typed error is the port's contract, not a Go error (Go has none).
    #[error("no columns left to update after dropping created_at/birth_time")]
    EmptyUpdateSet,
}

/// The two rendered legs of a `put`, one unit: the INSERT pair (sorted,
/// `id` last) and the UPDATE pair (filtered, `id` last as the `WHERE` arg),
/// plus the `Sql` strings.
///
/// Go's `put` (`sql_base_repository.go:618-642`) is *not* both-legs:
/// when `id != ""` it tries the UPDATE **first** and returns if the match
/// count is `> 0`; the INSERT runs only on an update miss (no row) or a new
/// (empty) id. The Go UPDATE's `SetMap` includes `id = id` because
/// `filterUpdateValues` re-adds the id to the values map (`:606-609`) — a
/// no-op SET; this module deliberately omits it and binds the id only in
/// `WHERE id = ?`. This module renders **both alternatives** and does no
/// execution; the caller chooses the leg exactly as Go does.
#[derive(Debug, Clone, PartialEq)]
pub struct MarshalledRow {
    /// `INSERT INTO <table> (<cols>, id) VALUES (?, …, <id>)`.
    pub insert_sql: String,
    /// The positional VALUES for [`Self::insert_sql`], `id` last.
    pub insert_args: Vec<Value>,
    /// `UPDATE <table> SET <filtered cols> = ?, … WHERE id = ?`.
    pub update_sql: String,
    /// The positional SET values for [`Self::update_sql`] + the trailing
    /// `WHERE id = ?` arg.
    pub update_args: Vec<Value>,
    /// The sorted INSERT column names (without `id`) — exposed so adapters
    /// and tests can assert the unit without re-parsing the SQL.
    pub insert_columns: Vec<String>,
    /// The filtered UPDATE column names (without `id`).
    pub update_columns: Vec<String>,
}

/// The columns `selectUpdateColumns` always drops from the UPDATE set
/// (`sql_base_repository.go:598-603`).
const UPDATE_DROP: [&str; 2] = ["created_at", "birth_time"];

/// `persistence/helpers.go:48-52` (`toSnakeCase`): the two-regex camel→snake
/// conversion Go applies to every `colsToUpdate` entry (and table names)
/// before matching against snake_case column keys.
///
/// Go's `toSnakeCase` runs **two independent** `ReplaceAllString` passes —
/// `matchFirstCap` = `(.)([A-Z][a-z]+)`, then `matchAllCap` = `([a-z0-9])([A-Z])`
/// — and lowercases the result. `ReplaceAllString` is leftmost, longest,
/// non-overlapping, scanning left to right: a match consumes its span and
/// scanning resumes after it. This mirrors that with two passes: first the
/// `(.)([A-Z][a-z]+)` pass (a context char + a capital starting a run of
/// ≥2 capitals whose second is lowercase), then the `([a-z0-9])([A-Z])` pass.
/// The passes must not be fused into one: for `RGAlbumGain` the first pass
/// matches `G|Album` leaving `Gain`, and the second pass then splits `m|G`
/// and `A|G` — yielding `rg_album_gain`. (Fused single-pass variants tend
/// to swallow the second boundary and produce `rg_albumgain`.)
///
/// Pinned against Go (`go run` on the real helper) in the unit tests below,
/// including the tricky acronym forms: `RGAlbumGain` → `rg_album_gain`,
/// `ID3` → `id3`, `XID` → `xid`.
#[must_use]
pub fn to_snake_case(s: &str) -> String {
    snake_pass_second(&snake_pass_first(s)).to_lowercase()
}

/// First pass: `matchFirstCap = (.)([A-Z][a-z]+)` → `"$1_$2"`, leftmost,
/// non-overlapping, longest (the lowercase run is consumed fully).
fn snake_pass_first(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let n = b.len();
    let mut out = String::with_capacity(n + n / 4);
    let mut i = 0usize;
    while i < n {
        if i + 2 < n && b[i + 1].is_ascii_uppercase() && b[i + 2].is_ascii_lowercase() {
            out.push(b[i]);
            out.push('_');
            out.push(b[i + 1]);
            let mut j = i + 2;
            while j < n && b[j].is_ascii_lowercase() {
                out.push(b[j]);
                j += 1;
            }
            i = j;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Second pass: `matchAllCap = ([a-z0-9])([A-Z])` → `"$1_$2"`, leftmost,
/// non-overlapping.
fn snake_pass_second(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let n = b.len();
    let mut out = String::with_capacity(n + n / 4);
    let mut i = 0usize;
    while i < n {
        if i + 1 < n
            && (b[i].is_ascii_lowercase() || b[i].is_ascii_digit())
            && b[i + 1].is_ascii_uppercase()
        {
            out.push(b[i]);
            out.push('_');
            out.push(b[i + 1]);
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

impl MarshalledRow {
    /// Build the two legs from the final, PostMapArgs-applied map.
    ///
    /// * `table` — the target table (caller-supplied schema constant).
    /// * `id` — appended **last** to both the INSERT column list and the
    ///   VALUES tuple, and bound as the trailing `WHERE id = ?` arg on the
    ///   UPDATE. `id` must not already be in `map`.
    /// * `cols_to_update` — optional `colsToUpdate` filter (empty = all
    ///   columns). Applies to the UPDATE only.
    ///
    /// INSERT = full sorted pairs + `id` last. UPDATE = pairs restricted to
    /// `cols_to_update` (if non-empty), minus `created_at`/`birth_time`, +
    /// `id` last in args. The UPDATE errors on an empty set.
    pub fn new(
        table: &str,
        id: &str,
        map: &ColumnMap,
        cols_to_update: &[&str],
    ) -> Result<Self, MarshalError> {
        if map.contains("id") {
            return Err(MarshalError::IdInMap);
        }

        // INSERT: every column, sorted, with id appended to both lists.
        let mut insert_columns = map.columns();
        let mut insert_args = map.values();
        insert_columns.push("id".to_string());
        insert_args.push(Value::Text(id.to_string()));
        let insert_sql = format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            insert_columns.join(", "),
            vec!["?"; insert_args.len()].join(", ")
        );

        // UPDATE: selectUpdateColumns — optional colsToUpdate filter (Go
        // toSnakeCase-normalizes each entry first, helpers.go:48-52), then
        // drop created_at/birth_time (sql_base_repository.go:587-604). Go's
        // filterUpdateValues re-adds id to the SET map (:606-609); we omit
        // that no-op and bind id only in WHERE.
        let restrict = !cols_to_update.is_empty();
        let normalized: Vec<String> = cols_to_update.iter().map(|c| to_snake_case(c)).collect();
        let update_columns: Vec<String> = map
            .columns()
            .into_iter()
            .filter(|c| {
                let c = c.as_str();
                let in_filter = !restrict || normalized.contains(&c.to_string());
                in_filter && !UPDATE_DROP.contains(&c)
            })
            .collect();
        if update_columns.is_empty() {
            return Err(MarshalError::EmptyUpdateSet);
        }
        let update_sql = format!(
            "UPDATE {table} SET {} WHERE id = ?",
            update_columns
                .iter()
                .map(|c| format!("{c} = ?"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut update_args: Vec<Value> = Vec::with_capacity(update_columns.len() + 1);
        for c in &update_columns {
            if let Some((_, v)) = map.iter().find(|(mc, _)| *mc == c.as_str()) {
                update_args.push(v.clone());
            }
        }
        update_args.push(Value::Text(id.to_string()));

        Ok(Self {
            insert_sql,
            insert_args,
            update_sql,
            update_args,
            insert_columns,
            update_columns,
        })
    }
}

/// The derived, PostMapArgs-computed inputs for [`album_post_map_args`].
/// Everything that is *not* the entity's own row columns arrives here already
/// computed by the landed primitives; the base map holds the rest.
///
/// **S-D2a consistency boundary.** The FTS inputs `participant_names`,
/// `disc_values`, `album_version`, and `catalog_number` are accepted
/// *separately* from the authoritative [`Participants`]/`discs`/[`DbTags`]
/// values, so a caller could (in S-D2a's record-free world) pass values that
/// contradict the JSON/junction values built from the latter. The invariant
/// S-D2b must preserve atomically at record construction:
/// * `participant_names` = `participants.all_names()` (Go
///   `a.Album.Participants.AllNames()`, `album_repository.go:73-74`);
/// * `disc_values` = the values of `discs` in Go's `maps.Values` iteration
///   (`album_repository.go:75` — non-deterministic key order in Go;
///   [`crate::sql_feeders::format_full_text`] sorts/dedupes the words,
///   so any iteration order of the same value set yields the same column);
/// * `album_version` = `tags["albumversion"]` (Go `Tags[TagAlbumVersion]`,
///   `model/tag.go:197`), `catalog_number` = `tags["catalognumber"]`
///   (`model/tag.go:206`) — empty when the tag is absent.
///
/// This slice keeps the inputs explicit (the prompt lists them as derived
/// inputs) rather than deriving them; a consistent construction is pinned
/// in the unit tests.
pub struct AlbumDerived {
    /// `Name` (`a.Name`) — full_text input + search_normalized input.
    pub name: String,
    /// `SortAlbumName` — full_text input.
    pub sort_album_name: String,
    /// `AlbumArtist` — full_text input + search_normalized input.
    pub album_artist: String,
    /// `Participants.AllNames()` — full_text input + search_participants.
    pub participant_names: Vec<String>,
    /// `maps.Values(a.Album.Discs)` — full_text input.
    pub disc_values: Vec<String>,
    /// `Tags[TagAlbumVersion]` values — full_text input (empty when absent).
    pub album_version: Vec<String>,
    /// `Tags[TagCatalogNumber]` values — full_text input (empty when absent).
    pub catalog_number: Vec<String>,
    /// `a.Album.Tags` — marshalled into `tags`, indexed into `album_tags`.
    pub tags: DbTags,
    /// `a.Album.Participants` — marshalled into `participants` and the
    /// junction-table INSERT.
    pub participants: Participants,
    /// `a.Album.FolderIDs` — `None` (Go nil slice) stores `null`.
    pub folder_ids: Option<Vec<String>>,
    /// `a.Album.Discs` — `None` (Go nil map) stores `null`.
    pub discs: Option<std::collections::BTreeMap<i64, String>>,
}

/// `dbAlbum.PostMapArgs` (`album_repository.go:71-97`) applied to `base`.
///
/// Returns a `ColumnMap` holding the base row columns **with** the derived
/// keys overwritten in place (`full_text`, `search_participants`,
/// `search_normalized`, `tags`, `participants`, `folder_ids`, `discs`),
/// plus the authoritative participants and indexed tag ids the repo adapter
/// uses for junction-table maintenance. The adapter deliberately delegates
/// participant JSON generation to the oracle-pinned maintenance helper.
///
/// The base map is consumed by value and the returned map is owned; `base`
/// is not mutated.
pub fn album_post_map_args(base: ColumnMap, d: AlbumDerived) -> (ColumnMap, AlbumMaintenance) {
    let mut map = base;

    // full_text: Name, SortAlbumName, AlbumArtist, participant names, disc
    // values, albumversion, catalognumber (album_repository.go:72-78).
    let mut full_text_inputs: Vec<&str> = vec![
        d.name.as_str(),
        d.sort_album_name.as_str(),
        d.album_artist.as_str(),
    ];
    full_text_inputs.extend(d.participant_names.iter().map(String::as_str));
    full_text_inputs.extend(d.disc_values.iter().map(String::as_str));
    full_text_inputs.extend(d.album_version.iter().map(String::as_str));
    full_text_inputs.extend(d.catalog_number.iter().map(String::as_str));
    map.insert(
        "full_text",
        Value::Text(crate::sql_feeders::format_full_text(&full_text_inputs)),
    );
    // search_participants: deterministic normalized join (settled divergence).
    map.insert(
        "search_participants",
        Value::Text(crate::sql_feeders::search_participants(
            &d.participant_names,
        )),
    );
    // search_normalized: NormalizeForFTS(Name, AlbumArtist).
    map.insert(
        "search_normalized",
        Value::Text(crate::sql_feeders::search_normalized(&[
            &d.name,
            &d.album_artist,
        ])),
    );
    map.insert("tags", Value::Text(marshal_tags(d.tags.clone())));
    map.insert(
        "participants",
        Value::Text(marshal_participants(&d.participants)),
    );
    map.insert(
        "folder_ids",
        Value::Text(sql_json::marshal_nullable_strings(d.folder_ids.as_deref())),
    );
    map.insert(
        "discs",
        Value::Text(sql_json::marshal_discs(d.discs.as_ref())),
    );

    (
        map,
        AlbumMaintenance {
            participants: d.participants,
            indexed_tag_ids: indexed_tag_ids(d.tags),
        },
    )
}

/// Inputs retained for the album junction-table maintenance.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumMaintenance {
    /// The participants the `album_artists` maintenance is built from (Go's
    /// `a.Album.Participants`). The `participants` JSON column and junction
    /// statements derive from this value. During S-D2a, FTS names remain the
    /// separately supplied `AlbumDerived::participant_names`; S-D2b must
    /// enforce the documented consistency invariant when constructing both.
    pub participants: Participants,
    /// `album_tags` INSERT ids (`indexedTagNames` over genre).
    pub indexed_tag_ids: Vec<String>,
}

/// The derived inputs for [`mediafile_post_map_args`]. The `full_title` is
/// the config-resolved `FullTitle()` (Go appends the subtitle tag value when
/// `Server.Subsonic.AppendSubtitle` is on); callers resolve it via
/// [`crate::sql_feeders::full_title`] and pass the result.
///
/// **S-D2a consistency boundary.** `participant_names` must be the names
/// derived from `participants`, exactly as Go calls `Participants.AllNames()`.
/// S-D2b must construct both atomically from its complete media-file record.
pub struct MediaFileDerived {
    /// `m.FullTitle()` — config-resolved (subtitle appended when configured).
    pub full_title: String,
    /// `m.Album` — full_text input + search_normalized input.
    pub album: String,
    /// `m.Artist` — full_text input + search_normalized input.
    pub artist: String,
    /// `m.AlbumArtist` — full_text input + search_normalized input.
    pub album_artist: String,
    /// `m.SortTitle` — full_text input.
    pub sort_title: String,
    /// `m.SortAlbumName` — full_text input.
    pub sort_album_name: String,
    /// `m.SortArtistName` — full_text input.
    pub sort_artist_name: String,
    /// `m.SortAlbumArtistName` — full_text input.
    pub sort_album_artist_name: String,
    /// `m.DiscSubtitle` — full_text input.
    pub disc_subtitle: String,
    /// `Participants.AllNames()` — full_text input + search_participants.
    pub participant_names: Vec<String>,
    /// `m.MediaFile.Tags`.
    pub tags: DbTags,
    /// `m.MediaFile.Participants`.
    pub participants: Participants,
}

/// `dbMediaFile.PostMapArgs` (`mediafile_repository.go:69-79`) applied to
/// `base`. Same overwrite-into-base contract as [`album_post_map_args`]; the
/// three search columns are computed via
/// [`crate::sql_feeders::mediafile_fts_columns`].
pub fn mediafile_post_map_args(
    base: ColumnMap,
    d: MediaFileDerived,
) -> (ColumnMap, MediaFileMaintenance) {
    let mut map = base;

    let fts = crate::sql_feeders::mediafile_fts_columns(
        &d.full_title,
        &d.album,
        &d.artist,
        &d.album_artist,
        &d.sort_title,
        &d.sort_album_name,
        &d.sort_artist_name,
        &d.sort_album_artist_name,
        &d.disc_subtitle,
        &d.participant_names,
    );
    map.insert("full_text", Value::Text(fts.full_text));
    map.insert("search_participants", Value::Text(fts.search_participants));
    map.insert("search_normalized", Value::Text(fts.search_normalized));
    map.insert("tags", Value::Text(marshal_tags(d.tags.clone())));
    map.insert(
        "participants",
        Value::Text(marshal_participants(&d.participants)),
    );

    (
        map,
        MediaFileMaintenance {
            participants: d.participants,
            indexed_tag_ids: indexed_tag_ids(d.tags),
        },
    )
}

/// The media_file junction-table maintenance (see [`AlbumMaintenance`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFileMaintenance {
    /// The participants the `media_file_artists` maintenance is built from
    /// (see [`AlbumMaintenance::participants`]).
    pub participants: Participants,
    /// `media_file_tags` INSERT ids (`indexedTagNames` over genre).
    pub indexed_tag_ids: Vec<String>,
}

/// The derived input for [`folder_post_map_args`].
pub struct FolderDerived {
    /// `f.Folder.ImageFiles` — `None` (Go nil) stores the literal `"[]"`.
    pub image_files: Option<Vec<String>>,
}

/// `dbFolder.PostMapArgs` (`folder_repository.go:48-59`) applied to `base`:
/// overwrites `image_files` with the marshalled column (nil→`"[]"`).
pub fn folder_post_map_args(base: ColumnMap, d: FolderDerived) -> ColumnMap {
    let mut map = base;
    map.insert(
        "image_files",
        Value::Text(sql_json::marshal_image_files(d.image_files.as_deref())),
    );
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_participations::{Participant, Role};
    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    /// Scrambled base + derived keys must emerge sorted, with positional
    /// values aligned to their column. A value rotation would break this.
    #[test]
    fn insert_columns_are_sorted_and_values_positional() {
        let mut base = BTreeMap::new();
        // Deliberately inserted in non-alphabetical order.
        base.insert("zzeta".to_string(), text("z-val"));
        base.insert("alpha".to_string(), text("a-val"));
        base.insert("mid".to_string(), Value::Integer(7));
        base.insert("created_at".to_string(), text("2024-01-01 00:00:00+00:00"));

        let row = MarshalledRow::new("album", "id-1", &ColumnMap::new(base), &[]).unwrap();

        // id is appended last, not sorted in (the 8c89aeb9 invariant).
        assert_eq!(
            row.insert_columns,
            vec!["alpha", "created_at", "mid", "zzeta", "id"]
        );
        assert_eq!(
            row.insert_args,
            vec![
                text("a-val"),
                text("2024-01-01 00:00:00+00:00"),
                Value::Integer(7),
                text("z-val"),
                text("id-1")
            ]
        );
        // The SQL lists columns in the same order the args bind, so a value
        // rotation is visible in the stored row (pinned in runtime_marshal.rs).
        assert!(row
            .insert_sql
            .contains("(alpha, created_at, mid, zzeta, id)"));
    }

    #[test]
    fn id_in_map_is_rejected() {
        let mut base = BTreeMap::new();
        base.insert("id".to_string(), text("x"));
        base.insert("name".to_string(), text("n"));
        let err = MarshalledRow::new("album", "id-1", &ColumnMap::new(base), &[])
            .err()
            .unwrap();
        assert_eq!(err, MarshalError::IdInMap);
    }

    #[test]
    fn created_at_and_birth_time_are_in_insert_not_update() {
        let mut base = BTreeMap::new();
        base.insert("created_at".to_string(), text("t0"));
        base.insert("birth_time".to_string(), text("t0"));
        base.insert("name".to_string(), text("n"));
        let row = MarshalledRow::new("media_file", "mf-1", &ColumnMap::new(base), &[]).unwrap();

        // INSERT keeps both.
        assert!(row.insert_columns.contains(&"created_at".to_string()));
        assert!(row.insert_columns.contains(&"birth_time".to_string()));
        // UPDATE drops both.
        assert!(!row.update_columns.contains(&"created_at".to_string()));
        assert!(!row.update_columns.contains(&"birth_time".to_string()));
        assert_eq!(row.update_columns, vec!["name".to_string()]);
        // SET name = ? then WHERE id = ? — name's value then the id.
        assert_eq!(row.update_args, vec![text("n"), text("mf-1")]);
        assert_eq!(
            row.update_sql,
            "UPDATE media_file SET name = ? WHERE id = ?"
        );
    }

    #[test]
    fn empty_update_set_errors_not_invalid_sql() {
        // Only the two dropped columns: nothing left to update.
        let mut base = BTreeMap::new();
        base.insert("created_at".to_string(), text("t0"));
        base.insert("birth_time".to_string(), text("t0"));
        let err = MarshalledRow::new("media_file", "mf-1", &ColumnMap::new(base), &[])
            .err()
            .unwrap();
        assert_eq!(err, MarshalError::EmptyUpdateSet);
    }

    #[test]
    fn cols_to_update_restricts_update_only() {
        let mut base = BTreeMap::new();
        base.insert("name".to_string(), text("n"));
        base.insert("album_artist".to_string(), text("aa"));
        base.insert("created_at".to_string(), text("t0"));

        // Restrict the update to just album_artist.
        let row =
            MarshalledRow::new("album", "a-1", &ColumnMap::new(base), &["album_artist"]).unwrap();
        assert_eq!(row.update_columns, vec!["album_artist".to_string()]);
        assert_eq!(row.update_args, vec![text("aa"), text("a-1")]);

        // INSERT is unaffected by cols_to_update: all columns present.
        assert_eq!(
            row.insert_columns,
            vec!["album_artist", "created_at", "name", "id"]
        );
    }

    #[test]
    fn cols_to_update_that_leaves_only_dropped_columns_errors() {
        let mut base = BTreeMap::new();
        base.insert("name".to_string(), text("n"));
        base.insert("created_at".to_string(), text("t0"));
        // Restrict to created_at only → dropped → empty update set.
        let err = MarshalledRow::new("album", "a-1", &ColumnMap::new(base), &["created_at"])
            .err()
            .unwrap();
        assert_eq!(err, MarshalError::EmptyUpdateSet);
    }

    #[test]
    fn derived_keys_overwrite_same_named_base_keys_like_go_map_assignment() {
        // Go: args["tags"] = marshalTags(...) overwrites whatever
        // structs.Map put there. The base map already has `tags`/`discs`/
        // `participants` (the raw model fields would map to those keys);
        // PostMapArgs replaces them.
        let mut base = BTreeMap::new();
        base.insert("tags".to_string(), text("SHOULD-BE-OVERRIDDEN"));
        base.insert("discs".to_string(), text("SHOULD-BE-OVERRIDDEN"));
        base.insert("folder_ids".to_string(), text("SHOULD-BE-OVERRIDDEN"));
        base.insert("full_text".to_string(), text("SHOULD-BE-OVERRIDDEN"));

        let d = AlbumDerived {
            name: "N".into(),
            sort_album_name: "S".into(),
            album_artist: "AA".into(),
            participant_names: vec!["P1".into()],
            disc_values: vec!["dv".into()],
            album_version: vec![],
            catalog_number: vec![],
            tags: DbTags::new(),
            participants: Participants::new(),
            folder_ids: Some(vec!["f1".into()]),
            discs: Some({
                let mut m = std::collections::BTreeMap::new();
                m.insert(1, "one".into());
                m
            }),
        };
        let (map, maint) = album_post_map_args(ColumnMap::new(base), d);

        // Every derived key is the marshalled value, not the base's old text.
        let tags = map.iter().find(|(c, _)| *c == "tags").unwrap().1;
        assert_eq!(tags, &Value::Text("{}".to_string()));
        let discs = map.iter().find(|(c, _)| *c == "discs").unwrap().1;
        assert_eq!(discs, &Value::Text(r#"{"1":"one"}"#.to_string()));
        let folder_ids = map.iter().find(|(c, _)| *c == "folder_ids").unwrap().1;
        assert_eq!(folder_ids, &Value::Text(r#"["f1"]"#.to_string()));
        let full_text = map.iter().find(|(c, _)| *c == "full_text").unwrap().1;
        assert!(full_text != &Value::Text("SHOULD-BE-OVERRIDDEN".to_string()));
        // Maintenance came from the same derived data.
        // Maintenance carries the typed participants (empty here) + tag ids.
        assert!(maint.participants.is_empty());
        assert!(maint.indexed_tag_ids.is_empty());
    }
    /// S-D2a consistency boundary: when a caller builds the FTS inputs from
    /// the *same* authoritative `Participants`/`discs`/`DbTags` values (the
    /// invariant S-D2b record construction must preserve atomically), the
    /// `full_text`/`search_participants` columns agree with the JSON/junction
    /// values. `format_full_text` sorts/dedupes the words, so the non-
    /// deterministic `maps.Values`/`AllNames` order is irrelevant.
    #[test]
    fn derived_fts_inputs_are_consistent_with_the_authoritative_values() {
        let participants = Participants::from([(
            Role::from("albumartist"),
            vec![Participant {
                id: "a1".into(),
                name: "Artist One".into(),
                sub_role: "".into(),
            }],
        )]);
        let mut names: Vec<String> = Vec::new();
        for ps in participants.values() {
            for p in ps {
                if !names.iter().any(|n| n == &p.name) {
                    names.push(p.name.clone());
                }
            }
        }
        let mut discs = std::collections::BTreeMap::new();
        discs.insert(1, "Disc One".to_string());
        discs.insert(10, "Disc Ten".to_string());
        // Go maps.Values order is non-deterministic; here we use one order.
        let disc_values: Vec<String> = discs.values().cloned().collect();
        let tags: DbTags = [
            ("albumversion".to_string(), vec!["Edition 2".to_string()]),
            ("catalognumber".to_string(), vec!["CN-1".to_string()]),
        ]
        .into_iter()
        .collect();
        let d = AlbumDerived {
            name: "N".into(),
            sort_album_name: "S".into(),
            album_artist: "AA".into(),
            participant_names: names,
            disc_values: disc_values.clone(),
            album_version: tags.get("albumversion").cloned().unwrap_or_default(),
            catalog_number: tags.get("catalognumber").cloned().unwrap_or_default(),
            tags: tags.clone(),
            participants: participants.clone(),
            folder_ids: None,
            discs: Some(discs.clone()),
        };
        let (map, maint) = album_post_map_args(ColumnMap::new(BTreeMap::new()), d);
        let get = |c: &str| map.iter().find(|(k, _)| *k == c).unwrap().1;
        // The full_text carries the participant names, disc values, and both
        // tag values from the authoritative values. `format_full_text`
        // (`SanitizeStrings`) sorts/dedupes the *words*, so phrase order is
        // not preserved — assert each word (the `maps.Values`/`AllNames`
        // non-deterministic order is irrelevant).
        let full = get("full_text").to_string();
        for w in ["artist", "disc", "edition", "cn-1"] {
            assert!(
                full.contains(w),
                "full_text {full:?} must contain word {w:?}"
            );
        }
        // search_participants = normalized (sorted) all_names — matches the
        // same participants value the JSON/junction maintenance uses.
        let sp = get("search_participants").to_string();
        assert!(sp.contains("Artist One"), "search_participants {sp:?}");
        // Maintenance participants IS the same value.
        assert_eq!(maint.participants, participants);
        // discs JSON comes from the same discs value.
        let discs_json = get("discs").to_string();
        assert!(discs_json.contains("Disc One") && discs_json.contains("Disc Ten"));
    }
    /// `toSnakeCase` (`helpers.go:48-52`), pinned against the real Go helper
    /// (`go run`). Covers the four forms the review names plus the tricky
    /// acronym/edge cases so a hand-rolled converter that gets the two-regex
    /// precedence wrong is caught.
    #[test]
    fn to_snake_case_matches_go_helper() {
        let cases: &[(&str, &str)] = &[
            ("updatedAt", "updated_at"),
            ("UpdatedAt", "updated_at"),
            ("ProbeData", "probe_data"),
            ("RGAlbumGain", "rg_album_gain"),
            ("RGTrackPeak", "rg_track_peak"),
            ("MBZAlbumID", "mbz_album_id"),
            ("ID3", "id3"),
            ("XID", "xid"),
            ("AB", "ab"),
            ("A1B", "a1_b"),
            ("camelCase", "camel_case"),
            ("PascalCase", "pascal_case"),
            ("ALLCAPS", "allcaps"),
            ("snake_case", "snake_case"),
            ("id", "id"),
            ("created_at", "created_at"),
            ("birth_time", "birth_time"),
            ("A", "a"),
            ("", ""),
            ("RGTrackPeak", "rg_track_peak"),
            ("ExternalInfoUpdatedAt", "external_info_updated_at"),
            ("UPPERlower", "uppe_rlower"),
            ("A1b2C3", "a1b2_c3"),
            ("AbcD", "abc_d"),
            ("ABCD", "abcd"),
            ("aBcD", "a_bc_d"),
        ];
        for (input, expected) in cases {
            assert_eq!(to_snake_case(input), *expected, "to_snake_case({input:?})");
        }
    }
    /// `cols_to_update` entries are `toSnakeCase`-normalized before matching
    /// the snake_case column keys, exactly as Go's `selectUpdateColumns`
    /// (`sql_base_repository.go:597`) does — so a camelCase `updatedAt` restricts
    /// the update to `updated_at`.
    #[test]
    fn camel_cased_cols_to_update_are_normalized_before_matching() {
        let mut base = BTreeMap::new();
        base.insert("name".to_string(), text("n"));
        base.insert("updated_at".to_string(), text("t1"));
        base.insert("rg_album_gain".to_string(), text("-9.5"));
        // Go callers pass the model field names (camelCase/PascalCase);
        // the filter must match the snake_case map keys.
        let row = MarshalledRow::new(
            "album",
            "a-1",
            &ColumnMap::new(base.clone()),
            &["updatedAt", "RGAlbumGain"],
        )
        .unwrap();
        assert_eq!(
            row.update_columns,
            vec!["rg_album_gain".to_string(), "updated_at".to_string()]
        );
        assert_eq!(row.update_args, vec![text("-9.5"), text("t1"), text("a-1")]);
        // PascalCase normalizes identically (helpers.go:48-52).
        let row = MarshalledRow::new(
            "album",
            "a-1",
            &ColumnMap::new(base.clone()),
            &["UpdatedAt"],
        )
        .unwrap();
        assert_eq!(row.update_columns, vec!["updated_at".to_string()]);
        // INSERT is unaffected: all columns present.
        assert_eq!(
            row.insert_columns,
            vec![
                "name".to_string(),
                "rg_album_gain".to_string(),
                "updated_at".to_string(),
                "id".to_string()
            ]
        );
        // An unknown-only filter matches nothing → typed error (the map has
        // no such column), not invalid SQL.
        let err =
            MarshalledRow::new("album", "a-1", &ColumnMap::new(base), &["NotAColumn"]).unwrap_err();
        assert_eq!(err, MarshalError::EmptyUpdateSet);
    }

    #[test]
    fn folder_nil_image_files_stores_empty_array_not_null() {
        let mut base = BTreeMap::new();
        base.insert("name".to_string(), text("rock"));
        let map = folder_post_map_args(ColumnMap::new(base), FolderDerived { image_files: None });
        let v = map.iter().find(|(c, _)| *c == "image_files").unwrap().1;
        assert_eq!(v, &Value::Text("[]".to_string()));

        // Non-empty stores the JSON array (order preserved).
        let base2 = BTreeMap::new();
        let map2 = folder_post_map_args(
            ColumnMap::new(base2),
            FolderDerived {
                image_files: Some(vec!["a.jpg".into(), "b.jpg".into()]),
            },
        );
        let v2 = map2.iter().find(|(c, _)| *c == "image_files").unwrap().1;
        assert_eq!(v2, &Value::Text(r#"["a.jpg","b.jpg"]"#.to_string()));
    }

    #[test]
    fn nullable_strings_and_discs_nil_store_null() {
        // album folder_ids: None (Go nil) → null (no special arm, unlike folder).
        let base = BTreeMap::new();
        let d = AlbumDerived {
            name: "N".into(),
            sort_album_name: "S".into(),
            album_artist: "AA".into(),
            participant_names: vec![],
            disc_values: vec![],
            album_version: vec![],
            catalog_number: vec![],
            tags: DbTags::new(),
            participants: Participants::new(),
            folder_ids: None,
            discs: None,
        };
        let (map, _) = album_post_map_args(ColumnMap::new(base), d);
        let fid = map.iter().find(|(c, _)| *c == "folder_ids").unwrap().1;
        assert_eq!(fid, &Value::Text("null".to_string()));
        let discs = map.iter().find(|(c, _)| *c == "discs").unwrap().1;
        assert_eq!(discs, &Value::Text("null".to_string()));
    }
}
