//! Port of `persistence/genre_repository.go` (51L) — the `genre` view of the
//! tag dictionary. Go builds it as `baseTagRepository` with a fixed
//! `tagFilter = model.TagGenre` (`genre_repository.go:18` → `sql_tags.go:161`),
//! so every statement is the base tag select (`sql_tags.go:197-228`) narrowed
//! to `tag.tag_name = "genre"`, plus the select's extra
//! `Columns("tag.tag_value as name")` (`:23`).
//!
//! As in the sibling `tag_repo.rs`, this port stops at the materialized
//! `(sql, args)` pairs (Go's `queryAll`/`queryOne` execute them in the store
//! layer), and the LEFT JOINs splice at squirrel's join position — only the
//! user id binds, join identifiers are fixed schema names.
//!
//! Deferred, with the reason: the base repository's REST `Count` (`sql_tags.go:202+`)
//! belongs to the tag repository's registry, and `Read`/`ReadAll` (`:42-49`)
//! are pass-throughs to `Get`/`GetAll`, ported as the two fns below.

use crate::sql_tags::{library_filter_joins, Sql};
use crate::sqlrepo::{Repository, SqlError};

/// `model.TagGenre` (`model/tag.go`): the fixed tag-name filter Go's genre
/// repository constructor passes to `newBaseTagRepository`. Schema constant,
/// never user text.
pub const GENRE_TAG_NAME: &str = "genre";

/// `model.Genre` (`model/genre.go:8-14`); counts are the base repository's
/// COALESCE'd aggregate columns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Genre {
    /// Dictionary id.
    pub id: String,
    /// `tag.tag_value` (aliased `as name` in the select).
    pub name: String,
    /// Aggregate album count.
    pub album_count: i64,
    /// Aggregate media-file count.
    pub song_count: i64,
}

/// `model.Genres`.
pub type GenreList = Vec<Genre>;

/// `GetAll` (`genre_repository.go:26-31`): `selectGenre` (`:22-24`) executed
/// through `queryAll`.
pub fn get_all_sql(user_id: Option<&str>) -> Result<Sql, SqlError> {
    select_genre(user_id, None)
}

/// `Get` (`:33-38`): the same select with `Where(Eq{"tag.id": id})` (`:34`).
pub fn get_sql(user_id: Option<&str>, id: String) -> Result<Sql, SqlError> {
    select_genre(user_id, Some(&id))
}

/// `selectGenre` composed on `baseTagRepository.newSelect`
/// (`sql_tags.go:197-228`): the aggregate columns, the fixed `genre` name
/// filter, squirrel `GroupBy` (`sqlrepo.group_by`, rendered WHERE→GROUP BY→
/// ORDER/LIMIT), and the `applyLibraryFiltering` LEFT JOINs from
/// [`library_filter_joins`] spliced after `FROM tag` (`sql_tags.go:205-210`).
fn select_genre(user_id: Option<&str>, id: Option<&str>) -> Result<Sql, SqlError> {
    let (joins, join_args) = library_filter_joins(user_id.map(|u| u.to_string()));
    let mut sq = Repository::new("tag")
        .select(vec![
            "tag.id",
            "tag.tag_name",
            "tag.tag_value",
            "COALESCE(SUM(library_tag.album_count), 0) as album_count",
            "COALESCE(SUM(library_tag.media_file_count), 0) as song_count",
            "tag.tag_value as name",
        ])
        .where_eq("tag.tag_name", GENRE_TAG_NAME);
    if let Some(id) = id {
        sq = sq.where_eq("tag.id", id);
    }
    sq = sq.group_by(vec!["tag.id", "tag.tag_name", "tag.tag_value"]);
    let (mut sql, mut args) = sq.to_sql()?;
    let from = "FROM tag";
    let Some(pos) = sql.find(from) else {
        return Err(SqlError::NoTable);
    };
    let at = pos + from.len();
    let join_sql = joins
        .iter()
        .map(|j| format!(" LEFT JOIN {j}"))
        .collect::<String>();
    sql.replace_range(at..at, &join_sql);
    // squirrel binds JOIN args before WHERE args.
    args.splice(0..0, join_args.iter().cloned());
    Ok((sql, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genre_select_narrows_the_tag_dictionary_and_aggregates() {
        // Arrange: unauthenticated GetAll (genre_repository.go:26-31).
        // Act:
        let (sql, args) = get_all_sql(None).expect("the aggregate select materializes");
        // Assert: fixed-name filter, library_tag LEFT JOIN, base GROUP BY,
        // and the selectGenre alias (sql_tags.go:197-228 + :23).
        assert!(sql.contains("WHERE tag.tag_name = ?"));
        assert!(sql.contains(" LEFT JOIN library_tag on library_tag.tag_id = tag.id"));
        assert!(sql.contains("GROUP BY tag.id, tag.tag_name, tag.tag_value"));
        assert!(sql.contains("tag.tag_value as name"));
        assert_eq!(args, vec![crate::sqlrepo::Value::Text("genre".to_string())]);
    }

    #[test]
    fn authenticated_get_scopes_counts_through_user_library() {
        // Arrange: Get for a logged user (applyLibraryFiltering adds the
        // user_library join, sql_tags.go:207-209).
        // Act:
        let (sql, args) =
            get_sql(Some("usr-1"), "tag-9".to_string()).expect("the id filter materializes");
        // Assert: the join id binds before the two WHERE values.
        assert!(sql.contains(" LEFT JOIN user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ?"));
        assert!(sql.contains("WHERE tag.tag_name = ? AND tag.id = ?"));
        assert_eq!(
            args,
            vec![
                crate::sqlrepo::Value::Text("usr-1".to_string()),
                crate::sqlrepo::Value::Text("genre".to_string()),
                crate::sqlrepo::Value::Text("tag-9".to_string())
            ]
        );
    }
}
