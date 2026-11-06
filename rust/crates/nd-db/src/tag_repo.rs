//! Port of `persistence/tag_repository.go` (107L) — the tag dictionary
//! repository, built on the `baseTagRepository` select shape of
//! `persistence/sql_tags.go:148-231`.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`queryAll`, which materialize through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`; execution, `WithTx`
//! and logging stay with `store.rs`/the caller, so this port stops at the same
//! `(sql, args)` pairs (see [`Sql`]).
//!
//! Ported here: `Add`'s chunked `tag` + `library_tag` inserts (`:23-45`,
//! `slices.Chunk(tags, 200)` → [`CHUNK_SIZE`]), `UpdateCounts`'s two templated
//! upserts (`:48-69`), `GetAll`'s name-filtered aggregate select
//! (`baseTagRepository.newSelect`, `sql_tags.go:180-199`), and the private
//! `purgeUnused` delete (`:86-103`).
//!
//! Not ported, with the reason: the base repository's REST surface
//! (`Count`/`Read`/`ReadAll`, `sql_tags.go:202-231`) depends on
//! `parseRestOptions` over a live request context that this crate does not
//! own — its filter/sort mapping (`registerModel` at `sql_tags.go:166-172`)
//! is already pinned by `sql_restful.rs`'s registry, so the auth-aware slice
//! attaches it. The `tags` JSON column aggregation Go scans in `purgeUnused`
//! is spelled out literally, as in Go (`json_tree(tags, '$')`).

use crate::sql_tags::{library_filter_joins, Sql};
use crate::sqlrepo::{Repository, Value};

/// `slices.Chunk(tags, 200)` (`tag_repository.go:25`): rows per multi-row
/// INSERT statement pair.
pub const CHUNK_SIZE: usize = 200;

/// The `tag` dictionary table (`newBaseTagRepository`, `sql_tags.go:161`).
pub const TAG_TABLE: &str = "tag";

/// The `library_tag` per-library statistics table (`tag_repository.go:33`).
pub const LIBRARY_TAG_TABLE: &str = "library_tag";

/// `model.Tag` (`model/tag.go`): the dictionary row plus the aggregate counts
/// `GetAll`'s COALESCE columns fill (`sql_tags.go:192-193`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tag {
    /// Dictionary id — `model.NewTag(name, value).ID`.
    pub id: String,
    /// Tag name, e.g. `genre`.
    pub tag_name: String,
    /// Display value.
    pub tag_value: String,
    /// `COALESCE(SUM(library_tag.album_count), 0)` across the user's libraries.
    pub album_count: i64,
    /// `COALESCE(SUM(library_tag.media_file_count), 0)`, Go's `SongCount`.
    pub song_count: i64,
}

/// `model.TagList`.
pub type TagList = Vec<Tag>;

/// One `Add` chunk's two statements, in Go's execution order
/// (`tag_repository.go:26-44`): the dictionary insert runs first, then the
/// `library_tag` rows; Go fails the whole `Add` if the second errors.
#[derive(Debug, Clone, PartialEq)]
pub struct AddChunkSql {
    /// `Insert(tag).Columns(id, tag_name, tag_value).Suffix("on conflict (id)
    /// do nothing")` — one VALUES row per tag of the chunk.
    pub tag_insert: Sql,
    /// `Insert(library_tag).Columns(tag_id, library_id, album_count,
    /// media_file_count).Suffix("on conflict (tag_id, library_id) do nothing")`
    /// — zeroed counts; `UpdateCounts` fills them.
    pub library_tag_insert: Sql,
}

/// `Add` (`tag_repository.go:23-45`): the chunked insert pairs. Idempotent via
/// both `ON CONFLICT … DO NOTHING` suffixes, so a re-scan re-adds safely.
#[must_use]
pub fn add_sql(library_id: i64, tags: &[Tag]) -> Vec<AddChunkSql> {
    tags.chunks(CHUNK_SIZE)
        .map(|chunk| {
            let mut tag_rows = String::new();
            let mut tag_args = Vec::new();
            let mut lib_rows = String::new();
            let mut lib_args = Vec::new();
            for (i, tag) in chunk.iter().enumerate() {
                if i > 0 {
                    tag_rows.push_str(", ");
                    lib_rows.push_str(", ");
                }
                tag_rows.push_str("(?, ?, ?)");
                tag_args.push(Value::from(tag.id.clone()));
                tag_args.push(Value::from(tag.tag_name.clone()));
                tag_args.push(Value::from(tag.tag_value.clone()));
                lib_rows.push_str("(?, ?, 0, 0)");
                lib_args.push(Value::from(tag.id.clone()));
                lib_args.push(Value::Integer(library_id));
            }
            AddChunkSql {
                tag_insert: (
                    format!(
                        "INSERT INTO {TAG_TABLE} (id, tag_name, tag_value) VALUES {tag_rows} on conflict (id) do nothing"
                    ),
                    tag_args,
                ),
                library_tag_insert: (
                    format!(
                        "INSERT INTO {LIBRARY_TAG_TABLE} (tag_id, library_id, album_count, media_file_count) VALUES {lib_rows} on conflict (tag_id, library_id) do nothing"
                    ),
                    lib_args,
                ),
            }
        })
        .collect()
}

/// `UpdateCounts` (`tag_repository.go:48-69`): one upsert per stats table
/// (`album`, `media_file`), Go's `fmt.Sprintf(template, table)` with `%[1]s`
/// = the table name. Only genres (`json_tree(tags, '$.genre')`) aggregate.
#[must_use]
pub fn update_counts_sql() -> Vec<Sql> {
    ["album", "media_file"]
        .iter()
        .map(|table| {
            (
                format!(
                    "\
INSERT INTO library_tag (tag_id, library_id, {table}_count)
SELECT jt.value as tag_id, {table}.library_id, count(distinct {table}.id) as {table}_count
FROM {table}
JOIN json_tree({table}.tags, '$.genre') as jt ON jt.atom IS NOT NULL AND jt.key = 'id'
JOIN tag ON tag.id = jt.value
GROUP BY jt.value, {table}.library_id
ON CONFLICT (tag_id, library_id)
DO UPDATE SET {table}_count = excluded.{table}_count;"
                ),
                Vec::new(),
            )
        })
        .collect()
}

/// `GetAll` (`tag_repository.go:71-74`) over the `baseTagRepository.newSelect`
/// shape (`sql_tags.go:180-199`): the name-filtered aggregate select —
/// `COALESCE(SUM(…), 0)` keeps tags with no `library_tag` rows at zero counts
/// (LEFT is why untagged rows keep contributing, `sql_tags.rs`
/// `library_filter_joins`), and the authenticated user's `user_library` join
/// scopes the SUM to accessible libraries (`sql_tags.go:206-210` — `None`
/// means no logged-in user).
pub fn get_all_sql(
    tag_name: &str,
    user_id: Option<&str>,
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let (joins, mut join_args) =
        library_filter_joins(user_id.map(std::string::ToString::to_string));
    let join_sql = joins
        .iter()
        .map(|join| format!(" LEFT JOIN {join}"))
        .collect::<String>();
    let mut sq = Repository::new(TAG_TABLE)
        .select([
            "tag.id",
            "tag.tag_name",
            "tag.tag_value",
            "COALESCE(SUM(library_tag.album_count), 0) as album_count",
            "COALESCE(SUM(library_tag.media_file_count), 0) as song_count",
        ])
        .where_eq("tag.tag_name", tag_name);
    if let Some(options) = options {
        sq = crate::sql_search::apply_pagination(sq, options);
        sq = crate::sql_search::apply_filters(sq, options);
    }
    let (mut sql, mut args) = sq.to_sql()?;
    // Splice the LEFT JOINs after FROM tag, before WHERE — squirrel's join
    // position. `library_tag`/`user_library` join identifiers are fixed
    // schema names (sql_tags.go:205-210), the only bound value is the user id.
    let from = format!("FROM {TAG_TABLE}");
    let Some(pos) = sql.find(&from) else {
        return Err(crate::sqlrepo::SqlError::NoTable);
    };
    let at = pos + from.len();
    sql.replace_range(at..at, &join_sql);
    args.splice(..0, join_args.drain(..));
    // GROUP BY sits before any LIMIT/OFFSET applyOptions appended
    // (squirrel renders .GroupBy before pagination).
    let group_by = " GROUP BY tag.id, tag.tag_name, tag.tag_value";
    match sql.find(" LIMIT") {
        Some(pos) => sql.replace_range(pos..pos, group_by),
        None => sql.push_str(group_by),
    }
    Ok((sql, args))
}

/// The private `purgeUnused` (`tag_repository.go:86-103`): delete dictionary
/// rows no `album`/`media_file` `tags` JSON references — the two `json_tree`
/// scans UNION'd, exactly Go's fragment (`:88-98`).
#[must_use]
pub fn purge_unused_sql() -> Sql {
    (
        format!(
            "DELETE FROM {TAG_TABLE} WHERE \
             id not in (select jt.value \
             from album left join json_tree(album.tags, '$') as jt \
             where atom is not null \
             and key = 'id' \
             UNION \
             select jt.value \
             from media_file left join json_tree(media_file.tags, '$') as jt \
             where atom is not null \
             and key = 'id')"
        ),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: &str, name: &str, value: &str) -> Tag {
        Tag {
            id: id.to_string(),
            tag_name: name.to_string(),
            tag_value: value.to_string(),
            album_count: 0,
            song_count: 0,
        }
    }

    #[test]
    fn add_emits_chunked_tag_and_library_tag_inserts() {
        // Arrange: Add's two statements for one chunk (tag_repository.go:26-44).
        let tags = vec![tag("t1", "genre", "Rock"), tag("t2", "genre", "Jazz")];
        // Act:
        let chunks = add_sql(3, &tags);
        // Assert: one chunk, dictionary insert first with 200-row cap respected.
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].tag_insert.0,
            "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?), (?, ?, ?) on conflict (id) do nothing"
        );
        assert_eq!(
            chunks[0].tag_insert.1,
            vec![
                Value::Text("t1".to_string()),
                Value::Text("genre".to_string()),
                Value::Text("Rock".to_string()),
                Value::Text("t2".to_string()),
                Value::Text("genre".to_string()),
                Value::Text("Jazz".to_string()),
            ]
        );
        assert_eq!(
            chunks[0].library_tag_insert.0,
            "INSERT INTO library_tag (tag_id, library_id, album_count, media_file_count) VALUES (?, ?, 0, 0), (?, ?, 0, 0) on conflict (tag_id, library_id) do nothing"
        );
        assert_eq!(
            chunks[0].library_tag_insert.1,
            vec![
                Value::Text("t1".to_string()),
                Value::Integer(3),
                Value::Text("t2".to_string()),
                Value::Integer(3),
            ]
        );
    }

    #[test]
    fn add_splits_at_the_200_row_chunk_boundary() {
        // Arrange: slices.Chunk(tags, 200) (tag_repository.go:25).
        let tags = (0..201)
            .map(|i| tag(&format!("t{i}"), "genre", "Rock"))
            .collect::<Vec<_>>();
        // Act:
        let chunks = add_sql(1, &tags);
        // Assert: 201 tags → a 200-row chunk plus a 1-row chunk.
        assert_eq!(chunks.len(), 2);
        let full = &chunks[0].tag_insert.0;
        let rows = full.matches("), ").count() + 1;
        assert_eq!(rows, CHUNK_SIZE);
        assert!(chunks[1].tag_insert.0.matches("?, ?, ?").count() == 1);
    }

    #[test]
    fn update_counts_renders_both_table_upserts_with_genre_only_json_path() {
        // Arrange/Act: UpdateCounts' template per table (tag_repository.go:50-66).
        let stmts = update_counts_sql();
        // Assert: album then media_file, only the $.genre path aggregates.
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].0.contains("FROM album"));
        assert!(stmts[0].0.contains("json_tree(album.tags, '$.genre')"));
        assert!(stmts[0]
            .0
            .contains("DO UPDATE SET album_count = excluded.album_count;"));
        assert!(stmts[1].0.contains("FROM media_file"));
        assert!(stmts[1]
            .0
            .contains("DO UPDATE SET media_file_count = excluded.media_file_count;"));
        for (_, args) in &stmts {
            assert!(args.is_empty());
        }
    }

    #[test]
    fn get_all_binds_the_tag_name_and_scopes_the_sums_to_the_user() {
        // Arrange: GetAll + baseTagRepository.newSelect (tag_repository.go:71-74,
        // sql_tags.go:180-199) with a logged-in user.
        // Act:
        let (sql, args) =
            get_all_sql("genre", Some("user1"), None).expect("the aggregate select materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT tag.id, tag.tag_name, tag.tag_value, \
             COALESCE(SUM(library_tag.album_count), 0) as album_count, \
             COALESCE(SUM(library_tag.media_file_count), 0) as song_count \
             FROM tag \
             LEFT JOIN library_tag on library_tag.tag_id = tag.id \
             LEFT JOIN user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ? \
             WHERE tag.tag_name = ? \
             GROUP BY tag.id, tag.tag_name, tag.tag_value"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Text("genre".to_string())
            ]
        );
    }

    #[test]
    fn get_all_without_user_skips_the_user_library_join_and_honors_pagination() {
        // Arrange: no logged-in user (sql_tags.go:206-210) with a page.
        let options = crate::sql_restful::QueryOptions {
            max: 10,
            offset: 20,
            ..crate::sql_restful::QueryOptions::default()
        };
        // Act:
        let (sql, args) = get_all_sql("genre", None, Some(&options))
            .expect("the unscoped aggregate select materializes");
        // Assert:
        assert!(!sql.contains("user_library"));
        assert!(sql.ends_with(" GROUP BY tag.id, tag.tag_name, tag.tag_value LIMIT ? OFFSET ?"));
        assert_eq!(
            args,
            vec![
                Value::Text("genre".to_string()),
                Value::Integer(10),
                Value::Integer(20),
            ]
        );
    }

    #[test]
    fn purge_unused_deletes_unreferenced_dictionary_rows_without_bound_args() {
        // Arrange/Act: purgeUnused's delete (tag_repository.go:88-102).
        let (sql, args) = purge_unused_sql();
        // Assert:
        assert!(sql.starts_with("DELETE FROM tag WHERE id not in ("));
        assert!(sql.contains("from album left join json_tree(album.tags, '$')"));
        assert!(sql.contains("from media_file left join json_tree(media_file.tags, '$')"));
        assert!(args.is_empty());
    }
}
