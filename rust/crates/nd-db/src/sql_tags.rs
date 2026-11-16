//! Port of `persistence/sql_tags.go` (231L) — the tag join-table half of the
//! tag repositories: `album_tags` / `media_file_tags` rows plus the `tag`
//! dictionary (`crates/nd-dbschema/src/schema_ddl.rs:55-74`).
//!
//! Ported here: the dbTag/dbTags JSON column (`marshalTags`/`unmarshalTags`,
//! `:22-47`), the indexed-tag list (`indexedTagNames`, `:51`), the delete-then-insert
//! row rewrite (`sqlRepository.updateTags`, `:57-79`), `genreFilterDef`'s
//! `ByID`/`ByName` semi-joins and `AlbumArtistsByGenreID` (`:85-118`), the
//! `library_tag` filter Eq (`:134-136`) and `applyLibraryFiltering`'s join
//! fragments (`:204-212`).
//!
//! Not ported, with the reason: `tagIDFilter` (`:124-131`) is a squirrel `Exists`/
//! `NotEq` node `crate::sqlrepo` has no `Condition` for, and the `baseTagRepository`
//! CRUD/REST surface (`:148-231` — `registerModel`, `setSortMappings`,
//! `newSelect`, `Count`, `Read*`) belongs to the repository slices that own that
//! registry. `applyLibraryFiltering` is returned as fragments because `crate::sqlrepo`
//! renders only INNER JOIN while Go LEFT-joins `library_tag`, and LEFT is what lets
//! untagged rows keep their `COALESCE(SUM(…), 0)` zeros; the auth-aware slice
//! attaches them (it also owns `loggedUser(ctx)`/`invalidUserId`, `:206-210`).
//! `ByName`'s squirrel `Like` has no `Condition` either, so it is a `where_raw`
//! fragment — the genre value is still a bound `?` arg, only the operator is text.
//!
//! `Tags` is `map[TagName][]string` in Go and still unmodelled in Rust
//! (`crates/nd-model/src/album.rs:6`), so this port uses nd-conf's free-form string-map
//! spelling (`Tags.*`, `crates/nd-conf/src/keys.rs:469`): a sorted map of tag name to
//! ordered values. Sorted also makes `marshal_tags` byte-stable, where Go's map
//! iteration order is unspecified; `new_tag_id` is what Go's `model.NewTag` yields.
//!
//! Safety rule carried over from `sqlrepo.rs`: table/column/join identifiers come only
//! from caller-supplied schema names (the `_tags`/`_id` suffixes, the [`GenreFilterDef`]
//! fields), never from user text; every value — item ids, tag ids, genre names, the
//! `json_each` id array — is parameterized (`?` + [`Value`]).

use std::collections::BTreeMap;

use crate::sql_json::JsonColumnError;
use crate::sqlrepo::Repository;
use crate::sqlrepo::Value;
use serde_json::Value as JsonValue;

use nd_id::new_tag_id;

use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::from_str;
use serde_json::to_string;

/// Go's `dbTags map[model.TagName][]dbTag` (`:22-27`) minus the ids `marshalTags`
/// generates: the in-memory `model.Tags` shape, tag name → ordered values.
pub type DbTags = BTreeMap<String, Vec<String>>;

/// One materialized statement or clause: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape `executeSQL` consumes.
pub type Sql = (String, Vec<Value>);

/// Go's `dbTag` (`:22-24`), one element of the tags JSON column.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DbTag {
    /// `model.NewTag(name, value).ID`, i.e. [`nd_id::new_tag_id`]: the
    /// `(tag_name, tag_value)`-unique dictionary id.
    #[serde(default)]
    pub id: String,
    /// The display value (Go `Value`, `:23`).
    #[serde(default)]
    pub value: String,
}

/// The tag types materialized into the `<table>_tags` join tables, so filtering by
/// them is an index-backed semi-join instead of a per-row `json_tree(tags)` scan
/// (`indexedTagNames`, `:51`; Go `model.TagGenre`, `model/tag.go:193`). Genre only
/// for now, so any other tag name still needs the unported `tagIDFilter` JSON path.
pub const INDEXED_TAG_NAMES: [&str; 1] = ["genre"];

/// The `_tags` join-table suffix of `<table>_tags` (`updateTags`, `:57`/`:66`).
pub const TAGS_TABLE_SUFFIX: &str = "_tags";

/// The `_id` suffix of `<table>_id`, the column scoping a join row to one item.
pub const ID_COLUMN_SUFFIX: &str = "_id";

/// The `tag_id` column of every `<table>_tags` row (`:67`, `ByID`, `:96`).
pub const TAG_ID_COLUMN: &str = "tag_id";

/// The `tag` dictionary table the INSERT and the semi-joins JOIN (`:68`, `:105`).
pub const TAG_TABLE: &str = "tag";

/// The `tag.tag_name` column `ByName` pins (`:106`).
pub const TAG_NAME_COLUMN: &str = "tag.tag_name";

/// The `tag.tag_value` column `ByName` LIKEs (`:107`).
pub const TAG_VALUE_COLUMN: &str = "tag.tag_value";

/// The `library_tag` join table `applyLibraryFiltering` aggregates through
/// (`:205`) and `tagLibraryIdFilter` filters (`:134-136`).
pub const LIBRARY_TAG_TABLE: &str = "library_tag";

/// `genreFilterDef` (`:85`): one item type's genre filter — outer column, join
/// table, join-table id column.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenreFilterDef {
    /// The outer column matched, e.g. `media_file.id` (`:88`).
    pub id_col: String,
    /// The `<table>_tags` join table, e.g. `media_file_tags` (`:88`).
    pub table: String,
    /// The join table's id column, e.g. `media_file_id` (`:88`).
    pub join_col: String,
}

/// Go's `SongGenres` instance (`:88`).
#[must_use]
pub fn song_genres() -> GenreFilterDef {
    GenreFilterDef {
        id_col: "media_file.id".to_string(),
        table: "media_file_tags".to_string(),
        join_col: "media_file_id".to_string(),
    }
}

/// Go's `AlbumGenres` instance (`:89`).
#[must_use]
pub fn album_genres() -> GenreFilterDef {
    GenreFilterDef {
        id_col: "album.id".to_string(),
        table: "album_tags".to_string(),
        join_col: "album_id".to_string(),
    }
}
/// The genre tag name (`model.TagGenre`, `model/tag.go:193`; the same value as
/// `INDEXED_TAG_NAMES[0]`). Restated here so this module stays self-contained:
/// `genre_repo::GENRE_TAG_NAME` lives in another lane's file, and its `Genre`
/// type carries aggregate counts a hydration read cannot fill.
pub const GENRE_TAG_NAME: &str = "genre";

/// Hydrated genre entry — the port of `model.Genre` (`model/genre.go:9-14`) as
/// `Tags.ToGenres` (`model/tag.go:138-148`) produces it. Go's `SongCount`/
/// `AlbumCount` (`model/genre.go:12-13`) are `structs:"-"` aggregate columns only
/// the genre repository's `COALESCE(SUM(…))` select fills, and `ToGenres` leaves
/// them at Go's zero, so this hydrated shape declares the two fields `ToGenres`
/// actually sets. `genre_repo::Genre` (`genre_repo.rs:28-36`) carries the counts
/// and is not this lane's file, so the DTO is declared here — DTOs stay local to
/// nd-db (`hydrate-contract.md` §4: nd-model must not gain them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Genre {
    /// `NewTag("genre", value).ID` (`model/tag.go:63-71`), i.e.
    /// [`nd_id::new_tag_id`], which lowercases name AND value before hashing.
    pub id: String,
    /// The tag value verbatim (`Genre.Name`, `model/tag.go:145`): not lowercased,
    /// not deduplicated, not sorted.
    pub name: String,
}

/// `Tags.ToGenres` (`model/tag.go:138-148`): the `genre` tag values as
/// `(first name, entries)`, the pair `dbAlbum.PostScan`/`dbMediaFile.PostScan`
/// assign to `Genre`/`Genres` (`album_repository.go:57`,
/// `mediafile_repository.go:64`).
///
/// Semantics pinned by the Go source (verified with
/// `{"genre":["Rock","Jazz","rock","Rock"]}` — scalar `Rock`, 4 entries, stored
/// order, `Rock`/`rock` sharing an id):
/// * `Values("genre")` (`model/tag.go:81-83`) is a plain map read, so a missing
///   key is a nil slice and no other tag name is ever consulted;
/// * an empty or absent list returns `("", vec![])` — Go's early return
///   (`:140-142`) yields `""` and a NIL `Genres`, which the empty `Vec` spells.
///   Neither reaches a write map (`Genres` is `structs:"-"`), so the distinction
///   is invisible to SQL;
/// * one entry per value in STORED order with NO dedup and NO sort — `slice.Map`
///   (`:143-146`) maps element-wise;
/// * `id` comes from `NewTag("genre", g)` (`model/tag.go:63-71`), which lowercases
///   the name and the value before hashing (`tagID`, `:77-79`), while `Name` stays
///   verbatim;
/// * the scalar is `genres[0].Name` (`:147`) — the FIRST value, not the most
///   common one, and whatever the `genre` column said is overwritten.
#[must_use]
pub fn tags_to_genres(tags: &DbTags) -> (String, Vec<Genre>) {
    let Some(values) = tags.get(GENRE_TAG_NAME) else {
        return (String::new(), Vec::new());
    };
    if values.is_empty() {
        return (String::new(), Vec::new());
    }
    let genres: Vec<Genre> = values
        .iter()
        .map(|value| Genre {
            id: new_tag_id(GENRE_TAG_NAME, value),
            name: value.clone(),
        })
        .collect();
    // `genres[0].Name` — the guard above makes the index safe.
    (genres[0].name.clone(), genres)
}

/// `marshalTags` (`:37-47`): the dbTags JSON column — one `{id, value}` entry per
/// tag value, the id from [`nd_id::new_tag_id`] (Go's `model.NewTag`). Go ignores
/// `json.Marshal`'s error (`:45`), which cannot occur for this shape.
///
/// **A name with no values gets no key.** Go's loop appends per value, so an empty
/// (or nil) value list never creates `dbTags[name]`: executed against the real
/// `marshalTags`, `model.Tags{"genre": {}}` and `model.Tags{"genre": nil}` both
/// marshal `{}`, and `{"genre": {}, "mood": {"Chill"}}` marshals only the `mood`
/// key. serde's `BTreeMap` would emit `{"genre":[]}`, so empty lists are dropped
/// here — the same shape Go writes, and the reason a name is absent from the column
/// rather than present-but-empty.
#[must_use]
pub fn marshal_tags(tags: DbTags) -> String {
    let mut db_tags: BTreeMap<String, Vec<DbTag>> = BTreeMap::new();
    for (name, values) in tags {
        // Go's `for _, value := range values` appends nothing for an empty list, so
        // the key is never created (`dbTags[name]` stays absent).
        if values.is_empty() {
            continue;
        }
        let entries = values
            .iter()
            .map(|value| DbTag {
                id: new_tag_id(&name, value),
                value: value.clone(),
            })
            .collect();
        db_tags.insert(name, entries);
    }
    to_string(&db_tags).unwrap_or_else(|_| "{}".to_string())
}

/// `unmarshalTags` (`:22-35`): dbTags JSON → name → values, dropping the ids (Go
/// reads only `tag.Value`, `:31`). `(tags, error)` is this crate's spelling of Go's
/// two-value return, carrying Go's `parsing tags:` prefix and `""` for no error.
///
/// Decoding is Go-faithful about `null`, which serde is not: `encoding/json`
/// assigns the **zero value** to a non-pointer map/list/element/field for a JSON
/// `null` (`decode.go` `literalStore`), executed against the real `unmarshalTags`:
/// `null` → empty map with `err=nil` (`model.Tags{}`), `{"genre":null}` →
/// `[]string{}`, `{"genre":[null]}` and `{"genre":[{"id":null,"value":null}]}` →
/// `[""]`. serde would reject all four as type mismatches, so they go through
/// `decode_tags`'s `Option` fields. Any other wrong type still errors, as Go's
/// `json: cannot unmarshal array into dbTags.genre.0 of type persistence.dbTag`
/// does. `""` is an error on both sides (Go never calls it: the hook guards
/// `a.Tags != ""`, `album_repository.go:52`).
#[must_use]
pub fn unmarshal_tags(data: String) -> (DbTags, String) {
    match decode_tags(&data) {
        Ok(tags) => (tags, "".to_string()),
        Err(error) => (DbTags::new(), format!("parsing tags: {error}")),
    }
}

/// The element shape as `encoding/json` decodes it: both fields nullable, so a
/// JSON `null` becomes the empty string rather than a serde type error.
#[derive(Deserialize)]
struct RawDbTag {
    /// Go's `dbTag.ID`. `unmarshalTags` never reads it (`:31-34` reads only
    /// `tag.Value`), but its decoder still type-checks the field, so keeping it is
    /// what makes `{"genre":[{"id":5}]}` an error on both sides rather than being
    /// silently ignored.
    #[allow(dead_code)]
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

/// `json.Unmarshal(data, &dbTags)` (`persistence/sql_tags.go:22-27`) over the
/// null-tolerant shape. Errors carry no column token, because Go's wrap is only
/// `parsing tags:` (`:26`) and never names the column.
fn decode_tags(data: &str) -> Result<DbTags, JsonColumnError> {
    let root: JsonValue = from_str(data).map_err(JsonColumnError::bare)?;
    match &root {
        // Go: `null` into a map leaves it nil and returns no error; `unmarshalTags`
        // then builds `make(model.Tags, 0)` — an empty, non-nil map.
        JsonValue::Null => return Ok(DbTags::new()),
        JsonValue::Object(_) => {}
        other => {
            return Err(JsonColumnError::bare(serde_json::Error::custom(format!(
                "tags column must be a JSON object or null, not {}",
                json_type_name(other)
            ))));
        }
    }
    let mut out = DbTags::new();
    for (name, value) in root.as_object().expect("matched above") {
        let values: Vec<String> = match value {
            JsonValue::Null => Vec::new(),
            JsonValue::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    match item {
                        JsonValue::Null => values.push(String::new()),
                        JsonValue::Object(_) => {
                            let raw: RawDbTag = serde_json::from_value(item.clone())
                                .map_err(JsonColumnError::bare)?;
                            values.push(raw.value.unwrap_or_default());
                        }
                        other => {
                            return Err(JsonColumnError::bare(serde_json::Error::custom(format!(
                                "cannot unmarshal {} into dbTags.{name}.{i}",
                                json_type_name(other)
                            ))));
                        }
                    }
                }
                values
            }
            other => {
                return Err(JsonColumnError::bare(serde_json::Error::custom(format!(
                    "cannot unmarshal {} into dbTags.{name}",
                    json_type_name(other)
                ))));
            }
        };
        out.insert(name.clone(), values);
    }
    Ok(out)
}

/// Go's `decode.go` error vocabulary for value-level rejections.
fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

/// `updateTags`' id collection (`:61-65`): one `model.NewTag` id per indexed tag
/// name × value. Unindexed names contribute nothing — their rows live only in the
/// `tags` JSON column the unported `tagIDFilter` reads.
#[must_use]
pub fn indexed_tag_ids(tags: DbTags) -> Vec<String> {
    let mut ids: Vec<String> = vec![];
    for name in INDEXED_TAG_NAMES {
        if let Some(values) = tags.get(name) {
            ids.extend(values.iter().map(|value| new_tag_id(name, value)));
        }
    }
    ids
}

/// The DELETE half of `updateTags` (`:57-60`), always issued before any insert so
/// a re-saved item cannot keep stale join rows.
#[must_use]
pub fn delete_tags_sql(table: String, item_id: String) -> Sql {
    (
        format!("DELETE FROM {table}{TAGS_TABLE_SUFFIX} WHERE {table}{ID_COLUMN_SUFFIX} = ?"),
        vec![Value::from(item_id)],
    )
}

/// The INSERT half of `updateTags` (`:61-79`): a scoped `INSERT … SELECT` over
/// `json_each` of the marshalled ids, JOINed to `tag` so ids the dictionary has
/// not saved cannot create a row, and `ON CONFLICT … DO NOTHING` for the
/// `unique (<table>_id, tag_id)` constraint (`schema_ddl.rs:72-74`). `None` for
/// an empty id list — Go returns before building the query (`:72-74`).
#[must_use]
pub fn insert_tags_sql(table: String, item_id: String, tag_ids: Vec<String>) -> Option<Sql> {
    if tag_ids.is_empty() {
        return None;
    }
    let ids_json = to_string(&tag_ids).unwrap_or_else(|_| "[]".to_string());
    let scoped = format!("{table}{ID_COLUMN_SUFFIX}");
    Some((
        format!("INSERT INTO {table}{TAGS_TABLE_SUFFIX} ({scoped}, {TAG_ID_COLUMN}) SELECT ?, value FROM json_each(?) JOIN {TAG_TABLE} ON {TAG_TABLE}.id = value ON CONFLICT ({scoped}, {TAG_ID_COLUMN}) DO NOTHING"),
        vec![Value::from(item_id), Value::from(ids_json)],
    ))
}

/// `updateTags` (`:57-79`) as its two statements in one Put (Go's `executeSQL`
/// pair inside `WithTx`, ported in `store.rs`): the delete always runs, the
/// insert only when the item has indexed tags.
#[must_use]
pub fn update_tags_sql(table: String, item_id: String, tags: DbTags) -> UpdateTags {
    UpdateTags {
        delete: delete_tags_sql(table.clone(), item_id.clone()),
        insert: insert_tags_sql(table, item_id, indexed_tag_ids(tags)),
    }
}

/// The delete-then-insert pair `updateTags` issues.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateTags {
    /// `Delete(<table>_tags).Where(Eq{<table>_id})` (`:57-60`).
    pub delete: Sql,
    /// The scoped INSERT, absent when no indexed tag ids exist (`:72-74`).
    pub insert: Option<Sql>,
}

/// `genreFilterDef.ByID`'s subquery (`:92-99`): `SELECT <joinCol> FROM <table>
/// WHERE tag_id IN (?, …)`; squirrel expands the id slice to placeholders.
#[must_use]
pub fn genre_ids_subquery(def: GenreFilterDef, tag_ids: Vec<String>) -> Sql {
    let values = tag_ids.iter().map(|id| Value::from(id.clone())).collect();
    let builder = Repository::new(def.table)
        .select([def.join_col])
        .where_in(TAG_ID_COLUMN, values);
    // The join table is always set, so materializing cannot fail (see sqlrepo.rs).
    builder.to_sql().expect("the genre semi-join materializes")
}

/// `genreFilterDef.ByID` (`:92-99`): `<idCol> IN (<subquery>)`, the semi-join
/// `Sqlizer` that matches items carrying any of the given genre tag ids.
#[must_use]
pub fn genre_id_filter(def: GenreFilterDef, tag_ids: Vec<String>) -> Sql {
    let id_col = def.id_col.clone();
    let (subquery, args) = genre_ids_subquery(def, tag_ids);
    (format!("{id_col} IN ({subquery})"), args)
}

/// `genreFilterDef.ByName` (`:101-109`): the name-resolving semi-join — the
/// dictionary is uniquely indexed on `(tag_name, tag_value)` (`schema_ddl.rs:55`),
/// so the tag name is Eq-bound and the genre LIKE-bound. `crate::sqlrepo` has no
/// `Like` condition, hence the raw `?`-bearing fragment for the LIKE.
#[must_use]
pub fn genre_name_filter(def: GenreFilterDef, genre: String) -> Sql {
    let table = def.table;
    let join_col = def.join_col;
    let join_table = format!("{table} jt");
    let join_column = format!("jt.{join_col}");
    let on = format!("{TAG_TABLE}.id = {join_column}");
    let builder = Repository::new(join_table)
        .select([join_column])
        .inner_join(TAG_TABLE, on)
        .where_eq(TAG_NAME_COLUMN, INDEXED_TAG_NAMES[0])
        .where_raw(
            format!("{TAG_VALUE_COLUMN} LIKE ?"),
            vec![Value::from(genre)],
        );
    let (subquery, args) = builder
        .to_sql()
        .expect("the genre-name semi-join materializes");
    let id_col = def.id_col;
    (format!("{id_col} IN ({subquery})"), args)
}

/// `AlbumArtistsByGenreID` (`:111-118`): album artists of albums carrying any of
/// the genre ids — a two-table join (`album_artists` ⨝ `album_tags`) that does not
/// fit [`GenreFilterDef`], wrapped for the artist repository.
#[must_use]
pub fn album_artists_by_genre_ids(tag_ids: Vec<String>) -> Sql {
    let values = tag_ids.iter().map(|id| Value::from(id.clone())).collect();
    let builder = Repository::new("album_artists aa")
        .select(["aa.artist_id"])
        .inner_join("album_tags at", "at.album_id = aa.album_id")
        .where_eq("aa.role", "albumartist")
        .where_in(format!("at.{TAG_ID_COLUMN}"), values);
    let (subquery, args) = builder
        .to_sql()
        .expect("the album-artist semi-join materializes");
    (format!("artist.id IN ({subquery})"), args)
}

/// `applyLibraryFiltering`'s join fragments (`:204-212`) and their bound args.
/// Fragments, not builder calls, because `crate::sqlrepo` renders only INNER JOIN
/// and Go LEFT-joins `library_tag` — LEFT is what keeps untagged rows contributing
/// their `COALESCE(SUM(…), 0)` zeros, so the auth-aware slice must attach them
/// itself. Go adds the `user_library` join only when `loggedUser(ctx).ID !=
/// invalidUserId` (`:206-210`), which this crate cannot read, so the caller passes
/// the user id or `None`.
#[must_use]
pub fn library_filter_joins(user_id: Option<String>) -> (Vec<String>, Vec<Value>) {
    let mut joins = vec![format!(
        "{LIBRARY_TAG_TABLE} on {LIBRARY_TAG_TABLE}.tag_id = {TAG_TABLE}.id"
    )];
    let mut args = vec![];
    if let Some(id) = user_id {
        joins.push(
            "user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ?".to_string(),
        );
        args.push(Value::from(id));
    }
    (joins, args)
}

/// `tagLibraryIdFilter` (`:134-136`): the Eq the tag repository registers for its
/// `library_id` key (`:167`), applied through the builder so the value stays bound.
#[must_use]
pub fn where_tag_library_id(sq: Repository, library_id: String) -> Repository {
    sq.where_eq(format!("{LIBRARY_TAG_TABLE}.library_id"), library_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Tags.ToGenres` (`model/tag.go:138-148`), the four modes its Go body
    /// distinguishes. The `{"genre":[…]}` payloads are what a `tags` column
    /// decodes to after `unmarshalTags`, and the ids are `NewTag("genre", v)`
    /// (`model/tag.go:63-71`) — `new_tag_id` lowercases name AND value, so the
    /// case-variant and the repeat share one id while `name` stays verbatim.
    #[test]
    fn empty_or_absent_genre_list_is_the_go_early_return() {
        // Absent key: Go's map read returns a nil slice (`model/tag.go:81-83`).
        let tags = DbTags::new();
        assert_eq!(tags_to_genres(&tags), (String::new(), Vec::new()));
        // Present-but-empty list: `len(values) == 0` takes the same arm (`:140`).
        let mut tags = DbTags::new();
        tags.insert("genre".to_string(), vec![]);
        assert_eq!(tags_to_genres(&tags), (String::new(), Vec::new()));
        // A non-genre name never contributes (`Values("genre")` only).
        let mut tags = DbTags::new();
        tags.insert("mood".to_string(), vec!["Chill".to_string()]);
        assert_eq!(tags_to_genres(&tags), (String::new(), Vec::new()));
    }

    #[test]
    fn single_genre_is_both_the_scalar_and_the_one_entry() {
        let mut tags = DbTags::new();
        tags.insert("genre".to_string(), vec!["Rock".to_string()]);
        let (scalar, genres) = tags_to_genres(&tags);
        assert_eq!(scalar, "Rock".to_string());
        assert_eq!(
            genres,
            vec![Genre {
                id: new_tag_id("genre", "Rock"),
                name: "Rock".to_string(),
            }]
        );
    }

    #[test]
    fn genre_entries_keep_stored_order_and_never_dedup() {
        // The brief's verified case: 4 entries, no dedup, no sort, scalar = first.
        let mut tags = DbTags::new();
        tags.insert(
            "genre".to_string(),
            vec![
                "Rock".to_string(),
                "Jazz".to_string(),
                "rock".to_string(),
                "Rock".to_string(),
            ],
        );
        let (scalar, genres) = tags_to_genres(&tags);
        assert_eq!(scalar, "Rock".to_string());
        assert_eq!(genres.len(), 4, "slice.Map is element-wise, no dedup");
        let names: Vec<&str> = genres.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["Rock", "Jazz", "rock", "Rock"]);
        // NewTag lowercases before hashing: Rock/rock/Rock share one id, Jazz has
        // its own (`id.NewTagID`, model/id/id.go:46-48).
        let shared = new_tag_id("genre", "Rock");
        assert_eq!(new_tag_id("genre", "rock"), shared, "case-insensitive id");
        assert_eq!(genres[0].id, shared);
        assert_eq!(genres[1].id, new_tag_id("genre", "Jazz"));
        assert_eq!(genres[2].id, shared);
        assert_eq!(genres[3].id, shared);
        assert_ne!(genres[0].id, genres[1].id);
    }

    #[test]
    fn genre_name_is_verbatim_including_odd_case_and_spacing() {
        // `Genre.Name` is the tag value unchanged; only the id is lowercased.
        let mut tags = DbTags::new();
        tags.insert(
            "genre".to_string(),
            vec!["Post-Punk".to_string(), "  Nu Jazz  ".to_string()],
        );
        let (scalar, genres) = tags_to_genres(&tags);
        assert_eq!(scalar, "Post-Punk".to_string());
        assert_eq!(genres[1].name, "  Nu Jazz  ".to_string());
        assert_eq!(genres[1].id, new_tag_id("genre", "  Nu Jazz  "));
    }

    #[test]
    fn marshal_tags_drops_names_with_an_empty_value_list_like_go() {
        // Arrange: a tag name mapped to NO values. Go's `marshalTags` (`:39-47`)
        // appends per value into `dbTags`, so the key is never created — executed
        // against the real function: `model.Tags{"genre": {}}` and
        // `model.Tags{"genre": nil}` both marshal `{}`, and
        // `{"genre": {}, "mood": {"Chill"}}` marshals only the `mood` key.
        // serde's map would emit `{"genre":[]}`.
        let mut empty = DbTags::new();
        empty.insert("genre".to_string(), Vec::new());
        // Act:
        let json = marshal_tags(empty);
        // Assert:
        assert_eq!(json, "{}", "an empty tag name gets no key, as Go");
        let mut mixed = DbTags::new();
        mixed.insert("genre".to_string(), Vec::new());
        mixed.insert("mood".to_string(), vec!["Chill".to_string()]);
        assert_eq!(
            marshal_tags(mixed),
            format!(
                r#"{{"mood":[{{"id":"{}","value":"Chill"}}]}}"#,
                new_tag_id("mood", "Chill")
            ),
            "only the populated name survives, matching Go"
        );
        // Absent-on-write stays absent-on-read: ToGenres sees no genre key at all.
        let (tags, err) = unmarshal_tags(json);
        assert_eq!(err, "");
        assert_eq!(tags.get("genre"), None);
        let (scalar, genres) = tags_to_genres(&tags);
        assert_eq!(scalar, "");
        assert!(genres.is_empty());
    }

    #[test]
    fn tags_json_round_trips_with_ids_from_new_tag_id() {
        // Arrange: model.Tags with one indexed and one unindexed name (sql_tags.go:22-47).
        let mut tags = DbTags::new();
        tags.insert(
            "genre".to_string(),
            vec!["Rock".to_string(), "Punk".to_string()],
        );
        tags.insert("custom.Label".to_string(), vec!["X".to_string()]);
        // Act:
        let json = marshal_tags(tags.clone());
        let (back, error) = unmarshal_tags(json.clone());
        // Assert:
        assert_eq!(error, "".to_string());
        assert_eq!(back, tags);
        let label_x_id = new_tag_id("custom.Label", "X");
        let rock_id = new_tag_id("genre", "Rock");
        let punk_id = new_tag_id("genre", "Punk");
        assert_eq!(
            json,
            format!("{{\"custom.Label\":[{{\"id\":\"{label_x_id}\",\"value\":\"X\"}}],\"genre\":[{{\"id\":\"{rock_id}\",\"value\":\"Rock\"}},{{\"id\":\"{punk_id}\",\"value\":\"Punk\"}}]}}"),
        );
    }

    #[test]
    fn null_list_null_element_and_null_field_zero_fill_like_go() {
        // Arrange / Assert: executed against the real Go `unmarshalTags`, every
        // `null` position decodes to the zero value with `err=nil` —
        // `{"genre":null}` -> `[]string{}`, `{"genre":[null]}` and
        // `{"genre":[{"id":null,"value":null}]}` -> `[""]`, and a top-level `null`
        // -> an empty, NON-nil map. serde's plain derive rejects all of these as
        // `invalid type: null, expected a sequence`, so decode_tags goes through
        // nullable fields.
        let (tags, err) = unmarshal_tags(r#"{"genre":null}"#.to_string());
        assert_eq!(err, "");
        assert_eq!(tags.get("genre"), Some(&Vec::new()));

        let (tags, err) = unmarshal_tags(r#"{"genre":[null]}"#.to_string());
        assert_eq!(err, "");
        assert_eq!(tags.get("genre"), Some(&vec![String::new()]));

        let (tags, err) = unmarshal_tags(r#"{"genre":[{"id":null,"value":null}]}"#.to_string());
        assert_eq!(err, "");
        assert_eq!(tags.get("genre"), Some(&vec![String::new()]));

        let (tags, err) = unmarshal_tags("null".to_string());
        assert_eq!(err, "");
        assert!(tags.is_empty());
        // A null list still yields a `genre` entry, so ToGenres sees an empty
        // genre list and clears the scalar genre (Go: tags["genre"] is non-nil).
        let (scalar, genres) = tags_to_genres(&tags_with_null_genre());
        assert_eq!(scalar, "");
        assert!(genres.is_empty());

        let (tags, err) = unmarshal_tags(r#"{"genre":[{"value":"Rock"}]}"#.to_string());
        assert_eq!(err, "");
        assert_eq!(tags.get("genre"), Some(&vec!["Rock".to_string()]));
    }

    /// The shape `{"genre":null}` materializes: entry present, list empty.
    fn tags_with_null_genre() -> DbTags {
        let (tags, err) = unmarshal_tags(r#"{"genre":null}"#.to_string());
        assert_eq!(err, "");
        tags
    }

    #[test]
    fn non_null_wrong_types_are_rejected_and_empty_input_errors() {
        // Go rejects a non-object element, a non-array list and a top-level
        // non-object; `""` errors because Go's PostScan guard keeps it away from
        // `json.Unmarshal` but the codec has no guard of its own.
        let (tags, err) = unmarshal_tags(r#"{"genre":[[1]]}"#.to_string());
        assert!(tags.is_empty());
        assert!(err.starts_with("parsing tags: "), "got {err:?}");
        assert!(err.contains("cannot unmarshal"), "got {err:?}");

        let (tags, err) = unmarshal_tags(r#"{"genre":"Rock"}"#.to_string());
        assert!(tags.is_empty());
        assert!(err.contains("cannot unmarshal"), "got {err:?}");

        let (tags, err) = unmarshal_tags("{}".to_string());
        assert_eq!(err, "");
        assert!(tags.is_empty());

        let (tags, err) = unmarshal_tags(String::new());
        assert!(tags.is_empty());
        assert!(err.starts_with("parsing tags: "), "got {err:?}");
    }

    #[test]
    fn malformed_tags_json_reports_the_go_parse_prefix() {
        // Arrange: a dbTags document whose value list is not dbTag objects.
        let json = "{\"genre\":\"Rock\"}".to_string();
        // Act:
        let (tags, error) = unmarshal_tags(json);
        // Assert:
        assert!(tags.is_empty());
        assert!(error.starts_with("parsing tags:"));
    }

    #[test]
    fn update_tags_deletes_then_inserts_only_indexed_names() {
        // Arrange: an album item whose only indexed name is genre (sql_tags.go:57-79).
        let mut tags = DbTags::new();
        tags.insert("genre".to_string(), vec!["Rock".to_string()]);
        tags.insert("custom.Label".to_string(), vec!["X".to_string()]);
        // Act:
        let stmts = update_tags_sql("album".to_string(), "alb-1".to_string(), tags);
        // Assert:
        assert_eq!(
            stmts.delete,
            (
                "DELETE FROM album_tags WHERE album_id = ?".to_string(),
                vec![Value::Text("alb-1".to_string())]
            ),
        );
        let (sql, args) = stmts.insert.unwrap();
        assert_eq!(
            sql,
            "INSERT INTO album_tags (album_id, tag_id) SELECT ?, value FROM json_each(?) JOIN tag ON tag.id = value ON CONFLICT (album_id, tag_id) DO NOTHING".to_string(),
        );
        assert_eq!(args[0], Value::Text("alb-1".to_string()));
        let rock_id = new_tag_id("genre", "Rock");
        assert_eq!(args[1], Value::Text(format!("[\"{rock_id}\"]")));
    }

    #[test]
    fn unindexed_only_tags_skip_the_insert() {
        // Arrange: Go returns before building the INSERT (sql_tags.go:72-74).
        let mut tags = DbTags::new();
        tags.insert("custom.Label".to_string(), vec!["X".to_string()]);
        // Act:
        let stmts = update_tags_sql("album".to_string(), "alb-1".to_string(), tags);
        // Assert:
        assert!(stmts.insert.is_none());
    }

    #[test]
    fn genre_id_semi_join_pins_one_sql_shape() {
        // Arrange: SongGenres.ByID with two genre ids (sql_tags.go:92-99).
        let ids = vec![new_tag_id("genre", "rock"), new_tag_id("genre", "pop")];
        // Act:
        let (sql, args) = genre_id_filter(song_genres(), ids);
        // Assert:
        assert_eq!(
            sql,
            "media_file.id IN (SELECT media_file_id FROM media_file_tags WHERE tag_id IN (?, ?))"
        );
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn genre_name_semi_join_resolves_through_the_tag_dictionary() {
        // Arrange: AlbumGenres.ByName (sql_tags.go:101-109).
        // Act:
        let (sql, args) = genre_name_filter(album_genres(), "Rock".to_string());
        // Assert:
        assert_eq!(
            sql,
            "album.id IN (SELECT jt.album_id FROM album_tags jt INNER JOIN tag ON tag.id = jt.album_id WHERE tag.tag_name = ? AND tag.tag_value LIKE ?)");
        assert_eq!(
            args,
            vec![
                Value::Text("genre".to_string()),
                Value::Text("Rock".to_string())
            ]
        );
    }

    #[test]
    fn album_artists_semi_join_is_a_two_table_join() {
        // Arrange: AlbumArtistsByGenreID (sql_tags.go:111-118).
        // Act:
        let (sql, args) = album_artists_by_genre_ids(vec![new_tag_id("genre", "rock")]);
        // Assert:
        assert_eq!(
            sql,
            "artist.id IN (SELECT aa.artist_id FROM album_artists aa INNER JOIN album_tags at ON at.album_id = aa.album_id WHERE aa.role = ? AND at.tag_id IN (?))");
        assert_eq!(
            args,
            vec![
                Value::Text("albumartist".to_string()),
                Value::Text(new_tag_id("genre", "rock"))
            ]
        );
    }

    #[test]
    fn library_joins_add_the_user_clause_only_when_authenticated() {
        // Arrange: no logged user, then a logged user (sql_tags.go:204-212).
        // Act:
        let (anonymous, no_args) = library_filter_joins(None);
        let (authenticated, args) = library_filter_joins(Some("usr-1".to_string()));
        // Assert:
        assert_eq!(
            anonymous,
            vec!["library_tag on library_tag.tag_id = tag.id"]
        );
        assert!(no_args.is_empty());
        assert_eq!(authenticated.len(), 2);
        assert_eq!(args, vec![Value::Text("usr-1".to_string())]);
    }

    #[test]
    fn library_id_filter_stays_a_bound_argument() {
        // Arrange: tagLibraryIdFilter applied to a tag select (sql_tags.go:134-136).
        let sq = Repository::new(TAG_TABLE).select(["tag.id"]);
        // Act:
        let (sql, args) = where_tag_library_id(sq, "1".to_string())
            .to_sql()
            .expect("renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT tag.id FROM tag WHERE library_tag.library_id = ?"
        );
        assert_eq!(args, vec![Value::Text("1".to_string())]);
    }
}
