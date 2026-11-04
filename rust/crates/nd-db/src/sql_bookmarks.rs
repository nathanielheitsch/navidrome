//! Port of `persistence/sql_bookmarks.go` (166L) — bookmark SQL shaping only.
//!
//! Every Go method is a statement builder handed to `executeSQL`
//! (`sql_bookmarks.go:40-`); execution/logging/`WithTx` stay with the
//! `store.rs`/later slices, so the ports here stop at `(sql, args)` pairs,
//! the shape `toSQL()` (`sql_base_repository.go:358-372`) hands dbx. Two
//! Go-side composites are intentionally not re-composed:
//! `withAnnotation` (`sql_annotations.go:63-77`) and `applyLibraryFilter`
//! belong to their own modules/slices; `GetBookmarks` notes where they slot in.
//!
//! Safety deviation from Go: `withBookmark` interpolates the user id into the
//! join string (`sql_bookmarks.go:23-26`, `bookmark.user_id = '<id>'`); the
//! port binds it as a `?` parameter per the crate rule in `sqlrepo.rs`
//! (values are always parameterized). Go also renders `SetMap` SET columns
//! in map-iteration order (nondeterministic); the ports pin the map
//! literals' declaration order so the SQL is stable.

use crate::sqlrepo::Value;

/// `bookmarkTable` (`sql_bookmarks.go:13`).
pub const BOOKMARK_TABLE: &str = "bookmark";

/// `invalidUserId` (`sql_base_repository.go:51`): the "no logged-in user"
/// sentinel `withBookmark` checks to skip the join (`sql_bookmarks.go:19-21`).
pub const INVALID_USER_ID: &str = "-1";

/// One materialized statement or clause: `(sql, args)`, the
/// `toSQL()` (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `withBookmark` (`sql_bookmarks.go:17-29`): the `LEFT JOIN` fragment onto the
/// logged-in user's bookmark for `id_field`, plus the args it binds.
/// `None` when `user_id` is [`INVALID_USER_ID`] (Go returns the query as-is).
///
/// Squirrel emits ` LEFT JOIN bookmark ON (<on>)`; the user id is a bound
/// parameter here instead of Go's inline-quoted literal (`:23-26`).
#[must_use]
pub fn with_bookmark(user_id: &str, id_field: &str) -> Option<Sql> {
    if user_id == INVALID_USER_ID {
        return None;
    }
    Some((
        format!("LEFT JOIN bookmark ON (bookmark.item_id = {id_field} AND bookmark.user_id = ?)"),
        vec![Value::from(user_id)],
    ))
}

/// The column `withBookmark` appends to the select list
/// (`sql_bookmarks.go:28`, `Columns("coalesce(position, 0) as bookmark_position")`).
pub const BOOKMARK_POSITION_COLUMN: &str = "coalesce(position, 0) as bookmark_position";

/// `bmkID` (`sql_bookmarks.go:31-38`): the AND-joined bookmark identity filter.
/// Squirrel renders a one-element `Eq` slice as `= ?`, several as `IN (?, …)`,
/// and an empty one as `1 = 0` — all three shapes are pinned below.
#[must_use]
pub fn bmk_id(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let mut args = vec![Value::from(user_id), Value::from(table)];
    let item_clause = match item_ids {
        [] => "1 = 0".to_string(),
        [one] => {
            args.push(Value::from(*one));
            "bookmark.item_id = ?".to_string()
        }
        many => {
            let placeholders = vec!["?"; many.len()].join(", ");
            args.extend(many.iter().map(|id| Value::from(*id)));
            format!("bookmark.item_id IN ({placeholders})")
        }
    };
    (
        format!("bookmark.user_id = ? AND bookmark.item_type = ? AND {item_clause}"),
        args,
    )
}

/// The upsert payload of `bmkUpsert` (`sql_bookmarks.go:44-49,58-61`): the
/// `values` map literals of both legs — the shared SET columns plus the
/// INSERT-only identity/stamp columns — keep the two statements from
/// threading eight loose scalars through the port.
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkUpsert<'a> {
    /// `bookmark.comment`.
    pub comment: &'a str,
    /// `bookmark.position`.
    pub position: i64,
    /// `bookmark.created_at` (INSERT leg only).
    pub created_at: &'a str,
    /// `bookmark.updated_at`.
    pub updated_at: &'a str,
    /// `bookmark.changed_by` — `request.ClientFrom(ctx)`.
    pub changed_by: &'a str,
}

/// The `UPDATE` leg of `bmkUpsert` (`sql_bookmarks.go:40-49`): rewrite the
/// existing bookmark's comment/position/stamp. SET order is the `values` map
/// literal's declaration order (`:44-49`); the stamp/client values come from
/// [`BookmarkUpsert`], what the execution slice draws from
/// `time.Now()`/`request.ClientFrom(ctx)`.
#[must_use]
pub fn bmk_update_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    upsert: &BookmarkUpsert<'_>,
) -> Sql {
    let set = "comment = ?, position = ?, updated_at = ?, changed_by = ?";
    let (where_sql, mut args) = bmk_id(table, user_id, &[item_id]);
    let set_args = vec![
        Value::from(upsert.comment),
        Value::Integer(upsert.position),
        Value::from(upsert.updated_at),
        Value::from(upsert.changed_by),
    ];
    args.splice(0..0, set_args);
    (
        format!("UPDATE {BOOKMARK_TABLE} SET {set} WHERE {where_sql}"),
        args,
    )
}

/// The `INSERT` fallback of `bmkUpsert` (`sql_bookmarks.go:50-58`), used when
/// the UPDATE matched no rows: create the bookmark with the full identity
/// (`user_id`, `item_type`, `item_id`, `created_at`).
#[must_use]
pub fn bmk_insert_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    upsert: &BookmarkUpsert<'_>,
) -> Sql {
    (
        format!(
            "INSERT INTO {BOOKMARK_TABLE} \
             (user_id, item_type, item_id, comment, position, created_at, updated_at, changed_by) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            Value::from(user_id),
            Value::from(table),
            Value::from(item_id),
            Value::from(upsert.comment),
            Value::Integer(upsert.position),
            Value::from(upsert.created_at),
            Value::from(upsert.updated_at),
            Value::from(upsert.changed_by),
        ],
    )
}

/// `DeleteBookmark` (`sql_bookmarks.go:79-85`): `DELETE FROM bookmark WHERE bmkID(id)`.
#[must_use]
pub fn delete_bookmark_sql(table: &str, user_id: &str, item_id: &str) -> Sql {
    let (where_sql, args) = bmk_id(table, user_id, &[item_id]);
    (
        format!("DELETE FROM {BOOKMARK_TABLE} WHERE {where_sql}"),
        args,
    )
}

/// `GetBookmarks`' first leg (`sql_bookmarks.go:96-105`): the table's rows
/// carrying a bookmark for the user. `NotEq{item_id: nil}` renders
/// `bookmark.item_id IS NOT NULL` (`:101`). The `withAnnotation`
/// (`sql_annotations.go:63-77`) join/columns and `applyLibraryFilter` are
/// owned by their own slices and are not composed here.
#[must_use]
pub fn get_bookmarks_media_files_sql(table: &str, user_id: &str) -> Sql {
    let id_field = format!("{table}.id");
    let mut sql = format!("SELECT {table}.* FROM {table}");
    let mut args = Vec::new();
    // Go appends the coalesce column only on the joined path (:28).
    if let Some((join, join_args)) = with_bookmark(user_id, &id_field) {
        sql = format!("SELECT {table}.*, {BOOKMARK_POSITION_COLUMN} FROM {table} {join}");
        args = join_args;
    }
    sql.push_str(" WHERE bookmark.item_id IS NOT NULL");
    (sql, args)
}

/// `GetBookmarks`' second leg (`sql_bookmarks.go:120-122`):
/// `Select("*").From(bookmark).Where(bmkID(ids...))` over the bookmark rows.
#[must_use]
pub fn get_bookmarks_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = bmk_id(table, user_id, item_ids);
    (
        format!("SELECT * FROM {BOOKMARK_TABLE} WHERE {where_sql}"),
        args,
    )
}

/// `reassignBookmark` (`sql_bookmarks.go:159-163`): `update or ignore` moves
/// the table's bookmarks from `prev_id` onto `new_id`.
#[must_use]
pub fn reassign_bookmark_sql(table: &str, prev_id: &str, new_id: &str) -> Sql {
    (
        format!(
            "update or ignore {BOOKMARK_TABLE} set item_id = ? \
             where item_type = ? and item_id = ?"
        ),
        vec![
            Value::from(new_id),
            Value::from(table),
            Value::from(prev_id),
        ],
    )
}

/// `cleanBookmarks` (`sql_bookmarks.go:165-177`): drop bookmarks of this table's
/// type whose item no longer exists. Go stacks two `.Where()` calls —
/// `Eq{item_type}` then the raw `item_id not in (select id from …)` fragment.
#[must_use]
pub fn clean_bookmarks_sql(table: &str) -> Sql {
    (
        format!(
            "DELETE FROM {BOOKMARK_TABLE} \
             WHERE item_type = ? AND item_id not in (select id from {table})"
        ),
        vec![Value::from(table)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "media_file";
    const USER: &str = "user1";

    #[test]
    fn with_bookmark_renders_join_and_skips_invalid_user() {
        // Arrange: withBookmark's join for a logged-in user (sql_bookmarks.go:17-29).
        // Act:
        let joined = with_bookmark(USER, "media_file.id");
        let anonymous = with_bookmark(INVALID_USER_ID, "media_file.id");
        // Assert:
        let (sql, args) = joined.expect("logged-in user gets the bookmark join");
        assert_eq!(
            sql,
            "LEFT JOIN bookmark ON (bookmark.item_id = media_file.id AND bookmark.user_id = ?)"
        );
        assert_eq!(args, vec![Value::Text(USER.to_string())]);
        assert!(anonymous.is_none());
    }

    #[test]
    fn bmk_id_renders_eq_in_and_empty_shapes() {
        // Arrange: bmkID's three squirrel Eq-slice shapes (sql_bookmarks.go:31-38).
        // Act / Assert: single id → `= ?`.
        let (sql, args) = bmk_id(TABLE, USER, &["mf1"]);
        assert_eq!(
            sql,
            "bookmark.user_id = ? AND bookmark.item_type = ? AND bookmark.item_id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
            ]
        );
        // Act / Assert: several ids → IN list.
        let (sql, args) = bmk_id(TABLE, USER, &["mf1", "mf2"]);
        assert_eq!(
            sql,
            "bookmark.user_id = ? AND bookmark.item_type = ? AND bookmark.item_id IN (?, ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
                Value::Text("mf2".to_string()),
            ]
        );
        // Act / Assert: no ids → squirrel's empty-Eq 1 = 0.
        let (sql, args) = bmk_id(TABLE, USER, &[]);
        assert_eq!(
            sql,
            "bookmark.user_id = ? AND bookmark.item_type = ? AND 1 = 0"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string())
            ]
        );
    }

    #[test]
    fn upsert_renders_update_then_fallback_insert() {
        // Arrange: bmkUpsert's two legs (sql_bookmarks.go:40-58).
        // Act:
        let upsert = BookmarkUpsert {
            comment: "checkpoint",
            position: 42,
            created_at: "2024-01-02T03:04:05Z",
            updated_at: "2024-01-02T03:04:06Z",
            changed_by: "player",
        };
        let (upd_sql, upd_args) = bmk_update_sql(TABLE, USER, "mf1", &upsert);
        let (ins_sql, ins_args) = bmk_insert_sql(TABLE, USER, "mf1", &upsert);
        // Assert:
        assert_eq!(
            upd_sql,
            "UPDATE bookmark SET comment = ?, position = ?, updated_at = ?, changed_by = ? \
             WHERE bookmark.user_id = ? AND bookmark.item_type = ? AND bookmark.item_id = ?"
        );
        assert_eq!(
            upd_args,
            vec![
                Value::Text("checkpoint".to_string()),
                Value::Integer(42),
                Value::Text("2024-01-02T03:04:06Z".to_string()),
                Value::Text("player".to_string()),
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
            ]
        );
        assert_eq!(
            ins_sql,
            "INSERT INTO bookmark (user_id, item_type, item_id, comment, position, \
             created_at, updated_at, changed_by) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        );
        assert_eq!(
            ins_args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
                Value::Text("checkpoint".to_string()),
                Value::Integer(42),
                Value::Text("2024-01-02T03:04:05Z".to_string()),
                Value::Text("2024-01-02T03:04:06Z".to_string()),
                Value::Text("player".to_string()),
            ]
        );
    }

    #[test]
    fn delete_bookmark_renders_bmk_id_where() {
        // Arrange: DeleteBookmark (sql_bookmarks.go:79-85).
        // Act:
        let (sql, args) = delete_bookmark_sql(TABLE, USER, "mf1");
        // Assert:
        assert_eq!(
            sql,
            "DELETE FROM bookmark WHERE bookmark.user_id = ? \
             AND bookmark.item_type = ? AND bookmark.item_id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
            ]
        );
    }

    #[test]
    fn get_bookmarks_media_files_renders_join_column_and_not_null() {
        // Arrange: GetBookmarks' first leg (sql_bookmarks.go:96-105).
        // Act:
        let (sql, args) = get_bookmarks_media_files_sql(TABLE, USER);
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.*, coalesce(position, 0) as bookmark_position \
             FROM media_file LEFT JOIN bookmark ON (bookmark.item_id = media_file.id \
             AND bookmark.user_id = ?) WHERE bookmark.item_id IS NOT NULL"
        );
        assert_eq!(args, vec![Value::Text(USER.to_string())]);
        // Act: the anonymous-user path keeps the bare select (sql_bookmarks.go:19-21).
        let (sql, args) = get_bookmarks_media_files_sql(TABLE, INVALID_USER_ID);
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.* FROM media_file WHERE bookmark.item_id IS NOT NULL"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn get_bookmarks_renders_star_select_over_bmk_id() {
        // Arrange: GetBookmarks' second leg (sql_bookmarks.go:120-122).
        // Act:
        let (sql, args) = get_bookmarks_sql(TABLE, USER, &["mf1", "mf2"]);
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM bookmark WHERE bookmark.user_id = ? \
             AND bookmark.item_type = ? AND bookmark.item_id IN (?, ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
                Value::Text("mf2".to_string()),
            ]
        );
    }

    #[test]
    fn reassign_and_clean_render_verbatim_go_shapes() {
        // Arrange: reassignBookmark (sql_bookmarks.go:159-163) and cleanBookmarks (:165-177).
        // Act:
        let (reassign_sql, reassign_args) = reassign_bookmark_sql(TABLE, "old-id", "new-id");
        let (clean_sql, clean_args) = clean_bookmarks_sql(TABLE);
        // Assert:
        assert_eq!(
            reassign_sql,
            "update or ignore bookmark set item_id = ? \
             where item_type = ? and item_id = ?"
        );
        assert_eq!(
            reassign_args,
            vec![
                Value::Text("new-id".to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("old-id".to_string()),
            ]
        );
        assert_eq!(
            clean_sql,
            "DELETE FROM bookmark WHERE item_type = ? \
             AND item_id not in (select id from media_file)"
        );
        assert_eq!(clean_args, vec![Value::Text(TABLE.to_string())]);
    }
}
