//! Port of `persistence/sql_annotations.go` (209L) — the annotation
//! repository's SQL shaping only.
//!
//! Every Go method builds an `Update`/`Insert`/`Delete` squirrel statement and
//! hands it to `executeSQL`, which materializes it through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`. Execution, `WithTx` and
//! logging stay with `store.rs`/later slices, so the port stops at the same
//! `(sql, args)` pairs — the WithTx-only statements (`annUpsert`, `SetRating`'s
//! follow-up `updateAvgRating` and `IncPlayCount` run inside one transaction,
//! Go `:98-209`) are shaped here and composed there.
//!
//! `Repository::to_sql` (`sqlrepo.rs`) renders only `SELECT`, so the
//! `UPDATE`/`INSERT`/`DELETE` statements are rendered here directly, while their
//! `WHERE` clause reuses [`ann_id`] — the same rendering the builder's
//! conditions produce — and the `SELECT` accessors of `sql_base_repository.go`
//! ([`get_annotations_sql`], [`annotation_exists_sql`],
//! [`annotation_count_sql`]) build with `Repository` and call `to_sql()`.
//!
//! Not ported (different slices): `annotationColumns`/`annotationColumnRE`
//! (`:15-35`) and `filtersNeedAnnotation` (`:37-42`) reflect over
//! `model.Annotations` via `structs.Map` and over a live squirrel
//! `SelectBuilder`. nd-model has no `Annotations` type yet and this crate's
//! builder never holds annotation columns, so [`with_annotation`] hands back the
//! join/columns it adds and the caller decides whether to keep them;
//! `sql_restful.rs`'s `ANNOTATION_FIELDS` already pins the same column set for
//! the REST filter whitelist, so the regex's column list is not re-derived here.
//!
//! Safety rule carried from `sqlrepo.rs`: table/column identifiers come only
//! from the repositories' registered names, never user text; values are always
//! parameterized. Go's `SetMap` INSERT renders columns in map-iteration order
//! (`sql_annotations.go:100`/`147`); the ports pin the map literals' declaration
//! order so the SQL is stable.

use crate::sqlrepo::Repository;
use crate::sqlrepo::Value;

/// `annotationTable` (`sql_annotations.go:13`).
pub const ANNOTATION_TABLE: &str = "annotation";

/// `invalidUserId` (`sql_base_repository.go:51`): the "no logged-in user"
/// sentinel [`with_annotation`] checks to skip the join (`:63-65`).
pub const INVALID_USER_ID: &str = "-1";

/// `conf.Server.AlbumPlayCountMode == consts.AlbumPlayCountModeNormalized`
/// (`sql_annotations.go:71`); the normalized-album play-count column is only
/// selected when the config selects that mode.
pub const ALBUM_PLAY_COUNT_MODE_NORMALIZED: &str = "normalized";

/// The three identity keys `annUpsert` adds to its `values` map before
/// `SetMap` renders the INSERT (`sql_annotations.go:105-107`), in the order
/// squirrel emits them.
pub const ANNOTATION_IDENTITY_COLUMNS: &str = "user_id, item_type, item_id";

/// A materialized statement or clause: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// What [`with_annotation`] appends to a `SELECT`: squirrel's string overload of
/// `.LeftJoin` (`sql_annotations.go:66-70`) carries the whole `on (...)` text,
/// so the port keeps it as one fragment with its two `?` args bound.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationJoin {
    /// ` LEFT JOIN annotation ON (annotation.item_id = <id_field> AND …)` —
    /// the leading space is squirrel's separator.
    join: String,
    /// The select-list columns `withAnnotation` appends (`:67-76`), in
    /// squirrel's `.Columns()` declaration order.
    columns: Vec<String>,
    /// The join's bound args, in placeholder order: `item_type` (the
    /// repository's table) then the logged-in `user_id`.
    args: Vec<Value>,
}

/// `withAnnotation` (`sql_annotations.go:63-77`): the `LEFT JOIN` onto this
/// logged-in user's annotation row for `id_field` plus the annotation columns.
/// `None` for [`INVALID_USER_ID`]: Go returns the query untouched then
/// (`:63-65`), so neither the join nor the columns are added.
///
/// `normalized_album_play_count` is
/// `conf.Server.AlbumPlayCountMode == ALBUM_PLAY_COUNT_MODE_NORMALIZED`; the
/// normalized form only replaces the play-count column for the `album` table
/// (`:71-75`), every other table coalesces plainly. Star and rating are
/// coalesced to 0 so an unannotated row reads unrated rather than NULL — that
/// default is what keeps the `starred`/`rating` filters total.
#[must_use]
pub fn with_annotation(
    table: &str,
    user_id: &str,
    id_field: &str,
    normalized_album_play_count: bool,
) -> Option<AnnotationJoin> {
    if user_id == INVALID_USER_ID {
        return None;
    }
    let join = format!(
        " LEFT JOIN {ANNOTATION_TABLE} ON \
         ({ANNOTATION_TABLE}.item_id = {id_field} \
         AND {ANNOTATION_TABLE}.item_type = ? \
         AND {ANNOTATION_TABLE}.user_id = ?)"
    );
    let is_album = table == "album";
    let play_count_column = if is_album && normalized_album_play_count {
        format!(
                "round(coalesce(round(cast(play_count as float) / coalesce({table}.song_count, 1), 1), 0)) as play_count"
            )
    } else {
        "coalesce(play_count, 0) as play_count".to_string()
    };
    Some(AnnotationJoin {
        join,
        columns: vec![
            "coalesce(starred, 0) as starred".to_string(),
            "coalesce(rating, 0) as rating".to_string(),
            "starred_at".to_string(),
            "play_date".to_string(),
            "rated_at".to_string(),
            play_count_column,
            format!("{table}.average_rating"),
        ],
        args: vec![Value::from(table), Value::from(user_id)],
    })
}

/// `annotationBoolFilter` (`sql_annotations.go:80-88`): the starred/has_rating
/// filter on an annotation column. Both branches coalesce the column to 0 before
/// comparing, so a row with no annotation match fails `starred` and satisfies
/// `has_rating = false`. Go lowercases the filter value, so `"TRUE"` matches
/// `"true"` does; any other value takes the `= 0` branch. The fragment is a
/// squirrel `Expr`: raw SQL, no bound args.
#[must_use]
pub fn annotation_bool_filter(field: &str, value: &str) -> String {
    if value.to_lowercase() == "true" {
        format!("COALESCE({field}, 0) > 0")
    } else {
        format!("COALESCE({field}, 0) = 0")
    }
}

/// `annId` (`sql_annotations.go:90-96`): this user's annotation rows for
/// `item_ids`. Squirrel renders a one-element id slice as `= ?`, several as
/// `IN (?, …)` and an empty one as `1 = 0` — all three shapes are pinned below.
#[must_use]
pub fn ann_id(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let mut args = vec![Value::from(user_id), Value::from(table)];
    let item_clause = match item_ids {
        [] => "1 = 0".to_string(),
        [one] => {
            args.push(Value::from(*one));
            format!("{ANNOTATION_TABLE}.item_id = ?")
        }
        many => {
            let placeholders = vec!["?"; many.len()].join(", ");
            args.extend(many.iter().map(|id| Value::from(*id)));
            format!("{ANNOTATION_TABLE}.item_id IN ({placeholders})")
        }
    };
    (
        format!(
            "{ANNOTATION_TABLE}.user_id = ? AND {ANNOTATION_TABLE}.item_type = ? AND {item_clause}"
        ),
        args,
    )
}

/// Joins two arg lists in placeholder order (SET args before WHERE args, which
/// is how squirrel renders `Update(...).Where(...).Set(...)`).
#[must_use]
pub fn concat_args(leading: Vec<Value>, trailing: Vec<Value>) -> Vec<Value> {
    let mut args = leading;
    args.extend(trailing);
    args
}

/// The `UPDATE` leg of `annUpsert` (`sql_annotations.go:98-101`): rewrite the
/// existing annotation rows. `set` is the SET column list with one `?` per
/// bound value, in the map literal's declaration order.
#[must_use]
pub fn ann_update_sql(
    table: &str,
    user_id: &str,
    item_ids: &[&str],
    set: &str,
    set_args: Vec<Value>,
) -> Sql {
    let (where_sql, where_args) = ann_id(table, user_id, item_ids);
    (
        format!("UPDATE {ANNOTATION_TABLE} SET {set} WHERE {where_sql}"),
        concat_args(set_args, where_args),
    )
}

/// The `INSERT` fallback of `annUpsert` (`sql_annotations.go:102-112`), run once
/// per id when the UPDATE matched no rows (`c == 0 || err == sql.ErrNoRows`).
/// `columns` are the caller's value-map columns in that map's declaration order,
/// appended to [`ANNOTATION_IDENTITY_COLUMNS`].
#[must_use]
pub fn ann_insert_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    columns: &[&str],
    value_args: Vec<Value>,
) -> Sql {
    let names_text = columns.join(", ");
    let names = format!("{ANNOTATION_IDENTITY_COLUMNS}, {names_text}");
    let placeholders = vec!["?"; columns.len() + 3].join(", ");
    let identity_args = vec![
        Value::from(user_id),
        Value::from(table),
        Value::from(item_id),
    ];
    (
        format!("INSERT INTO {ANNOTATION_TABLE} ({names}) VALUES ({placeholders})"),
        concat_args(identity_args, value_args),
    )
}

/// `SetStar` (`sql_annotations.go:136-138`): the UPDATE leg for `starred`/
/// `starred_at` over every id.
#[must_use]
pub fn set_star_sql(
    table: &str,
    user_id: &str,
    starred: bool,
    item_ids: &[&str],
    starred_at: &str,
) -> Sql {
    let set_args = vec![Value::Bool(starred), Value::from(starred_at)];
    ann_update_sql(
        table,
        user_id,
        item_ids,
        "starred = ?, starred_at = ?",
        set_args,
    )
}

/// The INSERT fallback `annUpsert` runs when `SetStar`'s UPDATE matched no rows
/// (`sql_annotations.go:102-112` + `SetStar`'s map literal, `:142`).
#[must_use]
pub fn set_star_insert_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    starred: bool,
    starred_at: &str,
) -> Sql {
    let value_args = vec![Value::Bool(starred), Value::from(starred_at)];
    ann_insert_sql(
        table,
        user_id,
        item_id,
        &["starred", "starred_at"],
        value_args,
    )
}

/// `SetRating`'s first statement (`sql_annotations.go:142-151`): the UPDATE leg
/// for `rating`/`rated_at` over one id. Squirrel gets the rating as `SetRating`
/// received it — `server/subsonic/media_annotation.go:57` parses the request
/// value and this repository adds no clamp; only the read-side
/// `coalesce(…, 0)` defaults exist (`:68-69`, `:157-160`).
#[must_use]
pub fn set_rating_sql(
    table: &str,
    user_id: &str,
    rating: i64,
    item_id: &str,
    rated_at: &str,
) -> Sql {
    let set_args = vec![Value::Integer(rating), Value::from(rated_at)];
    ann_update_sql(
        table,
        user_id,
        &[item_id],
        "rating = ?, rated_at = ?",
        set_args,
    )
}

/// The INSERT fallback `annUpsert` runs when `SetRating`'s UPDATE matched no
/// rows (`sql_annotations.go:102-112` + `SetRating`'s map literal, `:149`).
#[must_use]
pub fn set_rating_insert_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    rating: i64,
    rated_at: &str,
) -> Sql {
    let value_args = vec![Value::Integer(rating), Value::from(rated_at)];
    ann_insert_sql(table, user_id, item_id, &["rating", "rated_at"], value_args)
}

/// `updateAvgRating` (`sql_annotations.go:153-162`): recompute the base table's
/// cached `average_rating` from the ratings still on the annotation table. The
/// subquery is squirrel `Expr`, so its two `?` args come before the outer
/// `WHERE`'s `id` arg, and `rating > 0` drops unrated rows from the average —
/// the reason a zero rating never lowers an album's average.
#[must_use]
pub fn update_avg_rating_sql(table: &str, item_id: &str) -> Sql {
    let subquery_args = vec![Value::from(item_id), Value::from(table)];
    let where_args = vec![Value::from(item_id)];
    (
        format!("UPDATE {table} SET average_rating = ? WHERE id = ?"),
        concat_args(subquery_args, where_args),
    )
}

/// The `coalesce((select …), 0)` squirrel `Expr` [`update_avg_rating_sql`] binds
/// as its SET value (`sql_annotations.go:157-160`), exposed separately so the
/// tests pin the raw SQL apart from the arg order.
#[must_use]
pub fn average_rating_expr_sql() -> String {
    "coalesce((select round(avg(rating), 2) from annotation where item_id = ? and item_type = ? and rating > 0), 0)".to_string()
}

/// `IncPlayCount`'s UPDATE leg (`sql_annotations.go:164-170`): the playcount
/// increment — `play_count+1` and `max(ifnull(play_date,''), ?)` are squirrel
/// `Expr`s with the timestamp bound. The `ifnull(...,'')` keeps a NULL
/// `play_date` from losing the comparison, so the newer of the two dates wins.
#[must_use]
pub fn inc_play_count_sql(table: &str, user_id: &str, item_id: &str, played_at: &str) -> Sql {
    let (where_sql, where_args) = ann_id(table, user_id, &[item_id]);
    (
        format!(
            "UPDATE {ANNOTATION_TABLE} SET play_count = play_count+1, play_date = max(ifnull(play_date,''), ?) WHERE {where_sql}"
        ),
        concat_args(vec![Value::from(played_at)], where_args),
    )
}

/// `IncPlayCount`'s INSERT fallback (`sql_annotations.go:171-181`): the first
/// play for this user/id, so the count starts at 1.
#[must_use]
pub fn inc_play_count_insert_sql(
    table: &str,
    user_id: &str,
    item_id: &str,
    played_at: &str,
) -> Sql {
    let value_args = vec![Value::Integer(1), Value::from(played_at)];
    ann_insert_sql(
        table,
        user_id,
        item_id,
        &["play_count", "play_date"],
        value_args,
    )
}

/// `ReassignAnnotation` (`sql_annotations.go:184-196`): move this table-type's
/// annotation rows from `prev_id` to `new_id`. `update or ignore` (Go's comment,
/// `:193`) keeps `new_id`'s own row where a user annotated both ids instead of
/// aborting the statement. The caller skips the call when either id is empty or
/// they are equal (`:184-186`), then re-runs [`update_avg_rating_sql`] on
/// `new_id` because the moved rows change its rating population.
#[must_use]
pub fn reassign_annotation_sql(table: &str, prev_id: &str, new_id: &str) -> Sql {
    (
        format!("update or ignore {ANNOTATION_TABLE} set item_id = ? where item_type = ? and item_id = ?"),
        vec![Value::from(new_id), Value::from(table), Value::from(prev_id)],
    )
}

/// `cleanAnnotations` (`sql_annotations.go:198-209`): drop this table-type's
/// annotations whose item no longer exists. Go stacks `.Where(Eq{item_type})` and
/// a raw `item_id not in (select id from <table>)` fragment, joined with AND.
#[must_use]
pub fn clean_annotations_sql(table: &str) -> Sql {
    (
        format!("DELETE FROM {ANNOTATION_TABLE} WHERE item_type = ? AND item_id not in (select id from {table})"),
        vec![Value::from(table)],
    )
}

/// The annotation repository's `Select().From(annotation).Where(annId(...))`:
/// the SELECT over the annotation rows the lazy `annotation()` repository
/// (`persistence.go:17-40`) serves.
#[must_use]
pub fn get_annotations_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = ann_id(table, user_id, item_ids);
    (
        format!("SELECT * FROM {ANNOTATION_TABLE} WHERE {where_sql}"),
        args,
    )
}

/// `sqlRepository.exists()` (`sql_base_repository.go:488-499`) over the
/// annotation table: `Select("count(*) as exist").From(annotation).Where(cond)`.
#[must_use]
pub fn annotation_exists_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = ann_id(table, user_id, item_ids);
    let repo = Repository::new(ANNOTATION_TABLE)
        .select(["count(*) as exist"])
        .where_raw(where_sql, args);
    materialize(repo)
}

/// `sqlRepository.count()` (`sql_base_repository.go:565-575`) over the
/// annotation rows: `RemoveColumns().Columns("count(distinct <table>.id) as count")`.
/// The DISTINCT aggregate is what keeps a page's total right when the
/// annotation JOIN could otherwise multiply rows.
#[must_use]
pub fn annotation_count_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = ann_id(table, user_id, item_ids);
    let repo = Repository::new(ANNOTATION_TABLE)
        .where_raw(where_sql, args)
        .count(ANNOTATION_TABLE);
    materialize(repo)
}

/// Renders a builder to its `(sql, args)` pair. The builders built here always
/// have a table and never set an order-by list, so sqlrepo's `SqlError` cannot
/// occur; the panic names the caller bug if it ever does.
#[must_use]
fn materialize(repo: Repository) -> Sql {
    repo.to_sql()
        .expect("annotation query builders always render")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "media_file";
    const USER: &str = "user1";
    const STAMP: &str = "2024-01-02 03:04:05+00:00";

    #[test]
    fn with_annotation_renders_join_star_rating_defaults_and_skips_invalid_user() {
        // Arrange: withAnnotation for a logged-in user on the media_file table,
        // plain play-count mode (sql_annotations.go:63-77).
        // Act:
        let joined = with_annotation(TABLE, USER, "media_file.id", false);
        let anonymous = with_annotation(TABLE, INVALID_USER_ID, "media_file.id", false);
        // Assert: the join binds item_type then user_id, and the star/rating
        // columns carry the coalesce-0 defaults.
        let join = joined.expect("logged-in user gets the annotation join");
        assert_eq!(
            join.join,
            " LEFT JOIN annotation ON (annotation.item_id = media_file.id AND annotation.item_type = ? AND annotation.user_id = ?)"
        );
        assert_eq!(
            join.args,
            vec![
                Value::Text(TABLE.to_string()),
                Value::Text(USER.to_string())
            ]
        );
        assert_eq!(join.columns[0], "coalesce(starred, 0) as starred");
        assert_eq!(join.columns[1], "coalesce(rating, 0) as rating");
        assert_eq!(
            join.columns,
            vec![
                "coalesce(starred, 0) as starred",
                "coalesce(rating, 0) as rating",
                "starred_at",
                "play_date",
                "rated_at",
                "coalesce(play_count, 0) as play_count",
                "media_file.average_rating",
            ]
        );
        // Assert: the anonymous user keeps neither join nor columns (:63-65).
        assert!(anonymous.is_none());
    }

    #[test]
    fn with_annotation_normalized_album_play_count_rewrites_only_that_column() {
        // Arrange: the album branch (sql_annotations.go:71-75,
        // AlbumPlayCountModeNormalized) versus the same table in plain mode.
        // Act:
        let normalized = with_annotation("album", USER, "album.id", true);
        let plain = with_annotation("album", USER, "album.id", false);
        // Assert:
        let normalized_join = normalized.expect("album join renders");
        assert_eq!(
            normalized_join.columns[5],
            "round(coalesce(round(cast(play_count as float) / coalesce(album.song_count, 1), 1), 0)) as play_count"
        );
        assert_eq!(normalized_join.columns[6], "album.average_rating");
        assert_eq!(
            plain.expect("plain mode").columns[5],
            "coalesce(play_count, 0) as play_count"
        );
    }

    #[test]
    fn annotation_bool_filter_coalesces_star_and_rating_both_ways() {
        // Assert: the star-rating clamp — NULL annotation columns compare as 0,
        // and Go's lowercased "true" test is case-insensitive
        // (sql_annotations.go:80-88).
        assert_eq!(
            annotation_bool_filter("starred", "true"),
            "COALESCE(starred, 0) > 0"
        );
        assert_eq!(
            annotation_bool_filter("starred", "TRUE"),
            "COALESCE(starred, 0) > 0"
        );
        assert_eq!(
            annotation_bool_filter("starred", "false"),
            "COALESCE(starred, 0) = 0"
        );
        assert_eq!(
            annotation_bool_filter("rating", "1"),
            "COALESCE(rating, 0) = 0"
        );
    }

    #[test]
    fn ann_id_renders_eq_in_and_empty_shapes() {
        // Act / Assert: single id → `= ?`.
        let (sql, args) = ann_id(TABLE, USER, &["mf1"]);
        assert_eq!(
            sql,
            "annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string())
            ]
        );
        // Act / Assert: several ids → IN list.
        let (sql, args) = ann_id(TABLE, USER, &["mf1", "mf2"]);
        assert_eq!(
            sql,
            "annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id IN (?, ?)"
        );
        assert_eq!(args.len(), 4);
        // Act / Assert: no ids → squirrel's empty-Eq 1 = 0.
        let (sql, args) = ann_id(TABLE, USER, &[]);
        assert_eq!(
            sql,
            "annotation.user_id = ? AND annotation.item_type = ? AND 1 = 0"
        );
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn set_star_renders_update_then_fallback_insert() {
        // Arrange: SetStar's annUpsert legs (sql_annotations.go:98-112,136-142).
        // Act:
        let (upd_sql, upd_args) = set_star_sql(TABLE, USER, true, &["mf1"], STAMP);
        let (ins_sql, ins_args) = set_star_insert_sql(TABLE, USER, "mf1", true, STAMP);
        // Assert:
        assert_eq!(
            upd_sql,
            "UPDATE annotation SET starred = ?, starred_at = ? WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?"
        );
        assert_eq!(
            upd_args,
            vec![
                Value::Bool(true),
                Value::Text(STAMP.to_string()),
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
            ]
        );
        assert_eq!(
            ins_sql,
            "INSERT INTO annotation (user_id, item_type, item_id, starred, starred_at) VALUES (?, ?, ?, ?, ?)"
        );
        assert_eq!(
            ins_args,
            vec![
                Value::Text(USER.to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string()),
                Value::Bool(true),
                Value::Text(STAMP.to_string()),
            ]
        );
    }

    #[test]
    fn set_rating_binds_rating_unclamped_then_insert_fallback() {
        // Arrange: SetRating's annUpsert legs (sql_annotations.go:142-151):
        // squirrel gets the rating as given — no clamp in this repository.
        // Act:
        let (upd_sql, upd_args) = set_rating_sql(TABLE, USER, 5, "mf1", STAMP);
        let (ins_sql, ins_args) = set_rating_insert_sql(TABLE, USER, "mf1", 5, STAMP);
        // Assert:
        assert_eq!(
            upd_sql,
            "UPDATE annotation SET rating = ?, rated_at = ? WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?"
        );
        assert_eq!(upd_args[0], Value::Integer(5));
        assert_eq!(
            ins_sql,
            "INSERT INTO annotation (user_id, item_type, item_id, rating, rated_at) VALUES (?, ?, ?, ?, ?)"
        );
        assert_eq!(ins_args[3], Value::Integer(5));
    }

    #[test]
    fn update_avg_rating_binds_subquery_args_before_where_id() {
        // Arrange: updateAvgRating (sql_annotations.go:153-162), squirrel
        // renders the SET Expr's args before the WHERE Eq's.
        // Act:
        let (sql, args) = update_avg_rating_sql(TABLE, "mf1");
        let expr = average_rating_expr_sql();
        // Assert:
        assert_eq!(sql, "UPDATE media_file SET average_rating = ? WHERE id = ?");
        assert_eq!(
            args,
            vec![
                Value::Text("mf1".to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string())
            ]
        );
        assert_eq!(expr.split('?').count(), 3);
        assert!(expr.contains("rating > 0"));
    }

    #[test]
    fn inc_playcount_increments_count_and_keeps_newer_date() {
        // Arrange: IncPlayCount's UPDATE leg (sql_annotations.go:164-170):
        // the playcount increment and the max/ifnull date guard.
        // Act:
        let (sql, args) = inc_play_count_sql(TABLE, USER, "mf1", STAMP);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE annotation SET play_count = play_count+1, play_date = max(ifnull(play_date,''), ?) WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?"
        );
        assert_eq!(args.len(), 4);
        assert_eq!(args[0], Value::Text(STAMP.to_string()));
    }

    #[test]
    fn inc_playcount_insert_fallback_starts_count_at_one() {
        // Arrange: IncPlayCount's INSERT fallback (sql_annotations.go:171-181).
        // Act:
        let (sql, args) = inc_play_count_insert_sql(TABLE, USER, "mf1", STAMP);
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO annotation (user_id, item_type, item_id, play_count, play_date) VALUES (?, ?, ?, ?, ?)"
        );
        assert_eq!(args[3], Value::Integer(1));
    }

    #[test]
    fn reassign_annotation_uses_update_or_ignore() {
        // Arrange: ReassignAnnotation (sql_annotations.go:184-196).
        // Act:
        let (sql, args) = reassign_annotation_sql(TABLE, "mf1", "mf2");
        // Assert:
        assert_eq!(
            sql,
            "update or ignore annotation set item_id = ? where item_type = ? and item_id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("mf2".to_string()),
                Value::Text(TABLE.to_string()),
                Value::Text("mf1".to_string())
            ]
        );
    }

    #[test]
    fn clean_annotations_drops_rows_of_missing_items() {
        // Arrange: cleanAnnotations (sql_annotations.go:198-209).
        // Act:
        let (sql, args) = clean_annotations_sql(TABLE);
        // Assert:
        assert_eq!(sql, "DELETE FROM annotation WHERE item_type = ? AND item_id not in (select id from media_file)");
        assert_eq!(args, vec![Value::Text(TABLE.to_string())]);
    }

    #[test]
    fn get_annotations_selects_annotation_rows_over_ann_id() {
        // Arrange: the annotation repository's Select/From/Where shape.
        // Act:
        let (sql, args) = get_annotations_sql(TABLE, USER, &["mf1"]);
        // Assert:
        assert_eq!(sql, "SELECT * FROM annotation WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?");
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn annotation_exists_and_count_keep_select_aggregates() {
        // Arrange: sqlRepository.exists() (sql_base_repository.go:488-499) and
        // count() (:565-575) over annId.
        // Act:
        let (exists_sql, exists_args) = annotation_exists_sql(TABLE, USER, &["mf1"]);
        let (count_sql, count_args) = annotation_count_sql(TABLE, USER, &["mf1", "mf2"]);
        // Assert: exists counts rows, count keeps the DISTINCT aggregate.
        assert_eq!(
            exists_sql,
            "SELECT count(*) as exist FROM annotation WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id = ?"
        );
        assert_eq!(exists_args.len(), 3);
        assert_eq!(
            count_sql,
            "SELECT count(distinct annotation.id) as count FROM annotation WHERE annotation.user_id = ? AND annotation.item_type = ? AND annotation.item_id IN (?, ?)"
        );
        assert_eq!(count_args.len(), 4);
    }
}
