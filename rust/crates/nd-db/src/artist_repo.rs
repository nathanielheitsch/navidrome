//! Port of `persistence/artist_repository.go` (753L) — the artist table with
//! its per-library stats aggregation, index groups and search scoping.
//!
//! Scope: the stats select shape ([`select_artist_parts`], `:207-214` via
//! `applyLibraryFilterToArtistQuery`, `:191-203`), `Get`/`GetAll`
//! ([`get_sql`]/[`get_all_sql`]), `CountAll` (`:217-227`, the annotation
//! join only when the filters reference annotation columns), the
//! `count(distinct …)` `Exists` (`:229-236`), the `Put`/`UpdateExternalInfo`
//! upserts (`:239-253` via `put`, `sql_base_repository.go:613-641`, with
//! `dbArtist.PostMapArgs`' derived columns and the empty
//! `sort_artist_name`/`mbz_artist_id` drops, `:98-121`), `getAllIDs`
//! (`:283-291`), `GetCursor`'s id pre-pass + chunking (`:293-308`),
//! `GetIndex` (`:327-367`, including `getIndexKey`, `:310-325`), the
//! maintenance statements (`purgeEmpty` `:370-405`, `markOrphansMissing`
//! `:408-419`, `markMissing` `:421-439`, `RefreshPlayCounts` `:441-463`,
//! `RefreshStats`' id selects + batched `library_artist` update + cleanup,
//! `:465-590`), the search config + scoping (`searchCfg` `:616-630`,
//! `artistLibraryFilter` `:632-649`, `Search` `:651-665`, `searchScope`
//! `:667-700`, `requestedLibraryIDs`/`isLibraryIDFilter` `:702-727`), and
//! the rest-facing `registerModel`/`setSortMappings` registry (`:52-69`).
//!
//! Rendering deviation, named: `selectArtist`'s stats column
//! (`JSON_GROUP_OBJECT(...)`) and `GROUP BY artist.id` have no
//! [`crate::sqlrepo::Repository`] spelling (that builder renders neither),
//! so the artist selects are hand-rendered here in squirrel's clause order
//! (JOIN, WHERE, GROUP BY, ORDER BY, LIMIT/OFFSET) with every value bound
//! and every identifier fixed. The search path composes the shared
//! exported phases ([`crate::sql_search::rowid_page_phase`]'s filter stack)
//! instead of [`crate::sql_search::do_search`], for the same reason — the
//! base select cannot ride through a `Repository` without losing the
//! aggregation; the dispatch order (empty → natural-order rowid page,
//! UUID → MBID, `<2` bytes → none, else strategy) is Go's `doSearch`
//! (`sql_search.go:35-62`) verbatim.
//!
//! Roles: Go's `model.Role` string set (`model/participants.go:28-48`) is
//! pinned as [`ALL_ROLES`]; `sanitizeArtistStatsRole` (`:79-88`) allowlists
//! them (plus `"total"`, the scanner's aggregate key) before any role
//! reaches SQL concatenation — `roleFilter` (`:71-77`) and the role sort
//! mappings (`:60-69`, `sortMappingsForRole` `:704-716`) build JSON paths
//! from the sanitized value only. The always-false `Eq{"1": 2}` fallback
//! of both is [`FALSE_EXPR`] — no bound args.
//!
//! Ownership: `applyLibraryFilterToArtistQuery`'s `user_library` join is
//! gated on a logged-in non-admin user (`:197-203`); the crate has no
//! ambient context (the `sqlrepo.rs` DI rule), so the selects take the
//! user as [`User`] — `None` is admin/headless (no join). Timestamps are
//! epoch-seconds integers, caller-owned clock (see `radio_repo.rs`).
//!
//! Not ported, with the reason: `hydrateArtwork` (`:294-296`) is the shared
//! batched `item_artwork` lookup (`artwork_hydration.go:77-94`) — that
//! select belongs to the artwork lane (`artwork_repo.rs`).
//! `dbArtist.PostScan`'s stats-JSON fold into `model.ArtistStats`
//! (`:43-67`) reads the row the caller materializes; the model type is the
//! #06 slice's. `visibleLibraryIDs`/`userSeesAllLibraries`
//! (`sql_base_repository.go:286-320`) need the user row; [`search_scope`]
//! takes the resolved visible set and the caller owns the fail-safe.

use std::collections::HashMap;

use crate::sql_restful::{Filter, QueryOptions, RestRepository};
use crate::sql_search::{mbid_expr, rowid_page_phase, SearchConfig, Sql};
use crate::sql_search_fts as fts;
use crate::sql_search_like as like;
use crate::sqlrepo::{Repository, SqlError, Value};
use nd_utils::str::{normalize_for_fts, sanitize_strings};

/// `r.tableName` (`:54`).
pub const TABLE: &str = "artist";

/// The `library_artist` junction (`:195`, `:465-586`).
pub const LIBRARY_ARTIST_TABLE: &str = "library_artist";

/// The `user_library` junction the non-admin join adds (`:200`).
pub const USER_LIBRARY_TABLE: &str = "user_library";

/// The `album_artists` junction `purgeEmpty` probes (`:371`).
pub const ALBUM_ARTISTS_TABLE: &str = "album_artists";

/// `model.AllRoles` keys (`model/participants.go:28-48`), pinned in Go's
/// map-declaration order.
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

/// The scanner's aggregate key, always a valid role path
/// (`sanitizeArtistStatsRole` `:83-84`).
pub const TOTAL_ROLE: &str = "total";

/// `Eq{"1": 2}` (`:76`, `:706`) — the always-false fallback, arg-free.
pub const FALSE_EXPR: &str = "1 = 2";

/// `invalidUserId` (`sql_base_repository.go:51`).
pub const INVALID_USER_ID: &str = "-1";

/// The effective user for the library join: `None` is admin/headless
/// (Go's `user.ID != invalidUserId && !user.IsAdmin` skip, `:199-203`),
/// `Some(id)` a logged-in non-admin.
pub type User = Option<String>;

/// `artworkChunkSize` (`artwork_hydration.go:16`, `artworkBatchSize`*3)
/// of `GetCursor`'s id pre-pass (`streamByIDs`, `:24-34`).
pub const CURSOR_CHUNK_SIZE: usize = 300;

/// `RefreshStats`'s batch size (`:520`).
pub const REFRESH_STATS_BATCH_SIZE: usize = 1000;

/// The number of `ROLE_IDS_PLACEHOLDER` IN-clauses the batch template
/// carries (`:491-576`).
const REFRESH_STATS_IN_CLAUSES: usize = 4;

/// The stats column `selectArtist` selects (`:210-211`), verbatim.
const LIBRARY_STATS_COLUMN: &str = "JSON_GROUP_OBJECT(library_artist.library_id, \
     JSONB(library_artist.stats)) as library_stats_json";

/// `model.Artist` minus the `structs:"-"` embeds (`model/artist.go:15-52`):
/// exactly the columns `toSQLArgs` yields, struct order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtistRecord {
    /// `id`.
    pub id: String,
    /// `name`.
    pub name: String,
    /// `sort_artist_name`.
    pub sort_artist_name: String,
    /// `order_artist_name`.
    pub order_artist_name: String,
    /// `mbz_artist_id`.
    pub mbz_artist_id: String,
    /// `biography`.
    pub biography: String,
    /// `small_image_url`.
    pub small_image_url: String,
    /// `medium_image_url`.
    pub medium_image_url: String,
    /// `large_image_url`.
    pub large_image_url: String,
    /// `external_url`.
    pub external_url: String,
    /// `similar_artists` — the marshalled `[{id, name}]` JSON
    /// (`PostMapArgs`, `:102-107`); `""` reads as `[]`.
    pub similar_artists: String,
    /// `external_info_updated_at` (epoch seconds; 0 = Go's nil).
    pub external_info_updated_at: i64,
    /// `missing`.
    pub missing: bool,
    /// `uploaded_image`.
    pub uploaded_image: String,
    /// `created_at` (epoch seconds; 0 = Go's nil).
    pub created_at: i64,
    /// `updated_at` (epoch seconds; 0 = Go's nil).
    pub updated_at: i64,
}

/// The `library_artist` + `user_library` joins every artist select carries
/// (`applyLibraryFilterToArtistQuery`, `:191-203`), rendered as one SQL
/// fragment plus the user id arg (bound at the join position, before any
/// WHERE arg — squirrel's order).
fn library_artist_join_fragment(user: &User) -> (String, Vec<Value>) {
    let mut sql = format!(
        " INNER JOIN {LIBRARY_ARTIST_TABLE} on \
         {LIBRARY_ARTIST_TABLE}.artist_id = {TABLE}.id"
    );
    let mut args = Vec::new();
    if let Some(user_id) = user {
        sql.push_str(&format!(
            " INNER JOIN {USER_LIBRARY_TABLE} on \
             {USER_LIBRARY_TABLE}.library_id = {LIBRARY_ARTIST_TABLE}.library_id \
             AND {USER_LIBRARY_TABLE}.user_id = ?"
        ));
        args.push(Value::Text(user_id.clone()));
    }
    (sql, args)
}

/// The `WHERE` body `options` contributes (`applyFilters`,
/// `sql_base_repository.go:247-251`): every parsed filter AND-joined,
/// `None` when there are none.
fn where_fragment(options: Option<&QueryOptions>) -> Option<(String, Vec<Value>)> {
    options.and_then(|o| o.where_clause())
}

/// The `LIMIT ? OFFSET ?` tail `applyOptions` appends (`:149-159`).
fn pagination_fragment(options: Option<&QueryOptions>) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut args = Vec::new();
    if let Some(o) = options {
        if o.max > 0 {
            sql.push_str(" LIMIT ?");
            args.push(Value::Integer(o.max));
        }
        if o.offset > 0 {
            sql.push_str(" OFFSET ?");
            args.push(Value::Integer(o.offset));
        }
    }
    (sql, args)
}

/// `selectArtist` (`:207-214`): the stats select — `artist.*` plus the
/// `JSON_GROUP_OBJECT` library-stats fold, the library joins, the caller's
/// filters, `GROUP BY artist.id`, and the annotation join's LEFT JOIN
/// fragment + columns when one applies (`withAnnotation`,
/// `sql_annotations.go:63-77`). Squirrel renders JOIN → WHERE → GROUP BY →
/// LIMIT; the annotation LEFT JOIN binds its args (`item_type`, `user_id`)
/// after the `user_library` join arg, before the filter args.
#[must_use]
pub fn select_artist_sql(
    user: &User,
    options: Option<&QueryOptions>,
    annotation: Option<&(String, Vec<String>, Vec<Value>)>,
) -> Sql {
    let (joins, mut args) = library_artist_join_fragment(user);
    let mut columns = vec![format!("{TABLE}.*"), LIBRARY_STATS_COLUMN.to_string()];
    let mut join_sql = joins;
    if let Some((join, ann_columns, ann_args)) = annotation {
        join_sql.push_str(join);
        args.extend(ann_args.iter().cloned());
        columns.extend(ann_columns.iter().cloned());
    }
    let mut sql = format!("SELECT {} FROM {TABLE}{join_sql}", columns.join(", "));
    if let Some((filter_sql, filter_args)) = where_fragment(options) {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
    }
    sql.push_str(&format!(" GROUP BY {TABLE}.id"));
    let (page_sql, page_args) = pagination_fragment(options);
    sql.push_str(&page_sql);
    args.extend(page_args);
    (sql, args)
}

/// `Get`/`Read` (`:255-267,718-720`): the stats select narrowed to one id.
/// Go appends `Eq{"artist.id": id}` to `selectArtist()`'s WHERE stack, so
/// the id binds after the join args and before any filter args.
#[must_use]
pub fn get_sql(
    id: &str,
    user: &User,
    annotation: Option<&(String, Vec<String>, Vec<Value>)>,
) -> Sql {
    let (mut sql, mut args) = select_artist_prefix(user, annotation);
    sql.push_str(&format!(" WHERE {TABLE}.id = ?"));
    args.push(Value::Text(id.to_string()));
    sql.push_str(&format!(" GROUP BY {TABLE}.id"));
    (sql, args)
}

/// The FROM+JOIN prefix [`get_sql`] renders (see [`select_artist_sql`]).
fn select_artist_prefix(
    user: &User,
    annotation: Option<&(String, Vec<String>, Vec<Value>)>,
) -> (String, Vec<Value>) {
    let (joins, mut args) = library_artist_join_fragment(user);
    let mut columns = vec![format!("{TABLE}.*"), LIBRARY_STATS_COLUMN.to_string()];
    let mut join_sql = joins;
    if let Some((join, ann_columns, ann_args)) = annotation {
        join_sql.push_str(join);
        args.extend(ann_args.iter().cloned());
        columns.extend(ann_columns.iter().cloned());
    }
    (
        format!("SELECT {} FROM {TABLE}{join_sql}", columns.join(", ")),
        args,
    )
}

/// `GetAll`/`ReadAll` (`:269-281`): the stats select with the caller's
/// filters and pagination.
#[must_use]
pub fn get_all_sql(user: &User, options: Option<&QueryOptions>) -> Sql {
    select_artist_sql(user, options, None)
}

/// The annotation columns `filtersNeedAnnotation`'s Go regex matches
/// (`annotationColumns`, `sql_annotations.go:21-33`).
pub const ANNOTATION_COLUMNS: [&str; 7] = [
    "play_count",
    "play_date",
    "rating",
    "rated_at",
    "starred",
    "starred_at",
    "average_rating",
];

/// `filtersNeedAnnotation` (`sql_annotations.go:37-42`): whether any filter
/// references an annotation column, gating `CountAll`'s annotation join
/// (`:221-225`). The port walks [`Filter`] — the crate's filter set —
/// matching whole column words case-insensitively, as Go's `\b` regex does.
#[must_use]
pub fn filters_need_annotation(filters: Option<&[Filter]>) -> bool {
    let Some(filters) = filters else {
        return false;
    };
    filters.iter().any(|f| match f {
        Filter::Eq { column, .. } | Filter::Like { column, .. } => is_annotation_column(column),
        Filter::Raw { sql, .. } => {
            let lower = sql.to_lowercase();
            ANNOTATION_COLUMNS.iter().any(|col| {
                let bare = format!(" {col} ");
                let paren = format!("({col} ");
                let coalesce = format!("coalesce({col}, 0)");
                let dot = format!(".{col} ");
                lower.contains(&bare)
                    || lower.contains(&paren)
                    || lower.contains(&dot)
                    || lower.contains(&coalesce)
            })
        }
    })
}

/// One whole-word column check for [`filters_need_annotation`].
fn is_annotation_column(column: &str) -> bool {
    let lower = column.to_lowercase();
    let name = lower.rsplit('.').next().unwrap_or(&lower);
    ANNOTATION_COLUMNS.contains(&name)
}

/// `CountAll` (`:217-227`): `count(distinct artist.id) as count` over the
/// library joins (kept — an artist spans libraries), the filters, and the
/// annotation join only when [`filters_need_annotation`] says so. The
/// GROUP BY drops with the columns (`count()`, `sql_base_repository.go:
/// 565-575`).
#[must_use]
pub fn count_all_sql(user: &User, options: Option<&QueryOptions>) -> Sql {
    let need_annotation = filters_need_annotation(options.and_then(|o| o.filters.as_deref()));
    let annotation = if need_annotation {
        user.as_deref().and_then(annotation_join)
    } else {
        None
    };
    let (joins, mut args) = library_artist_join_fragment(user);
    let mut join_sql = joins;
    if let Some((join, _, ann_args)) = &annotation {
        join_sql.push_str(join);
        args.extend(ann_args.iter().cloned());
    }
    let mut sql = format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE}{join_sql}");
    if let Some((filter_sql, filter_args)) = where_fragment(options) {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
    }
    (sql, args)
}

/// `Exists` (`:229-236`): `count(distinct artist.id) as exist` over the
/// library joins + the id clause.
#[must_use]
pub fn exists_sql(id: &str, user: &User) -> Sql {
    let (joins, mut args) = library_artist_join_fragment(user);
    let mut sql = format!("SELECT count(distinct {TABLE}.id) as exist FROM {TABLE}{joins}");
    sql.push_str(&format!(" WHERE {TABLE}.id = ?"));
    args.push(Value::Text(id.to_string()));
    (sql, args)
}

/// The `LEFT JOIN annotation …` fragment `withAnnotation` appends
/// (`sql_annotations.go:63-77`), spelled out here because
/// [`crate::sql_annotations::AnnotationJoin`] keeps its fields private (that
/// module's own slice) — the text is Go-verbatim squirrel output. `None`
/// for [`INVALID_USER_ID`] (Go returns the query untouched, `:63-65`).
/// `artist` is not `album`, so `normalized_album_play_count` never applies
/// (`:71-75`).
#[must_use]
pub fn annotation_join(user_id: &str) -> Option<(String, Vec<String>, Vec<Value>)> {
    if user_id == INVALID_USER_ID {
        return None;
    }
    let join = format!(
        " LEFT JOIN annotation ON \
         (annotation.item_id = {TABLE}.id \
         AND annotation.item_type = ? \
         AND annotation.user_id = ?)"
    );
    let columns = vec![
        "coalesce(starred, 0) as starred".to_string(),
        "coalesce(rating, 0) as rating".to_string(),
        "starred_at".to_string(),
        "play_date".to_string(),
        "rated_at".to_string(),
        "coalesce(play_count, 0) as play_count".to_string(),
        format!("{TABLE}.average_rating"),
    ];
    let args = vec![Value::from(TABLE), Value::from(user_id)];
    Some((join, columns, args))
}

/// `PostMapArgs`' `full_text` (`:109`): `formatFullText(name, sortArtistName)`
/// (`sql_search.go:15-18` — a leading space + `SanitizeStrings`).
#[must_use]
pub fn format_full_text(name: &str, sort_artist_name: &str) -> String {
    format!(" {}", sanitize_strings(&[name, sort_artist_name]))
}

/// `PostMapArgs`' `search_normalized` (`:110`): `str.NormalizeForFTS(name)`.
#[must_use]
pub fn search_normalized(name: &str) -> String {
    normalize_for_fts(&[name])
}

/// `sanitizeArtistStatsRole` (`:79-88`): allowlist a role for JSON-path
/// interpolation. `"total"` (the aggregate key) is always valid; `None`
/// marks rejection. Unknown values must not reach SQL concatenation.
#[must_use]
pub fn sanitize_artist_stats_role(role: &str) -> Option<String> {
    if role.is_empty() || role == TOTAL_ROLE {
        return Some(TOTAL_ROLE.to_string());
    }
    if ALL_ROLES.contains(&role) {
        return Some(role.to_string());
    }
    None
}

/// `roleFilter` (`:71-77`): `JSON_EXTRACT(library_artist.stats,
/// '$.<role>.m') IS NOT NULL` for a sanitized role that is not `total`,
/// else [`FALSE_EXPR`]. The JSON path interpolates only the allowlisted
/// role (`:75`), never the raw filter value.
#[must_use]
pub fn role_filter(role: &str) -> Sql {
    match sanitize_artist_stats_role(role) {
        Some(safe) if safe != TOTAL_ROLE => (
            format!("JSON_EXTRACT({LIBRARY_ARTIST_TABLE}.stats, '$.{safe}.m') IS NOT NULL"),
            Vec::new(),
        ),
        _ => (FALSE_EXPR.to_string(), Vec::new()),
    }
}

/// `artistLibraryIdFilter` (`:90-92`): `library_artist.library_id = ?`.
#[must_use]
pub fn artist_library_id_filter(library_id: Value) -> Sql {
    (
        format!("{LIBRARY_ARTIST_TABLE}.library_id = ?"),
        vec![library_id],
    )
}

/// `artistLibraryFilter` (`:632-649`): the join-free EXISTS scope. The
/// inner `LIMIT 1` is load-bearing (`:643-647`): it stops SQLite from
/// flattening the EXISTS back into a fan-out join while still using the
/// `(library_id, artist_id)` UNIQUE autoindex. Empty ids → [`FALSE_EXPR`]
/// (Go's `Eq{"1": 2}`, `:638`).
#[must_use]
pub fn artist_library_filter(library_ids: &[i64]) -> Sql {
    if library_ids.is_empty() {
        return (FALSE_EXPR.to_string(), Vec::new());
    }
    let placeholders = vec!["?"; library_ids.len()].join(", ");
    (
        format!(
            "EXISTS (SELECT 1 FROM {LIBRARY_ARTIST_TABLE} \
             WHERE {LIBRARY_ARTIST_TABLE}.artist_id = {TABLE}.id \
             AND {LIBRARY_ARTIST_TABLE}.library_id IN ({placeholders}) LIMIT 1)"
        ),
        library_ids.iter().map(|id| Value::Integer(*id)).collect(),
    )
}

/// `UpdateExternalInfo`'s column list (`:250-253`).
pub const EXTERNAL_INFO_COLS: [&str; 7] = [
    "biography",
    "small_image_url",
    "medium_image_url",
    "large_image_url",
    "similar_artists",
    "external_url",
    "external_info_updated_at",
];

/// `dbSimilarArtist` marshalling (`:31-34`, `PostMapArgs` `:102-107`):
/// `[{id, name}]` with Go's `omitempty` key skipping, `[]` when empty.
#[must_use]
pub fn marshal_similar_artists(similar: &[(String, String)]) -> String {
    let entries = similar
        .iter()
        .map(|(id, name)| {
            let mut fields = Vec::new();
            if !id.is_empty() {
                fields.push(format!("\"id\":{id:?}"));
            }
            if !name.is_empty() {
                fields.push(format!("\"name\":{name:?}"));
            }
            format!("{{{}}}", fields.join(","))
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}

/// `PostMapArgs` (`:98-121`) as the SET pair list of the upsert — the
/// struct columns plus the derived `full_text`/`search_normalized`, minus
/// Go's `delete` of an empty `sort_artist_name`/`mbz_artist_id` (`:113-120`)
/// so an empty value never overwrites the stored one.
#[must_use]
pub fn put_cols(artist: &ArtistRecord) -> Vec<(String, Value)> {
    let mut cols = vec![
        ("id".to_string(), Value::Text(artist.id.clone())),
        ("name".to_string(), Value::Text(artist.name.clone())),
        (
            "sort_artist_name".to_string(),
            Value::Text(artist.sort_artist_name.clone()),
        ),
        (
            "order_artist_name".to_string(),
            Value::Text(artist.order_artist_name.clone()),
        ),
        (
            "mbz_artist_id".to_string(),
            Value::Text(artist.mbz_artist_id.clone()),
        ),
        (
            "biography".to_string(),
            Value::Text(artist.biography.clone()),
        ),
        (
            "small_image_url".to_string(),
            Value::Text(artist.small_image_url.clone()),
        ),
        (
            "medium_image_url".to_string(),
            Value::Text(artist.medium_image_url.clone()),
        ),
        (
            "large_image_url".to_string(),
            Value::Text(artist.large_image_url.clone()),
        ),
        (
            "external_url".to_string(),
            Value::Text(artist.external_url.clone()),
        ),
        (
            "similar_artists".to_string(),
            Value::Text(artist.similar_artists.clone()),
        ),
        (
            "external_info_updated_at".to_string(),
            Value::Integer(artist.external_info_updated_at),
        ),
        ("missing".to_string(), Value::Bool(artist.missing)),
        (
            "uploaded_image".to_string(),
            Value::Text(artist.uploaded_image.clone()),
        ),
        ("created_at".to_string(), Value::Integer(artist.created_at)),
        ("updated_at".to_string(), Value::Integer(artist.updated_at)),
        (
            "full_text".to_string(),
            Value::Text(format_full_text(&artist.name, &artist.sort_artist_name)),
        ),
        (
            "search_normalized".to_string(),
            Value::Text(search_normalized(&artist.name)),
        ),
    ];
    cols.retain(|(col, _)| {
        !(col == "sort_artist_name" && artist.sort_artist_name.is_empty())
            && !(col == "mbz_artist_id" && artist.mbz_artist_id.is_empty())
    });
    cols
}

/// `Put` (`:239-245`) / `UpdateExternalInfo` (`:247-253`) via `put`
/// (`sql_base_repository.go:613-641`): the UPDATE-first/INSERT-fallback
/// upsert. `selectUpdateColumns` (`:587-604`) filters the SET list to the
/// requested columns (all when none), always minus `created_at`;
/// `Put`'s `CreatedAt = new(time.Now())` (`:240-241`) stamps a fresh row's
/// timestamps and every save re-stamps `updated_at`. The UPDATE's row
/// count decides which statement ran — the caller's.
#[must_use]
pub fn put_sql(artist: &ArtistRecord, now: i64, cols_to_update: &[&str]) -> (Sql, Sql) {
    let stamped = ArtistRecord {
        created_at: if artist.created_at == 0 {
            now
        } else {
            artist.created_at
        },
        updated_at: now,
        ..artist.clone()
    };
    let all_cols = put_cols(&stamped);
    let mut wanted: Vec<&str> = cols_to_update.to_vec();
    if !wanted.is_empty() && !wanted.contains(&"updated_at") {
        wanted.push("updated_at");
    }
    let update_cols: Vec<&(String, Value)> = all_cols
        .iter()
        .filter(|(col, _)| wanted.is_empty() || wanted.contains(&col.as_str()))
        .filter(|(col, _)| col != "created_at")
        .collect();
    let set = update_cols
        .iter()
        .map(|(col, _)| format!("{col} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_args: Vec<Value> = update_cols.iter().map(|(_, v)| v.clone()).collect();
    update_args.push(Value::Text(artist.id.clone()));
    let update = (
        format!("UPDATE {TABLE} SET {set} WHERE id = ?"),
        update_args,
    );
    let insert_set = all_cols
        .iter()
        .map(|(col, _)| format!("{col} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_args: Vec<Value> = all_cols.iter().map(|(_, v)| v.clone()).collect();
    let insert = (format!("INSERT INTO {TABLE} SET {insert_set}"), insert_args);
    (update, insert)
}

/// `getAllIDs` (`:283-291`): `SELECT artist.id` over the library joins,
/// `GROUP BY artist.id`, the annotation join gated like `CountAll`'s, plus
/// the caller's filters/pagination — the `GetCursor` pre-pass
/// (`:299-308` + `artwork_hydration.go:37-50`'s `chunkOptions`, which
/// reuses the sort and swaps the filters for an id IN-list).
#[must_use]
pub fn get_all_ids_sql(user: &User, options: Option<&QueryOptions>) -> Sql {
    let need_annotation = filters_need_annotation(options.and_then(|o| o.filters.as_deref()));
    let annotation = if need_annotation {
        user.as_deref().and_then(annotation_join)
    } else {
        None
    };
    let (joins, mut args) = library_artist_join_fragment(user);
    let mut join_sql = joins;
    if let Some((join, _, ann_args)) = &annotation {
        join_sql.push_str(join);
        args.extend(ann_args.iter().cloned());
    }
    let mut sql = format!("SELECT {TABLE}.id FROM {TABLE}{join_sql}");
    if let Some((filter_sql, filter_args)) = where_fragment(options) {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
    }
    sql.push_str(&format!(" GROUP BY {TABLE}.id"));
    let (page_sql, page_args) = pagination_fragment(options);
    sql.push_str(&page_sql);
    args.extend(page_args);
    (sql, args)
}

/// `chunkOptions` (`artwork_hydration.go:37-50`): the per-chunk options of
/// `GetCursor`'s hydrating fetch — the base sort kept, Max/Offset dropped
/// (the id pre-pass consumed them), the filters replaced by
/// `Eq{artist.id: chunk}` AND-joined over the base filters.
#[must_use]
pub fn chunk_options(base: Option<&QueryOptions>, id_chunk: &[String]) -> QueryOptions {
    let mut opts = match base {
        Some(base) => QueryOptions {
            sort: base.sort.clone(),
            order: base.order.clone(),
            ..QueryOptions::default()
        },
        None => QueryOptions::default(),
    };
    let chunk_filter = Filter::Eq {
        column: format!("{TABLE}.id"),
        value: Value::Text(id_chunk.join(",")),
    };
    let mut filters = match base.and_then(|b| b.filters.clone()) {
        Some(base_filters) => {
            let mut all = base_filters;
            all.push(chunk_filter);
            all
        }
        None => vec![chunk_filter],
    };
    filters.dedup();
    opts.filters = Some(filters);
    opts
}

/// `getIndexKey` (`:310-325`): the first matching index-group prefix of
/// the lowercased source name — `order_artist_name` unless
/// `PreferSortTags` and a `sort_artist_name` exists (`cmp.Or`,
/// `:315-317`), else `"#"`. The groups are caller-supplied config
/// (`utils.ParseIndexGroups`), matched case-insensitively (`:320-324`).
/// `name` is Go's unused-at-this-point fallback source, kept for parity of
/// the signature's inputs.
#[must_use]
pub fn get_index_key(
    index_groups: &HashMap<String, String>,
    prefer_sort_tags: bool,
    order_artist_name: &str,
    sort_artist_name: &str,
    name: &str,
) -> String {
    let _ = name;
    let source = if prefer_sort_tags && !sort_artist_name.is_empty() {
        sort_artist_name
    } else {
        order_artist_name
    };
    let lower = source.to_lowercase();
    for (key, value) in index_groups {
        if lower.starts_with(&key.to_lowercase()) {
            return value.clone();
        }
    }
    "#".to_string()
}

/// `purgeEmpty` (`:370-405`) — the orphan probe select plus the delete;
/// the uploaded-image file cleanup (`os.Remove`) is the caller's.
#[must_use]
pub fn purge_empty_sql() -> (Sql, Sql) {
    let orphan = format!("id not in (select artist_id from {ALBUM_ARTISTS_TABLE})");
    let select = (
        format!("SELECT uploaded_image FROM {TABLE} WHERE {orphan} AND uploaded_image != ''"),
        Vec::new(),
    );
    let delete = (format!("DELETE FROM {TABLE} WHERE {orphan}"), Vec::new());
    (select, delete)
}

/// `markOrphansMissing` (`:408-419`): the verbatim UPDATE.
#[must_use]
pub fn mark_orphans_missing_sql() -> Sql {
    (
        format!(
            "update {TABLE} set missing = true where missing = false \
             and not exists (select 1 from {LIBRARY_ARTIST_TABLE} \
             where {LIBRARY_ARTIST_TABLE}.artist_id = {TABLE}.id)"
        ),
        Vec::new(),
    )
}

/// `markMissing` (`:421-439`): the CTE UPDATE, verbatim.
#[must_use]
pub fn mark_missing_sql() -> Sql {
    (
        "with artists_with_non_missing_albums as ( \
         select distinct aa.artist_id \
         from album_artists aa \
         join album a on aa.album_id = a.id \
         where a.missing = false \
         ) \
         update artist \
         set missing = (artist.id not in (select artist_id from artists_with_non_missing_albums));"
            .to_string(),
        Vec::new(),
    )
}

/// `RefreshPlayCounts` (`:441-463`): the annotation upsert from the
/// `json_tree` participant fold, verbatim.
#[must_use]
pub fn refresh_play_counts_sql() -> Sql {
    (
        "with play_counts as ( \
         select user_id, atom as artist_id, sum(play_count) as total_play_count, max(play_date) as last_play_date \
         from media_file \
         join annotation on item_id = media_file.id \
         left join json_tree(participants, '$.artist') as jt \
         where atom is not null and key = 'id' \
         group by user_id, atom \
         ) \
         insert into annotation (user_id, item_id, item_type, play_count, play_date) \
         select user_id, artist_id, 'artist', total_play_count, last_play_date \
         from play_counts \
         where total_play_count > 0 \
         on conflict (user_id, item_id, item_type) do update \
         set play_count = excluded.play_count, \
         play_date  = excluded.play_date;"
            .to_string(),
        Vec::new(),
    )
}

/// `RefreshStats`'s id selects (`:469-489`): all artists, or only those
/// updated after the oldest recorded scan.
#[must_use]
pub fn refresh_stats_ids_sql(all_artists: bool) -> Sql {
    if all_artists {
        (
            format!("SELECT DISTINCT id FROM {TABLE} WHERE id <> ''"),
            Vec::new(),
        )
    } else {
        (
            format!(
                "SELECT DISTINCT id FROM {TABLE} \
                 WHERE updated_at > (SELECT last_scan_at FROM library \
                 ORDER BY last_scan_at ASC LIMIT 1)"
            ),
            Vec::new(),
        )
    }
}

/// One `RefreshStats` batch's UPDATE (`:491-578`): the CTE template with
/// each `ROLE_IDS_PLACEHOLDER` replaced by the batch's `IN (?, …)` list,
/// and the args repeated once per IN clause in per-clause order
/// (`:548-561`). `None` for an empty batch (Go never builds one).
#[must_use]
pub fn refresh_stats_batch_sql(artist_ids: &[String]) -> Option<Sql> {
    if artist_ids.is_empty() {
        return None;
    }
    let placeholders = vec!["?"; artist_ids.len()].join(", ");
    let mut sql = REFRESH_STATS_TEMPLATE.to_string();
    for _ in 0..REFRESH_STATS_IN_CLAUSES {
        sql = sql.replacen("ROLE_IDS_PLACEHOLDER", &placeholders, 1);
    }
    let mut args = Vec::with_capacity(REFRESH_STATS_IN_CLAUSES * artist_ids.len());
    for _ in 0..REFRESH_STATS_IN_CLAUSES {
        args.extend(artist_ids.iter().map(|id| Value::Text(id.clone())));
    }
    Some((sql, args))
}

/// The batch UPDATE template (`:491-578`), verbatim.
const REFRESH_STATS_TEMPLATE: &str = "\
    WITH artist_role_counters AS (
        SELECT mfa.artist_id,
               mf.library_id,
               mfa.role,
               count(DISTINCT mf.album_id) AS album_count,
               count(DISTINCT mf.id) AS count,
               sum(mf.size) AS size
        FROM media_file_artists mfa
        JOIN media_file mf ON mfa.media_file_id = mf.id
        WHERE mfa.artist_id IN (ROLE_IDS_PLACEHOLDER)
        GROUP BY mfa.artist_id, mf.library_id, mfa.role
    ),
    artist_total_counters AS (
        SELECT mfa.artist_id,
               mf.library_id,
               'total' AS role,
               count(DISTINCT mf.album_id) AS album_count,
               count(DISTINCT mf.id) AS count,
               sum(mf.size) AS size
        FROM media_file_artists mfa
        JOIN media_file mf ON mfa.media_file_id = mf.id
        WHERE mfa.artist_id IN (ROLE_IDS_PLACEHOLDER)
        GROUP BY mfa.artist_id, mf.library_id
    ),
    artist_participant_counter AS (
        SELECT mfa.artist_id,
               mf.library_id,
               'maincredit' AS role,
               count(DISTINCT mf.album_id) AS album_count,
               count(DISTINCT mf.id) AS count,
               sum(mf.size) AS size
        FROM media_file_artists mfa
        JOIN media_file mf ON mfa.media_file_id = mf.id
        WHERE mfa.artist_id IN (ROLE_IDS_PLACEHOLDER)
        AND mfa.role IN ('albumartist', 'artist')
        GROUP BY mfa.artist_id, mf.library_id
    ),
    combined_counters AS (
        SELECT artist_id, library_id, role, album_count, count, size FROM artist_role_counters
        UNION ALL
        SELECT artist_id, library_id, role, album_count, count, size FROM artist_total_counters
        UNION ALL
        SELECT artist_id, library_id, role, album_count, count, size FROM artist_participant_counter
    ),
    library_artist_counters AS (
        SELECT artist_id,
               library_id,
               json_group_object(
                       role,
                       json_object('a', album_count, 'm', count, 's', size)
               ) AS counters
        FROM combined_counters
        GROUP BY artist_id, library_id
    )
    UPDATE library_artist
    SET stats = coalesce((SELECT counters FROM library_artist_counters lac
                         WHERE lac.artist_id = library_artist.artist_id
                         AND lac.library_id = library_artist.library_id), '{}')
    WHERE library_artist.artist_id IN (ROLE_IDS_PLACEHOLDER);";

/// `RefreshStats`'s cleanup delete (`:583-585`).
#[must_use]
pub fn refresh_stats_cleanup_sql() -> Sql {
    (
        format!("DELETE FROM {LIBRARY_ARTIST_TABLE} WHERE stats = '{{}}'"),
        Vec::new(),
    )
}

/// `searchCfg` (`:616-630`): the artist search config — natural order by
/// id (the GROUP BY, `:622`), the text-search order (Go's literal
/// `sum(json_extract(...)) desc, name`, `:624`), MBID field
/// `mbz_artist_id` (`:625`). `scope` is [`search_scope`]'s resolved
/// library-id set: `None` is the fast-path (no Phase-1 filter,
/// `:626-629`), else the join-free [`artist_library_filter`].
#[must_use]
pub fn search_cfg(scope: Option<&[i64]>) -> (SearchConfig, Option<Sql>) {
    let cfg = SearchConfig {
        natural_order: format!("{TABLE}.id"),
        order_by: vec![
            "sum(json_extract(stats, '$.total.m')) desc".to_string(),
            "name".to_string(),
        ],
        mbid_fields: vec!["mbz_artist_id".to_string()],
    };
    (cfg, scope.map(artist_library_filter))
}

/// `getSearchStrategy` (`sql_search.go:27-33`) for the artist table: the
/// legacy/full-string LIKE family, the CJK LIKE fallback, or the FTS
/// default with its two-phase rowid core. Config flags
/// (`Search.Backend`/`Search.FullString`) are the caller's conf.
#[must_use]
pub fn get_search_strategy(
    query: &str,
    legacy_backend: bool,
    full_string: bool,
) -> Option<crate::sql_search::SearchStrategy> {
    use crate::sql_search::SearchStrategy;
    if legacy_backend || full_string {
        return like::new_legacy_search(TABLE, query, full_string).map(|s| SearchStrategy {
            clause: s.to_sql(),
            rowid_core: None,
        });
    }
    if fts::contains_cjk(query) {
        return like::new_like_search(TABLE, query).map(|s| SearchStrategy {
            clause: s.to_sql(),
            rowid_core: None,
        });
    }
    match fts::new_fts_search(TABLE, query) {
        Some(fts::SearchStrategy::Fts(search)) => Some(SearchStrategy {
            clause: search.to_sql(),
            rowid_core: Some(search.rowid_select(&[])),
        }),
        Some(fts::SearchStrategy::Like(cleaned)) => {
            like::new_like_search(TABLE, &cleaned).map(|s| SearchStrategy {
                clause: s.to_sql(),
                rowid_core: None,
            })
        }
        None => None,
    }
}

/// `Search`'s outcome — `doSearch`'s three shapes
/// (`sql_search.go:35-62`) for the artist base select.
#[derive(Debug, Clone, PartialEq)]
pub enum ArtistSearchOutcome {
    /// `sql_search.go:59`/`:47`/`None` strategy: no rows.
    NoResults,
    /// A materialized query plus its bound args, in bind order.
    Query(Sql),
}

/// `Search` (`:651-665`): consume the `library_id` filter into the
/// Phase-1 scope, build the stats base select, and dispatch in
/// `doSearch`'s order (`sql_search.go:41-61`): empty/`""` query → the
/// natural-order rowid page hydrated onto the base; UUID → the MBID
/// match; `<2` bytes → nothing; else the strategy (a two-phase rowid core
/// hydrates, a LIKE clause filters the base directly, ordered by
/// `searchCfg.OrderBy`).
///
/// Arg order — the contract the tests pin: base-join args (the
/// `user_library` user id) bind first, then the strategy/rowid-page args,
/// then the base's WHERE args, matching squirrel's join-before-where
/// rendering of Go's `sq.Join(ranked, rowidArgs…)`.
///
/// # Errors
/// [`SqlError`] when a `Repository` phase fails to materialize (it cannot
/// — the table is fixed).
#[allow(clippy::too_many_arguments)]
pub fn search_sql(
    user: &User,
    query: &str,
    options: Option<&QueryOptions>,
    scope: Option<&[i64]>,
    strategy: Option<crate::sql_search::SearchStrategy>,
) -> Result<ArtistSearchOutcome, SqlError> {
    let (cfg, library_filter) = search_cfg(scope);
    let empty_options = QueryOptions::default();
    let options = options.unwrap_or(&empty_options);

    let trimmed = query.trim();
    let query = trimmed.strip_suffix('*').unwrap_or(trimmed);

    // The base select: joins + filters + GROUP BY, no pagination — the
    // two-phase path strips it anyway (RemoveLimit/RemoveOffset,
    // sql_search.go:126-127) and the single-query paths never had it
    // applied to the base (Go's sq arrives with LIMIT/OFFSET set, but the
    // LIKE path keeps them — see below).
    let base = select_artist_sql(user, Some(options), None);
    let (_base_prefix, _base_join_args) = select_artist_prefix_with_args(user);
    let base_where = where_fragment(Some(options));

    // Empty query (OpenSubsonic `search3?query=""`) — natural-order rowid
    // page (sql_search.go:47-50).
    if query.is_empty() || query == "\"" {
        let rowid_core = Repository::new(TABLE)
            .select([format!("{TABLE}.rowid")])
            .order_by([cfg.natural_order.clone()]);
        let page = rowid_page_phase(rowid_core, TABLE, options, library_filter);
        let (page_sql, page_args) = page.to_sql()?;
        return Ok(ArtistSearchOutcome::Query(two_phase_sql(
            base, base_where, page_sql, page_args,
        )));
    }

    // UUID → MBID match (sql_search.go:54-57 via mbidExpr, :126-141).
    if let Some((mbid_sql, mbid_args)) = mbid_expr(TABLE, query, cfg.mbid_fields.clone()) {
        let (mut sql, mut args) = base;
        sql.push_str(&format!(" WHERE {TABLE}.missing = ? AND {mbid_sql}"));
        args.push(Value::Bool(false));
        args.extend(mbid_args);
        return Ok(ArtistSearchOutcome::Query((sql, args)));
    }

    // Sub-two-character query (byte length, sql_search.go:59-61).
    if query.len() < 2 {
        return Ok(ArtistSearchOutcome::NoResults);
    }

    let Some(strategy) = strategy else {
        return Ok(ArtistSearchOutcome::NoResults);
    };
    match strategy.rowid_core {
        Some(core) => {
            let page = rowid_page_phase(core, TABLE, options, library_filter);
            let (page_sql, page_args) = page.to_sql()?;
            Ok(ArtistSearchOutcome::Query(two_phase_sql(
                base, base_where, page_sql, page_args,
            )))
        }
        None => {
            // LIKE family (sql_search_like.go:16-24): the clause filters
            // the base, cfg.OrderBy orders it.
            let (clause_sql, clause_args) = strategy.clause;
            let (mut sql, mut args) = base;
            let missing = format!("{TABLE}.missing = ?");
            let filter_body = match &base_where {
                Some((where_sql, _)) => format!("{missing} AND ({where_sql}) AND {clause_sql}"),
                None => format!("{missing} AND {clause_sql}"),
            };
            sql.push_str(&format!(" WHERE {filter_body}"));
            args.push(Value::Bool(false));
            if let Some((_, filter_args)) = &base_where {
                args.extend(filter_args.iter().cloned());
            }
            args.extend(clause_args);
            sql.push_str(&format!(" ORDER BY {}", cfg.order_by.join(", ")));
            let (page_sql, page_args) = pagination_fragment(Some(options));
            sql.push_str(&page_sql);
            args.extend(page_args);
            Ok(ArtistSearchOutcome::Query((sql, args)))
        }
    }
}

/// The base select's prefix (SELECT … FROM … JOINs) and its join args —
/// [`select_artist_sql`] minus the WHERE/GROUP BY/pagination tail.
fn select_artist_prefix_with_args(user: &User) -> (String, Vec<Value>) {
    let (joins, args) = library_artist_join_fragment(user);
    (
        format!("SELECT {TABLE}.*, {LIBRARY_STATS_COLUMN} FROM {TABLE}{joins}"),
        args,
    )
}

/// `hydrateRowidPage` (`sql_search.go:108-121`): splice the ranked rowid
/// subquery into the base select's FROM (before its WHERE), strip the
/// base's pagination (the page query owns it), and order by the row
/// number so Phase 2 preserves Phase 1's order. The base's GROUP BY
/// stays (the aggregation survives the join).
fn two_phase_sql(
    base: Sql,
    base_where: Option<(String, Vec<Value>)>,
    page_sql: String,
    page_args: Vec<Value>,
) -> Sql {
    let (base_sql, mut base_args) = base;
    // Go strips the base's pagination before hydrating (RemoveLimit/
    // RemoveOffset, sql_search.go:126-127): the page query owns the
    // LIMIT/OFFSET and the base's trailing page args go with it.
    let (base_sql, dropped) = strip_pagination(&base_sql);
    for _ in 0..dropped {
        base_args.pop();
    }
    // Page args bind AT the join position (sql_search.go:141) — after the
    // base's JOIN args and ahead of its WHERE args (base_args is flat
    // [join_args, where_args]; base_where.1.len() splits them).
    let where_len = base_where.as_ref().map_or(0, |(_, a)| a.len());
    let split = base_args.len() - where_len;
    let mut args = base_args[..split].to_vec();
    args.extend(page_args);
    args.extend(base_args[split..].iter().cloned());
    let mut sql = base_sql;
    // Insert the JOIN before " WHERE" (or before " GROUP BY" when the base
    // has no filters).
    let marker = if sql.contains(" WHERE ") {
        " WHERE "
    } else {
        " GROUP BY"
    };
    let ranked =
        format!("(SELECT rowid as _rid, row_number() OVER () AS _rn FROM ({page_sql})) AS _ranked");
    let join_on = format!("{TABLE}.rowid = _ranked._rid");
    if let Some(pos) = sql.find(marker) {
        sql.replace_range(pos..pos, &format!(" INNER JOIN {ranked} ON {join_on}"));
    } else {
        sql.push_str(&format!(" INNER JOIN {ranked} ON {join_on}"));
    }
    sql.push_str(" ORDER BY _ranked._rn");
    let _ = base_where;
    (sql, args)
}

/// `RemoveLimit`/`RemoveOffset` (`sql_search.go:126-127`) on a rendered SQL
/// string: strip the trailing ` LIMIT ?`/` OFFSET ?` tail the base select
/// carried and report how many placeholders were dropped (one per arg to
/// pop).
#[must_use]
fn strip_pagination(sql: &str) -> (String, usize) {
    let mut s = sql.to_string();
    let mut dropped = 0;
    if s.ends_with(" OFFSET ?") {
        let cut = s.len() - " OFFSET ?".len();
        s.truncate(cut);
        dropped += 1;
    }
    if s.ends_with(" LIMIT ?") {
        let cut = s.len() - " LIMIT ?".len();
        s.truncate(cut);
        dropped += 1;
    }
    (s, dropped)
}

/// `requestedLibraryIDs` (`:702-710`): the `[]int` of an
/// `Eq{"library_id": ids}` filter. The crate's [`Filter::Eq`] carries one
/// value, so the port's spelling is a comma-joined id list — the caller
/// lifting the filter splits it back.
#[must_use]
pub fn requested_library_ids(filter: Option<&Filter>) -> Option<Vec<i64>> {
    let eq = match filter {
        Some(Filter::Eq { column, value }) if column == "library_id" => Some(value.clone()),
        _ => None,
    };
    match eq {
        Some(Value::Text(list)) => Some(
            list.split(',')
                .filter_map(|id| id.trim().parse::<i64>().ok())
                .collect(),
        ),
        _ => None,
    }
}

/// `isLibraryIDFilter` (`:712-720`): whether the filter is the
/// `library_id` key, so [`search_sql`] can consume it before it reaches
/// the bare artist table (which has no `library_id` column).
#[must_use]
pub fn is_library_id_filter(filter: Option<&Filter>) -> bool {
    matches!(filter, Some(Filter::Eq { column, .. }) if column == "library_id")
}

/// `searchScope` (`:667-700`): intersect the requested libraries with the
/// user's visible set. `None` for either result is the fast-path (no
/// Phase-1 filter). Comparison is by membership, not length
/// (`:691-699`) — requested ids may repeat. `visible: None` is Go's
/// fail-safe (`:673`): narrow to the request rather than widen.
#[must_use]
pub fn search_scope(visible: Option<&[i64]>, requested: Option<&[i64]>) -> Option<Vec<i64>> {
    let requested = match requested {
        None => return visible.map(|v| v.to_vec()),
        Some(r) => r,
    };
    let visible = match visible {
        None => return Some(requested.to_vec()),
        Some(v) => v,
    };
    if visible.iter().all(|id| requested.contains(id)) {
        return None;
    }
    Some(requested.to_vec())
}

/// `sortMappingsForRole` (`:704-716`): a copy of the shared stats sort
/// mappings narrowed to the sanitized role (falling back to `total`), in
/// `song_count`/`album_count`/`size` order. The caller grafts them over
/// [`sort_mapping`]'s shared values.
#[must_use]
pub fn sort_mappings_for_role(role_filter_value: Option<&str>) -> [String; 3] {
    let role = role_filter_value
        .and_then(sanitize_artist_stats_role)
        .unwrap_or_else(|| TOTAL_ROLE.to_string());
    [
        format!("sum(stats->>'{role}'->>'m')"),
        format!("sum(stats->>'{role}'->>'a')"),
        format!("sum(stats->>'{role}'->>'s')"),
    ]
}

/// `setSortMappings` (`:60-69`): the artist sort mappings, keyed by the
/// sanitized sort name — caller-supplied sort keys resolving to fixed
/// order-by expressions, never user text.
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<String> {
    match sort {
        "name" => Some("order_artist_name".to_string()),
        "starred_at" => Some("starred, starred_at".to_string()),
        "rated_at" => Some("rating, rated_at".to_string()),
        "song_count" => Some("stats->>'total'->>'m'".to_string()),
        "album_count" => Some("stats->>'total'->>'a'".to_string()),
        "size" => Some("stats->>'total'->>'s'".to_string()),
        "maincredit_song_count" => Some("sum(stats->>'maincredit'->>'m')".to_string()),
        "maincredit_album_count" => Some("sum(stats->>'maincredit'->>'a')".to_string()),
        "maincredit_size" => Some("sum(stats->>'maincredit'->>'s')".to_string()),
        _ => None,
    }
}

/// `registerModel`'s rest-facing half (`:52-69`): the `artist` table with
/// its filter mappings and field whitelist. `name`'s `fullTextFilter`
/// (`sql_restful.go:109-114`) is not a mapping here — it composes the
/// strategy dispatch ([`get_search_strategy`]) with `mbid_expr`, so the
/// search lane's [`search_sql`] owns it end-to-end.
#[must_use]
pub fn rest_repo() -> RestRepository {
    RestRepository::new(TABLE)
        .with_filter_mappings([
            ("id", crate::sql_restful::id_filter(TABLE)),
            ("starred", annotation_filter_fn("starred")),
            ("has_rating", annotation_filter_fn("rating")),
            ("role", role_filter_fn()),
            ("missing", boolean_filter_fn()),
            ("library_id", library_id_filter_fn()),
        ])
        .with_field_white_list([
            "ID",
            "Name",
            "SortArtistName",
            "OrderArtistName",
            "MbzArtistID",
            "Biography",
            "SmallImageUrl",
            "MediumImageUrl",
            "LargeImageUrl",
            "ExternalUrl",
            "SimilarArtists",
            "ExternalInfoUpdatedAt",
            "Missing",
            "UploadedImage",
            "CreatedAt",
            "UpdatedAt",
        ])
}

/// `annotationBoolFilter("starred")`/`("rating")` as a `filterFunc`
/// (`:57-58`; the raw fragment is `sql_annotations.rs`'s
/// `annotation_bool_filter`).
fn annotation_filter_fn(field: &'static str) -> crate::sql_restful::FilterFn {
    Box::new(move |_field: &str, value: &str| {
        Some(Filter::Raw {
            sql: crate::sql_annotations::annotation_bool_filter(field, value),
            raw_args: Vec::new(),
        })
    })
}

/// `roleFilter` as a `filterFunc` (`:60`).
fn role_filter_fn() -> crate::sql_restful::FilterFn {
    Box::new(move |_field: &str, value: &str| {
        let (sql, args) = role_filter(value);
        Some(Filter::Raw {
            sql,
            raw_args: args,
        })
    })
}

/// `booleanFilter` as a `filterFunc` (`:64`).
fn boolean_filter_fn() -> crate::sql_restful::FilterFn {
    Box::new(move |field: &str, value: &str| {
        Some(Filter::Eq {
            column: field.to_string(),
            value: Value::Bool(value.to_lowercase() == "true"),
        })
    })
}

/// `artistLibraryIdFilter` as a `filterFunc` (`:65`).
fn library_id_filter_fn() -> crate::sql_restful::FilterFn {
    Box::new(move |_field: &str, value: &str| {
        let (sql, args) = artist_library_id_filter(Value::Text(value.to_string()));
        Some(Filter::Raw {
            sql,
            raw_args: args,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlrepo::Value;

    #[test]
    fn sanitize_role_allowlists_total_and_registered_roles() {
        // Arrange/Act/Assert: sanitizeArtistStatsRole (:79-88).
        assert_eq!(
            sanitize_artist_stats_role("total"),
            Some("total".to_string())
        );
        assert_eq!(
            sanitize_artist_stats_role("albumartist"),
            Some("albumartist".to_string())
        );
        assert_eq!(sanitize_artist_stats_role(""), Some("total".to_string()));
        // The injection payloads of artist_repository_test.go:124-127.
        assert_eq!(
            sanitize_artist_stats_role("total'||(SELECT password FROM user LIMIT 1)||'"),
            None
        );
        assert_eq!(sanitize_artist_stats_role("wizard"), None);
        for role in ALL_ROLES {
            assert!(sanitize_artist_stats_role(role).is_some(), "{role}");
        }
    }

    #[test]
    fn role_filter_builds_the_json_path_only_for_safe_roles() {
        // Arrange: roleFilter (:71-77).
        // Act/Assert:
        let (sql, args) = role_filter("composer");
        assert_eq!(
            sql,
            "JSON_EXTRACT(library_artist.stats, '$.composer.m') IS NOT NULL"
        );
        assert!(args.is_empty());
        for bad in ["total", "", "wizard", "artist') OR 1=1--"] {
            let (sql, args) = role_filter(bad);
            assert_eq!(sql, FALSE_EXPR, "{bad:?}");
            assert!(args.is_empty());
        }
    }

    #[test]
    fn artist_library_filter_is_a_join_free_exists_with_limit_one() {
        // Arrange: artistLibraryFilter (:632-649) over two libraries.
        // Act:
        let (sql, args) = artist_library_filter(&[1, 2]);
        // Assert:
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM library_artist \
             WHERE library_artist.artist_id = artist.id \
             AND library_artist.library_id IN (?, ?) LIMIT 1)"
        );
        assert_eq!(args, vec![Value::Integer(1), Value::Integer(2)]);
        let (sql, args) = artist_library_filter(&[]);
        assert_eq!(sql, FALSE_EXPR);
        assert!(args.is_empty());
    }

    #[test]
    fn select_artist_pins_the_stats_column_joins_and_group_by() {
        // Arrange: selectArtist (:207-214) — admin (no user join), no
        // filters, no annotation.
        // Act:
        let (sql, args) = select_artist_sql(&None, None, None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT artist.*, JSON_GROUP_OBJECT(library_artist.library_id, \
             JSONB(library_artist.stats)) as library_stats_json FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             GROUP BY artist.id"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn select_artist_for_a_user_joins_user_library_and_binds_filters() {
        // Arrange: a non-admin user with a starred filter and a page.
        let user = Some("u1".to_string());
        let options = QueryOptions {
            max: 10,
            offset: 20,
            filters: Some(vec![Filter::Eq {
                column: "starred".to_string(),
                value: Value::Bool(true),
            }]),
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = select_artist_sql(&user, Some(&options), None);
        // Assert: squirrel's clause order — JOIN, WHERE, GROUP BY, LIMIT.
        assert_eq!(
            sql,
            "SELECT artist.*, JSON_GROUP_OBJECT(library_artist.library_id, \
             JSONB(library_artist.stats)) as library_stats_json FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             INNER JOIN user_library on user_library.library_id = library_artist.library_id \
             AND user_library.user_id = ? \
             WHERE starred = ? GROUP BY artist.id LIMIT ? OFFSET ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("u1".to_string()),
                Value::Bool(true),
                Value::Integer(10),
                Value::Integer(20),
            ]
        );
    }

    #[test]
    fn get_pins_the_id_clause_and_annotation_columns() {
        // Arrange: Get (:255-267) for a logged-in user — withAnnotation
        // appends its LEFT JOIN and columns (sql_annotations.go:63-77).
        let user = Some("u1".to_string());
        let ann = annotation_join("u1").expect("a logged-in user joins");
        // Act:
        let (sql, args) = get_sql("ar-1", &user, Some(&ann));
        // Assert:
        assert!(sql.starts_with("SELECT artist.*, JSON_GROUP_OBJECT("));
        assert!(sql.contains(" LEFT JOIN annotation ON"));
        assert!(sql.contains("coalesce(starred, 0) as starred"));
        assert!(sql.ends_with("GROUP BY artist.id"));
        assert_eq!(args[0], Value::Text("u1".to_string()), "user_library bind");
        assert_eq!(
            args[1],
            Value::Text("artist".to_string()),
            "annotation item_type bind"
        );
        assert_eq!(
            args[2],
            Value::Text("u1".to_string()),
            "annotation user bind"
        );
        assert_eq!(args[3], Value::Text("ar-1".to_string()));
        assert!(annotation_join(INVALID_USER_ID).is_none());
    }

    #[test]
    fn count_all_gates_the_annotation_join_on_the_filters() {
        // Arrange: CountAll (:217-227) — a starred filter needs the join,
        // a name filter does not.
        let user = Some("u1".to_string());
        let starred = QueryOptions {
            filters: Some(vec![Filter::Raw {
                sql: crate::sql_annotations::annotation_bool_filter("starred", "true"),
                raw_args: Vec::new(),
            }]),
            ..QueryOptions::default()
        };
        let name = QueryOptions {
            filters: Some(vec![Filter::Like {
                column: "artist.name".to_string(),
                pattern: "beat%".to_string(),
            }]),
            ..QueryOptions::default()
        };
        // Act:
        let (with_ann_sql, _) = count_all_sql(&user, Some(&starred));
        let (without_ann_sql, without_ann_args) = count_all_sql(&user, Some(&name));
        // Assert:
        assert_eq!(
            without_ann_sql,
            "SELECT count(distinct artist.id) as count FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             INNER JOIN user_library on user_library.library_id = library_artist.library_id \
             AND user_library.user_id = ? WHERE artist.name LIKE ?"
        );
        assert_eq!(
            without_ann_args,
            vec![
                Value::Text("u1".to_string()),
                Value::Text("beat%".to_string()),
            ]
        );
        // The starred branch swaps the LIKE clause for the annotation join.
        assert!(with_ann_sql.contains(" LEFT JOIN annotation ON"));
        assert!(!with_ann_sql.contains("artist.name LIKE ?"));
    }

    #[test]
    fn exists_pins_the_count_and_the_user_join() {
        // Arrange: Exists (:229-236) — admin sees no user_library join.
        // Act:
        let (admin_sql, admin_args) = exists_sql("ar-1", &None);
        // Assert:
        assert_eq!(
            admin_sql,
            "SELECT count(distinct artist.id) as exist FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             WHERE artist.id = ?"
        );
        assert_eq!(admin_args, vec![Value::Text("ar-1".to_string())]);
        let (user_sql, user_args) = exists_sql("ar-1", &Some("u1".to_string()));
        assert!(user_sql.contains("INNER JOIN user_library"));
        assert_eq!(
            user_args,
            vec![
                Value::Text("u1".to_string()),
                Value::Text("ar-1".to_string())
            ]
        );
    }

    #[test]
    fn format_full_text_and_search_normalized_pin_the_go_shapes() {
        // Arrange: PostMapArgs' derived columns (:109-110).
        // Act/Assert: formatFullText (sql_search.go:15-18).
        assert_eq!(format_full_text("legiao urbana", ""), " legiao urbana");
        assert_eq!(
            format_full_text("Eddie Van Halen", "Van Halen, Eddie"),
            " eddie halen van"
        );
        // NormalizeForFTS through nd_utils — the util owns the spelling;
        // the port only pins that the name feeds it.
        assert!(!search_normalized("Bjørk").is_empty());
    }

    #[test]
    fn put_cols_derives_and_drops_the_postmapargs_columns() {
        // Arrange: PostMapArgs (:98-121) — a full artist.
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "Eddie Van Halen".to_string(),
            sort_artist_name: "Van Halen, Eddie".to_string(),
            order_artist_name: "Eddie Van Halen".to_string(),
            mbz_artist_id: "mb-1".to_string(),
            ..ArtistRecord::default()
        };
        // Act:
        let cols = put_cols(&artist);
        // Assert:
        let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
        assert!(names.contains(&"full_text"));
        assert!(names.contains(&"search_normalized"));
        assert!(names.contains(&"mbz_artist_id"));
        // Empty sort_artist_name/mbz_artist_id are dropped (:113-120).
        let empty = ArtistRecord {
            name: "X".to_string(),
            ..ArtistRecord::default()
        };
        let empty_cols = put_cols(&empty);
        let empty_names: Vec<&str> = empty_cols.iter().map(|(c, _)| c.as_str()).collect();
        assert!(!empty_names.contains(&"sort_artist_name"));
        assert!(!empty_names.contains(&"mbz_artist_id"));
    }

    #[test]
    fn put_sql_update_leg_filters_cols_and_appends_updated_at() {
        // Arrange: UpdateExternalInfo (:247-253) — the fixed column list.
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "X".to_string(),
            biography: "b".to_string(),
            ..ArtistRecord::default()
        };
        // Act:
        let ((update_sql, update_args), (insert_sql, _)) =
            put_sql(&artist, 999, &EXTERNAL_INFO_COLS);
        // Assert: only the 7 external-info columns + appended updated_at,
        // never created_at.
        assert_eq!(
            update_sql,
            "UPDATE artist SET biography = ?, small_image_url = ?, medium_image_url = ?, \
             large_image_url = ?, external_url = ?, similar_artists = ?, \
             external_info_updated_at = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(update_args.len(), 9);
        assert_eq!(
            update_args[7],
            Value::Integer(999),
            "the appended updated_at"
        );
        assert_eq!(update_args[8], Value::Text("ar-1".to_string()));
        assert!(insert_sql.starts_with("INSERT INTO artist SET"));
    }

    #[test]
    fn marshal_similar_artists_omits_empty_keys() {
        // Arrange: dbSimilarArtist omitempty (:31-34, :102-107).
        // Act:
        let json = marshal_similar_artists(&[
            ("2".to_string(), "AC/DC".to_string()),
            (String::new(), "Test;With:Sep,Chars".to_string()),
        ]);
        // Assert:
        assert_eq!(
            json,
            "[{\"id\":\"2\",\"name\":\"AC/DC\"},{\"name\":\"Test;With:Sep,Chars\"}]"
        );
        assert_eq!(marshal_similar_artists(&[]), "[]");
    }

    #[test]
    fn get_all_ids_pins_the_light_pre_pass() {
        // Arrange: getAllIDs (:283-291) — id column only, no stats fetch.
        // Act:
        let (sql, args) = get_all_ids_sql(&None, None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT artist.id FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             GROUP BY artist.id"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn chunk_options_keep_the_sort_and_swap_in_the_id_chunk() {
        // Arrange: chunkOptions (artwork_hydration.go:37-50) — the
        // GetCursor hydrating fetch's per-chunk options.
        let base = QueryOptions {
            sort: "name".to_string(),
            order: "asc".to_string(),
            max: 50,
            offset: 100,
            filters: Some(vec![Filter::Eq {
                column: "artist.missing".to_string(),
                value: Value::Bool(false),
            }]),
            ..QueryOptions::default()
        };
        // Act:
        let chunked = chunk_options(Some(&base), &["ar-1".to_string(), "ar-2".to_string()]);
        // Assert: sort kept, pagination dropped, id chunk AND-joined.
        assert_eq!(chunked.sort, "name".to_string());
        assert_eq!(chunked.max, 0);
        assert_eq!(chunked.offset, 0);
        let (sql, args) = chunked.where_clause().expect("the chunk filter renders");
        assert_eq!(sql, "artist.missing = ? AND artist.id = ?");
        assert_eq!(
            args,
            vec![Value::Bool(false), Value::Text("ar-1,ar-2".to_string())]
        );
    }

    #[test]
    fn get_index_key_matches_prefixes_case_insensitively() {
        // Arrange: getIndexKey (:310-325) with a default-style IndexGroups.
        let mut groups = HashMap::new();
        groups.insert("The".to_string(), "T".to_string());
        groups.insert("A".to_string(), "A".to_string());
        // Act/Assert: a non-empty sort tag BECOMES the source under
        // PreferSortTags (cmp.Or picks the sort tag itself, :315-317) — the
        // order name never participates.
        assert_eq!(
            get_index_key(&groups, true, "the beatles", "foo", "ignored"),
            "#"
        );
        assert_eq!(
            get_index_key(&groups, true, "foo bar", "qux", "ignored"),
            "#"
        );
        // The sort tag matching a group prefix wins over the order name.
        assert_eq!(
            get_index_key(&groups, true, "the beatles", "A beatles", "ignored"),
            "A"
        );
        // No sort tags / flag off: order_artist_name wins (:313-314).
        assert_eq!(get_index_key(&groups, true, "foo bar", "", "ignored"), "#");
        assert_eq!(
            get_index_key(&groups, false, "kraftwerk", "zzz", "ignored"),
            "#"
        );
    }

    #[test]
    fn search_scope_narrows_only_when_the_request_is_a_real_subset() {
        // Arrange: searchScope (:667-700) semantics on plain slices.
        let visible = [1, 2, 3];
        // Act/Assert: a strict subset narrows.
        assert_eq!(
            search_scope(Some(&visible), Some(&[1, 2])),
            Some(vec![1, 2])
        );
        // A covering request fast-paths to None.
        assert_eq!(search_scope(Some(&visible), Some(&visible)), None);
        // No request: scope to the visible set.
        assert_eq!(search_scope(Some(&visible), None), Some(vec![1, 2, 3]));
        // No visibility info: fail safe to the request (:673).
        assert_eq!(search_scope(None, Some(&[7])), Some(vec![7]));
    }

    #[test]
    fn requested_and_is_library_id_filters_detect_the_key() {
        // Arrange: requestedLibraryIDs/isLibraryIDFilter (:702-720).
        let lib = Filter::Eq {
            column: "library_id".to_string(),
            value: Value::Text("1,2".to_string()),
        };
        let name = Filter::Eq {
            column: "name".to_string(),
            value: Value::Text("x".to_string()),
        };
        // Act/Assert:
        assert_eq!(requested_library_ids(Some(&lib)), Some(vec![1, 2]));
        assert!(is_library_id_filter(Some(&lib)));
        assert!(!is_library_id_filter(Some(&name)));
        assert_eq!(requested_library_ids(Some(&name)), None);
        assert_eq!(requested_library_ids(None), None);
    }

    #[test]
    fn sort_mappings_pin_the_go_literals_and_role_narrowing() {
        // Arrange: setSortMappings (:60-69) and sortMappingsForRole (:704-716).
        // Act/Assert:
        assert_eq!(sort_mapping("name"), Some("order_artist_name".to_string()));
        assert_eq!(
            sort_mapping("starred_at"),
            Some("starred, starred_at".to_string())
        );
        assert_eq!(
            sort_mapping("song_count"),
            Some("stats->>'total'->>'m'".to_string())
        );
        assert_eq!(
            sort_mapping("maincredit_song_count"),
            Some("sum(stats->>'maincredit'->>'m')".to_string())
        );
        // The role narrowing keeps valid paths and falls back for payloads.
        assert_eq!(
            sort_mappings_for_role(Some("composer"))[0],
            "sum(stats->>'composer'->>'m')"
        );
        assert_eq!(
            sort_mappings_for_role(Some("total') OR 1=1--"))[0],
            "sum(stats->>'total'->>'m')"
        );
        assert_eq!(
            sort_mappings_for_role(None)[0],
            "sum(stats->>'total'->>'m')"
        );
    }

    #[test]
    fn maintenance_statements_are_verbatim_and_arg_free() {
        // Arrange: markOrphansMissing (:408-419), markMissing (:421-439),
        // RefreshPlayCounts (:441-463), the RefreshStats id selects
        // (:469-489) and cleanup (:583-585), purgeEmpty (:370-405).
        // Act:
        let orphan = mark_orphans_missing_sql();
        let missing = mark_missing_sql();
        let play_counts = refresh_play_counts_sql();
        let ids_all = refresh_stats_ids_sql(true);
        let ids_touched = refresh_stats_ids_sql(false);
        let cleanup = refresh_stats_cleanup_sql();
        let (probe, delete) = purge_empty_sql();
        // Assert:
        assert!(orphan.0.starts_with("update artist set missing = true"));
        assert!(orphan.1.is_empty());
        assert!(missing.0.contains("artists_with_non_missing_albums"));
        assert!(missing.0.ends_with(");"));
        assert!(play_counts
            .0
            .contains("json_tree(participants, '$.artist')"));
        assert!(play_counts.0.ends_with("play_date  = excluded.play_date;"));
        assert_eq!(ids_all.0, "SELECT DISTINCT id FROM artist WHERE id <> ''");
        assert!(ids_touched.0.contains("last_scan_at FROM library"));
        assert_eq!(cleanup.0, "DELETE FROM library_artist WHERE stats = '{}'");
        assert!(probe.0.contains("uploaded_image != ''"));
        assert_eq!(
            delete.0,
            "DELETE FROM artist WHERE id not in (select artist_id from album_artists)"
        );
        for (_, args) in [
            &orphan,
            &missing,
            &play_counts,
            &ids_all,
            &ids_touched,
            &cleanup,
        ] {
            assert!(args.is_empty());
        }
    }

    #[test]
    fn refresh_stats_batch_replaces_all_four_placeholders() {
        // Arrange: the batch UPDATE (:491-578) for a 2-id batch.
        // Act:
        let Some((sql, args)) = refresh_stats_batch_sql(&["ar-1".to_string(), "ar-2".to_string()])
        else {
            panic!("a nonempty batch must render");
        };
        // Assert: 4 IN clauses × 2 ids, in per-clause order (:548-561).
        assert_eq!(args.len(), 8);
        assert_eq!(args[0], Value::Text("ar-1".to_string()));
        assert_eq!(args[1], Value::Text("ar-2".to_string()));
        assert_eq!(args[2], Value::Text("ar-1".to_string()));
        assert!(!sql.contains("ROLE_IDS_PLACEHOLDER"));
        assert_eq!(sql.matches("IN (?, ?)").count(), 4);
        assert!(sql.contains("'maincredit' AS role"));
        assert!(refresh_stats_batch_sql(&[]).is_none());
    }

    #[test]
    fn search_cfg_pins_the_artist_constants() {
        // Arrange: searchCfg (:616-630).
        // Act:
        let (cfg, no_scope) = search_cfg(None);
        let (_, scoped) = search_cfg(Some(&[1]));
        // Assert:
        assert_eq!(cfg.natural_order, "artist.id");
        assert_eq!(
            cfg.order_by,
            vec![
                "sum(json_extract(stats, '$.total.m')) desc".to_string(),
                "name".to_string()
            ]
        );
        assert_eq!(cfg.mbid_fields, vec!["mbz_artist_id".to_string()]);
        assert!(no_scope.is_none(), "the fast-path carries no filter");
        let Some((sql, args)) = scoped else {
            panic!("a scope renders a filter");
        };
        assert!(sql.starts_with("EXISTS (SELECT 1 FROM library_artist"));
        assert_eq!(args, vec![Value::Integer(1)]);
    }

    #[test]
    fn search_empty_query_hydrates_the_natural_order_rowid_page() {
        // Arrange: Search (:651-665) dispatching doSearch's empty branch
        // (sql_search.go:47-50) — a scoped user with pagination.
        let user = Some("u1".to_string());
        let options = QueryOptions {
            max: 5,
            offset: 10,
            ..QueryOptions::default()
        };
        // Act:
        let outcome = search_sql(&user, "  ", Some(&options), Some(&[1]), None)
            .expect("the empty query renders");
        // Assert:
        let ArtistSearchOutcome::Query((sql, args)) = outcome else {
            panic!("the empty query must produce a query");
        };
        assert!(sql.contains("row_number() OVER () AS _rn"));
        assert!(sql.contains("ORDER BY artist.id LIMIT ? OFFSET ?"));
        assert!(sql.contains("INNER JOIN user_library"));
        assert!(sql.ends_with("GROUP BY artist.id ORDER BY _ranked._rn"));
        // Base join arg (u1) binds before the page args.
        assert_eq!(args[0], Value::Text("u1".to_string()));
        assert!(
            args.contains(&Value::Integer(1)),
            "the scope library id binds"
        );
        assert!(args.contains(&Value::Integer(5)) && args.contains(&Value::Integer(10)));
    }

    #[test]
    fn search_uuid_query_matches_the_mbid_field() {
        // Arrange: the UUID branch (sql_search.go:54-57).
        // Act:
        let outcome = search_sql(
            &None,
            "F76D3C25-3B6B-4C61-83C7-8E0A0B6D0A47",
            None,
            None,
            None,
        )
        .expect("the UUID query renders");
        // Assert:
        let ArtistSearchOutcome::Query((sql, args)) = outcome else {
            panic!("the UUID query must produce a query");
        };
        assert!(sql.ends_with(
            "GROUP BY artist.id WHERE artist.missing = ? \
             AND (artist.mbz_artist_id = ?)"
        ));
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47".to_string()),
            ]
        );
    }

    #[test]
    fn search_short_and_nil_strategy_return_no_results() {
        // Arrange: sql_search.go:59-61 (min length, bytes) and the nil
        // strategy (:43-49).
        // Act:
        let short = search_sql(&None, "a*", None, None, None).expect("short resolves");
        let nil_strategy = search_sql(&None, "ab", None, None, None).expect("nil resolves");
        // Assert:
        assert_eq!(short, ArtistSearchOutcome::NoResults);
        assert_eq!(nil_strategy, ArtistSearchOutcome::NoResults);
    }

    #[test]
    fn search_like_strategy_filters_the_base_with_cfg_order() {
        // Arrange: likeSearch.execute (sql_search_like.go:16-24) — the
        // clause onto the base, cfg.OrderBy, no rowid page.
        let strategy = crate::sql_search::SearchStrategy {
            clause: (
                "artist.name LIKE ?".to_string(),
                vec![Value::Text("%beatles%".to_string())],
            ),
            rowid_core: None,
        };
        // Act:
        let outcome = search_sql(&None, "beatles", None, None, Some(strategy))
            .expect("the like strategy renders");
        // Assert:
        let ArtistSearchOutcome::Query((sql, args)) = outcome else {
            panic!("the LIKE query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT artist.*, JSON_GROUP_OBJECT(library_artist.library_id, \
             JSONB(library_artist.stats)) as library_stats_json FROM artist \
             INNER JOIN library_artist on library_artist.artist_id = artist.id \
             GROUP BY artist.id WHERE artist.missing = ? AND artist.name LIKE ? \
             ORDER BY sum(json_extract(stats, '$.total.m')) desc, name"
        );
        assert_eq!(
            args,
            vec![Value::Bool(false), Value::Text("%beatles%".to_string())]
        );
    }

    #[test]
    fn search_fts_strategy_hydrates_the_rowid_page() {
        // Arrange: ftsSearch.execute (sql_search_fts.go:278-286) — the
        // MATCH param binds at the page's JOIN position.
        let rowid_core = Repository::new(TABLE)
            .select([format!("{TABLE}.rowid")])
            .join(
                "artist_fts".to_string(),
                "artist_fts.rowid = artist.rowid AND artist_fts MATCH ?".to_string(),
                vec![Value::Text("(beatles OR beatles*)".to_string())],
            )
            .order_by(["artist_fts.rank".to_string()]);
        let strategy = crate::sql_search::SearchStrategy {
            clause: (
                "artist.rowid IN (SELECT rowid FROM artist_fts WHERE artist_fts MATCH ?)"
                    .to_string(),
                vec![Value::Text("(beatles OR beatles*)".to_string())],
            ),
            rowid_core: Some(rowid_core),
        };
        let options = QueryOptions {
            max: 10,
            ..QueryOptions::default()
        };
        // Act:
        let outcome = search_sql(&None, "beatles", Some(&options), None, Some(strategy))
            .expect("the fts strategy renders");
        // Assert:
        let ArtistSearchOutcome::Query((sql, args)) = outcome else {
            panic!("the FTS query must produce a query");
        };
        assert!(sql.contains("artist_fts MATCH ?"));
        assert!(sql.ends_with("ORDER BY _ranked._rn"));
        assert_eq!(args[0], Value::Text("(beatles OR beatles*)".to_string()));
        assert!(args.contains(&Value::Bool(false)));
        assert!(args.contains(&Value::Integer(10)));
    }

    #[test]
    fn filters_need_annotation_detects_the_annotation_filters() {
        // Arrange: filtersNeedAnnotation (sql_annotations.go:37-42) — the
        // crate's Filter spelling of the artist filter mappings.
        let starred = Filter::Raw {
            sql: crate::sql_annotations::annotation_bool_filter("starred", "true"),
            raw_args: Vec::new(),
        };
        let rating = Filter::Raw {
            sql: crate::sql_annotations::annotation_bool_filter("rating", "false"),
            raw_args: Vec::new(),
        };
        let name = Filter::Like {
            column: "artist.name".to_string(),
            pattern: "x%".to_string(),
        };
        let qualified = Filter::Eq {
            column: "annotation.rating".to_string(),
            value: Value::Integer(5),
        };
        // Act/Assert:
        assert!(filters_need_annotation(Some(&[starred])));
        assert!(filters_need_annotation(Some(&[rating])));
        assert!(filters_need_annotation(Some(&[qualified])));
        assert!(!filters_need_annotation(Some(&[name])));
        assert!(!filters_need_annotation(None));
    }

    #[test]
    fn rest_repo_maps_the_registered_filters() {
        // Arrange: registerModel (:52-69) — the mapping set.
        let repo = rest_repo();
        // Act:
        let idf = repo
            .parse_rest_options(Some(&crate::sql_restful::RestQueryOptions {
                filters: HashMap::from([("id".to_string(), "ar-9".to_string())])
                    .into_iter()
                    .collect(),
                ..crate::sql_restful::RestQueryOptions::default()
            }))
            .where_clause();
        let missing = repo
            .parse_rest_options(Some(&crate::sql_restful::RestQueryOptions {
                filters: HashMap::from([("missing".to_string(), "true".to_string())])
                    .into_iter()
                    .collect(),
                ..crate::sql_restful::RestQueryOptions::default()
            }))
            .where_clause();
        let role = repo
            .parse_rest_options(Some(&crate::sql_restful::RestQueryOptions {
                filters: HashMap::from([("role".to_string(), "wizard".to_string())])
                    .into_iter()
                    .collect(),
                ..crate::sql_restful::RestQueryOptions::default()
            }))
            .where_clause();
        // Assert:
        assert_eq!(
            idf,
            Some((
                "artist.id = ?".to_string(),
                vec![Value::Text("ar-9".to_string())]
            ))
        );
        assert_eq!(
            missing,
            Some(("missing = ?".to_string(), vec![Value::Bool(true)]))
        );
        let Some((role_sql, role_args)) = role else {
            panic!("the role filter renders");
        };
        assert_eq!(role_sql, FALSE_EXPR);
        assert!(role_args.is_empty());
    }
}
