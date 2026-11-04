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

use crate::sqlrepo::Repository;
use crate::sqlrepo::Value;

use nd_id::new_tag_id;

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

/// `marshalTags` (`:37-47`): the dbTags JSON column — one `{id, value}` entry per
/// tag value, the id from [`nd_id::new_tag_id`] (Go's `model.NewTag`). Go ignores
/// `json.Marshal`'s error (`:45`), which cannot occur for this shape.
#[must_use]
pub fn marshal_tags(tags: DbTags) -> String {
    let mut db_tags: BTreeMap<String, Vec<DbTag>> = BTreeMap::new();
    for (name, values) in tags {
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
#[must_use]
pub fn unmarshal_tags(data: String) -> (DbTags, String) {
    let db_tags: BTreeMap<String, Vec<DbTag>> = match from_str(&data) {
        Ok(parsed) => parsed,
        Err(error) => return (DbTags::new(), format!("parsing tags: {error}")),
    };
    let mut tags = DbTags::new();
    for (name, entries) in db_tags {
        tags.insert(name, entries.iter().map(|tag| tag.value.clone()).collect());
    }
    (tags, "".to_string())
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
