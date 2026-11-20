//! Port of `persistence/sql_annotations.go` (209L) — the annotation
//! repository's SQL shaping, plus the two EXECUTED rating/transfer writes.
//!
//! Every Go method builds an `Update`/`Insert`/`Delete` squirrel statement and
//! hands it to `executeSQL`, which materializes it through `toSQL()`
//! (`sql_base_repository.go:336-354,358-372`) as `(sql, args)`. Most of the port
//! stops at the same `(sql, args)` pairs; the rating/transfer pair is the
//! exception — [`update_avg_rating_in_tx`] and [`reassign_annotation_in_tx`]
//! EXECUTE those pairs on the caller's transaction (DB-15's runtime half), and
//! every other statement's execution stays with `store.rs`/later slices. Nothing
//! in `sql_annotations.go` opens a transaction: every statement is a separate
//! `executeSQL`, and running it inside one is the caller's choice. Only
//! `ReassignAnnotation` (`:185-197`) needs its UPDATE and `updateAvgRating`
//! atomic, and its callers wrap it in a tx:
//! `WithTx` (`core/maintenance.go:79,89,102`), `WithTxRetry` around
//! `persistFolder` (`scanner/phase_1_folders.go:353`, call at `:490`) and
//! `WithTxRetry` (`scanner/phase_2_missing_tracks.go:291`, call at `:313`).
//! `SetStar` runs in a caller `WithTxImmediate`
//! (`server/subsonic/media_annotation.go:116-144`) and `IncPlayCount` in a
//! `WithTx` (`core/scrobbler/play_tracker.go:506-516`); `SetRating`
//! (`server/subsonic/media_annotation.go:63`) runs its upsert and the follow-up
//! `updateAvgRating` as separate statements with no transaction.
//!
//! `Repository::to_sql` (`sqlrepo.rs`) renders only `SELECT`, so the
//! `UPDATE`/`INSERT`/`DELETE` statements are rendered here directly, while their
//! `WHERE` clause reuses [`ann_id`] — the same rendering the builder's
//! conditions produce. The three standalone annotation `SELECT` renderers
//! ([`get_annotations_sql`], [`annotation_exists_sql`],
//! [`annotation_count_sql`]) build with `Repository` and call `to_sql()`, but
//! they are an **invented seam** with no production call site — see their docs
//! and `tests/runtime_annotation_rating.rs`.
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

/// `updateAvgRating` (`sql_annotations.go:151-160`): recompute the base table's
/// cached `average_rating` from the ratings still on the annotation table.
///
/// Go's `Set("average_rating", Expr(…))` (`:154-157`) puts the aggregate
/// *inside* the SET as raw SQL, so the statement carries three `?`: two from
/// the subquery and one from the outer `WHERE id = ?`. This renderer embeds
/// [`average_rating_expr_sql`] at that position instead of binding it as a
/// value — a `SET average_rating = ?` placeholder cannot execute against the 3
/// args Go's `toSQL()` produces. Arg order follows squirrel rendering SET before
/// WHERE: `[item_id, table, id]`.
///
/// Parity facts the shape must keep: `rating > 0` drops unrated rows, so a
/// zero rating never lowers an average, and there is no `user_id` predicate —
/// Go aggregates across **all** users (`:155-156`), so the cached value is a
/// cross-user average, not a per-user one. `table` is bound as a value
/// (`item_type = ?`), never interpolated, and `coalesce(…, 0)` makes an item
/// with no positive ratings read 0 (the column is `REAL NOT NULL DEFAULT 0`).
#[must_use]
pub fn update_avg_rating_sql(table: &str, item_id: &str) -> Sql {
    let subquery_args = vec![Value::from(item_id), Value::from(table)];
    let where_args = vec![Value::from(item_id)];
    (
        format!(
            "UPDATE {table} SET average_rating = {expr} WHERE id = ?",
            expr = average_rating_expr_sql()
        ),
        concat_args(subquery_args, where_args),
    )
}

/// The `coalesce((select …), 0)` squirrel `Expr` [`update_avg_rating_sql`]
/// embeds as its SET value (`sql_annotations.go:155-157`), exposed separately so
/// the tests pin the raw SQL apart from the arg order. It carries two `?`
/// placeholders; production embeds it as text and never binds it (the runtime
/// test's negative control substitutes it deliberately to reproduce the pre-fix
/// shape).
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

/// Callback-level failure of the executed annotation writes ([`update_avg_rating_in_tx`],
/// [`reassign_annotation_in_tx`]).
///
/// `Infallible` is the honest domain type: `updateAvgRating` (`:151-160`) and
/// `ReassignAnnotation` (`:185-197`) return a bare `error` that can only be a driver
/// failure — no `ErrNotFound`, no validation, and the empty/equal-id short-circuit
/// (`:186-188`) is a successful no-op rather than an error. So the `Domain` arm is
/// structurally uninhabited rather than reserved, exactly as
/// [`crate::property_repo::PropertyWriteError`] does for DB-04.
///
/// The executor (`crate::store::try_with_tx`/`try_with_tx_retry`) is the only place
/// that assigns a [`crate::TransactionStage`], so `Body`/`Begin`/`Commit` cannot be
/// forged here.
pub type AnnotationWriteError = crate::runtime_error::DbOperationError<std::convert::Infallible>;

/// The entities whose repositories implement `model.AnnotatedRepository` —
/// `albumRepository` (`persistence/album_repository.go:105-108`), `mediaFileRepository`
/// (`:88-91`), `artistRepository` (`persistence/artist_repository.go:133-137`) and
/// `playlistRepository` (`:53-56`, whose `tableName` comes from
/// `registerModel(&model.Playlist{})` → `toSnakeCase("playlist")`,
/// `sql_base_repository.go:104-110`) — i.e. exactly the types
/// `ReassignAnnotation`/`updateAvgRating` can legitimately be called for
/// (`model/annotation.go:18-23`, `model/album.go:161`, `model/mediafile.go:580`,
/// `model/artist.go:103`, `model/playlist.go:213`).
///
/// This is the fixed registered entity enum that gates the runtime writes: a caller
/// cannot hand an arbitrary table name to [`reassign_annotation_in_tx`] or
/// [`update_avg_rating_in_tx`], so the table identifier baked into the statement and
/// the bound `item_type` value both come from this closed set. Go reaches the same
/// guarantee structurally — `r.tableName` is set once by the constructor and never
/// taken from a caller.
///
/// The string is Go's `r.tableName`, which serves BOTH roles in these two methods:
/// the base table in `UPDATE <table> SET average_rating = … WHERE id = ?`
/// (`:154-157`) and the bound `item_type` value in the move (`:191`) and in the
/// subquery (`:155`). It is NOT the artwork kind prefix: `item_artwork.item_kind` and
/// `artwork_queue.item_kind` store `al`/`mf` (`nd_id::ARTWORK_KIND_ALBUM_PREFIX` etc,
/// `model/artwork_id.go:22`, `:30-35`; see [`crate::artwork_repo::OWNER_TABLES`]),
/// while `annotation.item_type` stores the full name. The two are not
/// interchangeable, and reading the annotation column by prefix matches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationEntityKind {
    /// `albumRepository`, table and `item_type` `"album"`.
    Album,
    /// `artistRepository`, table and `item_type` `"artist"`.
    Artist,
    /// `mediaFileRepository`, table and `item_type` `"media_file"`.
    MediaFile,
    /// `playlistRepository`, table and `item_type` `"playlist"`.
    Playlist,
}

impl AnnotationEntityKind {
    /// Every registered kind, in `persistence.go:267-270`'s GC order (album, artist,
    /// media file, playlist). Tests iterate it rather than naming variants ad hoc.
    pub const ALL: [Self; 4] = [Self::Album, Self::Artist, Self::MediaFile, Self::Playlist];

    /// Go's `r.tableName` for this kind: the base table that carries the
    /// `average_rating REAL NOT NULL DEFAULT 0` column
    /// (`nd-dbschema/src/schema_ddl.rs:48,58,64,66`) and the `item_type` value bound
    /// by the move and the aggregate subquery.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            Self::Album => crate::album_repo::ALBUM_TABLE,
            Self::Artist => crate::artist_repo::TABLE,
            Self::MediaFile => crate::mediafile_repo::TABLE,
            Self::Playlist => crate::playlist_repo::TABLE,
        }
    }
}

/// `updateAvgRating` (`sql_annotations.go:151-160`) EXECUTED on the caller's
/// transaction: recompute the base table's cached `average_rating` from the ratings
/// that remain on `annotation`, using the [`update_avg_rating_sql`] pair verbatim —
/// embedded aggregate expression in the SET, three ordered binds
/// `[item_id, table, item_id]` (the `ab466928` repair).
///
/// Executed semantics, each pinned by `tests/runtime_annotation_transfers.rs`:
/// `rating > 0` drops zero and NULL ratings (`NULL > 0` is NULL, so a NULL row is
/// excluded too), there is deliberately no `user_id` predicate so the cached value is
/// a cross-user average, and `coalesce(…, 0)` means an item with no positive ratings
/// is reset to `0` rather than left at its stale cached value or written NULL. The
/// update is unconditional, so a call with no matching annotation rows still wipes a
/// stale cache — which is why [`reassign_annotation_in_tx`]'s empty/equal-id
/// short-circuit has to be a real no-op and not an early return that still updates.
///
/// A missing `item_id` in the base table is a successful 0-row statement, not an
/// error (Go ignores the affected-row count in `executeSQL`,
/// `sql_base_repository.go:336-354`).
///
/// # Errors
/// [`AnnotationWriteError::Sqlite`] carrying the concrete `rusqlite::Error` (extended
/// code intact, so `store::is_busy` still classifies it) when the statement fails to
/// prepare, bind, or execute.
pub fn update_avg_rating_in_tx(
    tx: &rusqlite::Transaction<'_>,
    entity: AnnotationEntityKind,
    item_id: &str,
) -> Result<(), AnnotationWriteError> {
    let statement = update_avg_rating_sql(entity.table(), item_id);
    crate::execution::execute(tx, &statement).map_err(AnnotationWriteError::sqlite)?;
    Ok(())
}

/// `ReassignAnnotation` (`sql_annotations.go:185-197`) EXECUTED on the caller's
/// transaction: move this kind's annotation rows from `prev_id` to `new_id`, then
/// recalculate the TARGET's cached average in the SAME transaction.
///
/// Go's `update or ignore` (`:190`, comment at `:189`) keeps `new_id`'s own row for a
/// user who annotated both ids and skips the conflicting source row instead of
/// aborting the statement: the conflict row stays on `prev_id`, is never merged into
/// the target, and is never deleted. An `or replace`/merge shortcut would overwrite
/// the target's own rating — the row assertions in
/// `tests/runtime_annotation_transfers.rs` fail on that shortcut.
///
/// The short-circuit is Go's own (`:186-188`): `prev_id == new_id`, or either id
/// empty, returns success without executing EITHER statement, so the cached averages
/// keep their stored values. Neither leg runs on that path; with two distinct
/// non-empty ids and no source rows the move affects 0 rows but the average
/// recalculation still runs and resets the target.
///
/// Go ignores both affected-row counts (`executeSQL` discards `c` for these callers),
/// so this returns `()`; the moved-row population is observable by reading the
/// `annotation` rows back.
///
/// Retryable: Go wraps this call in `WithTx`/`WithTxRetry`
/// (`core/maintenance.go:79,89,102`, `scanner/phase_1_folders.go:353`,
/// `scanner/phase_2_missing_tracks.go:291`), and a rerun converges — the move is
/// idempotent once the rows already carry `new_id` and the average is recomputed from
/// the stored population.
///
/// # Errors
/// [`AnnotationWriteError::Sqlite`] from either statement, with the concrete
/// `rusqlite::Error` retained as source. The average leg runs only after a successful
/// move, matching Go's order (`:192-196`).
pub fn reassign_annotation_in_tx(
    tx: &rusqlite::Transaction<'_>,
    entity: AnnotationEntityKind,
    prev_id: &str,
    new_id: &str,
) -> Result<(), AnnotationWriteError> {
    // Go's guard, verbatim (`sql_annotations.go:186-188`): equal ids or an empty id
    // succeed without touching either statement.
    if prev_id == new_id || prev_id.is_empty() || new_id.is_empty() {
        return Ok(());
    }
    let statement = reassign_annotation_sql(entity.table(), prev_id, new_id);
    crate::execution::execute(tx, &statement).map_err(AnnotationWriteError::sqlite)?;
    // The moved rows change newID's rating population, so its cached average no
    // longer matches (`sql_annotations.go:195-196`). Only the target is refreshed;
    // prev_id's cached average is deliberately left as stored.
    update_avg_rating_in_tx(tx, entity, new_id)
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

/// Invented seam, not a Go read: `SELECT * FROM annotation WHERE annId(…)`.
///
/// Go has **no annotation repository**: `SQLStore` has no annotation field
/// (`persistence/persistence.go`) and nothing does `Select().From(annotation)`
/// anywhere. Go's annotation surface is the `AnnotatedRepository` mixin
/// (`model/annotation.go:18-23`) implemented by `sqlRepository`
/// (`persistence/sql_annotations.go:137,142,162,185`), and this file reaches
/// `annotation` only through the `withAnnotation` LEFT JOIN (`:63-91`) and the
/// `updateAvgRating` subquery (`:151-160`). Other repositories join `annotation`
/// too — playlist (`playlist_repository.go:390`), playlist_tracks
/// (`playlist_track_repository.go:101`), smart-playlist criteria
/// (`criteria_sql.go:838,843`) and the play-count CTEs (`album_repository.go:416`,
/// `artist_repository.go:446`) — but none is this standalone read, so this
/// renderer asserts a shape Go never produces. It has no production call site:
/// only this crate's unit tests and `tests/runtime_annotation_rating.rs`.
#[must_use]
pub fn get_annotations_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = ann_id(table, user_id, item_ids);
    (
        format!("SELECT * FROM {ANNOTATION_TABLE} WHERE {where_sql}"),
        args,
    )
}

/// Invented seam, not a Go read: the approximate shape `sqlRepository.exists()`
/// (`sql_base_repository.go:488-493`) would take over `annotation`, i.e.
/// `Select("count(*) as exist").From(annotation).Where(annId(…))`. Go cannot
/// instantiate that generic helper here at all (no annotation repository exists),
/// and `annotation` has no `id` column (`schema_ddl.rs:59`, PK
/// `unique(user_id, item_id, item_type)`). Kept as a documented seam; see
/// [`annotation_count_sql`] for the prepare failure this family pins.
#[must_use]
pub fn annotation_exists_sql(table: &str, user_id: &str, item_ids: &[&str]) -> Sql {
    let (where_sql, args) = ann_id(table, user_id, item_ids);
    let repo = Repository::new(ANNOTATION_TABLE)
        .select(["count(*) as exist"])
        .where_raw(where_sql, args);
    materialize(repo)
}

/// Invented seam, **deliberately non-executable**: the approximate shape
/// `sqlRepository.count()` (`sql_base_repository.go:565-575`) would take over
/// `annotation` — `count(distinct annotation.id) as count`. `annotation` has
/// **no `id` column** (nd-dbschema fresh DDL `schema_ddl.rs:59`; the PK is
/// `unique(user_id, item_id, item_type)`), so SQLite rejects preparation with
/// `no such column: annotation.id`. That is the parity fact worth pinning: the
/// generic `count`/`exists` helpers cannot be instantiated over `annotation`, and
/// Go never tries. The renderer stays so the latent defect is pinned by
/// `tests/runtime_annotation_rating.rs` rather than silently shipped; do not
/// "fix" it by inventing an id column or swapping to `count(distinct item_id)`,
/// which is a different number and not Go's aggregate.
///
/// Not full-shape parity: Go's `count` also strips offset/limit and appends
/// `OrderBy(annotation.id)` (`:568-569`), which `sqlrepo::Repository::count` does
/// not emit. Both forms fail preparation for the same missing-column reason, so
/// the pinned fact is unaffected; the ORDER BY gap is tracked separately in
/// `docs/rust-port/next-work.md`.
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
        // Arrange: updateAvgRating (sql_annotations.go:151-160). squirrel
        // renders the SET Expr's args before the WHERE Eq's, and the Expr text
        // itself is embedded in the SET (never bound as a value).
        // Act:
        let (sql, args) = update_avg_rating_sql(TABLE, "mf1");
        let expr = average_rating_expr_sql();
        // Assert: 3 placeholders (2 from the embedded subquery, 1 from the
        // outer WHERE) for the 3 args. Executed end-to-end in
        // tests/runtime_annotation_rating.rs.
        assert_eq!(
            sql,
            "UPDATE media_file SET average_rating = coalesce((select round(avg(rating), 2) from annotation where item_id = ? and item_type = ? and rating > 0), 0) WHERE id = ?"
        );
        assert_eq!(sql.split('?').count(), 4);
        assert!(sql.contains(&expr));
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
        // Go's subquery has no user_id predicate: the average spans all users.
        assert!(!expr.contains("user_id"));
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
