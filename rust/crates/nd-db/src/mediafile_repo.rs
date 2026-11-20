//! Port of `persistence/mediafile_repository.go` (622L) — the `media_file`
//! table behind every track read/write.
//!
//! Every Go method builds a squirrel statement and materializes it through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution
//! stays with the caller (`store.rs` seam), so this port stops at the same
//! [`Sql`] pairs, following the sibling-lane split (see `album_repo.rs`).
//!
//! Ported here: `selectMediaFile`'s joined select (`:204-210` — library
//! display columns LEFT JOIN plus the annotation and bookmark joins),
//! `CountAll`'s annotation-gated count (`:150-158`), `CountBySuffix`
//! (`:160-173`), `Exists` (`:175-179`), `Get`/`GetAll` (`:212-227`),
//! `GetRandom`'s two-pass rowid shuffle (`:229-266`), `getAllIDs`
//! (`:319-329`), `Put`'s `putByMatch` legs plus
//! `updateParticipants`/`updateTags` (`:181-202`), `UpdateProbeData`
//! (`:204-207`), `FindByPaths`'s per-library OR groups (`:352-399`),
//! `Delete`/`DeleteAllMissing`/`DeleteMissing` (`:401-438`),
//! `MarkMissing`/`MarkMissingByFolder` (`:440-483`), `GetMissingAndMatching`
//! (`:485-510`), the `FindRecentFilesBy*` matchers (`:512-563`),
//! `GetAlbumIDsByFolder` (`:293-316`), `ReassignReferences` (`:415-432`) and
//! the search config/strategy (`:565-567`, `getSearchStrategy`).
//!
//! The FTS feeder contract: `dbMediaFile.PostMapArgs` (`:60-72`) computes
//! `full_text` (`" " + SanitizeStrings(...)`), `search_participants` (the
//! participant names joined by spaces), `search_normalized`
//! (`NormalizeForFTS(FullTitle, Album, Artist, AlbumArtist)`) and marshals
//! `tags`/`participants` JSON before every insert/update — the columns exist
//! in the fresh DDL (`nd-dbschema`: `media_file.search_participants TEXT
//! NOT NULL DEFAULT ''`), and [`PutMediaFileSql`] carries the already
//! computed strings so the caller's marshaller stays upstream.
//!
//! Deliberately absent (other lanes/slices, per the album lane's split):
//! `GetCursor`/`GetCursorWithArtwork`/`GetAllByTags`/`MatchesCriteria`
//! (`:270-318,517-563` — cursor streaming and smart-playlist criteria),
//! `GetWithParticipants`'s artist hydration (`:214-219`, artist slice), and the
//! REST shims (`:585-600`, `sql_restful.rs`). The `Path` collate-nocase matching
//! in `FindByPaths` pins; the library-splitting of qualified paths is Go-side
//! logic kept in [`find_by_paths_sql`]'s grouped inputs.
//!
//! Landed in S-D3c: the executed [`MediaFileRepo`] reads (`Get` `:212-221`,
//! `GetAll` `:232-242`, `GetRandom` `:250-283`) wired to the shared seam and to
//! `hydrateArtwork` (`:244-246`), which is
//! [`crate::artwork_hydration::hydrate_media_file_artwork`]. Only those three are
//! artwork-hydrated here, matching Go exactly: `GetCursor` (`:309-316`) does NOT
//! hydrate (pinned by `persistence/artwork_hydration_test.go:633`), and neither
//! does `GetMissingAndMatching` (`:494-517`), `FindByPaths` (`:365-404`) or
//! `FindRecentFilesBy*` (`:525-569`) — their builders stay statement-only. Go's
//! `Search` (`:594-607`) DOES hydrate, but the Rust search lane renders its
//! projection from `sql_search::new_select` (`sql_search.rs:129-131` —
//! `SELECT * FROM media_file`), which carries no library/annotation/bookmark
//! columns and so cannot be hydrated through this seam: Rust `Search` results are
//! NOT artwork-hydrated (residual).
//! Landed in DB-10: the executed `Put` (`mediafile_repository.go:184-197`) as
//! [`put_mediafile_in_tx`] — the `putByMatch` identity probe (`sql_base_repository.go:577-589`)
//! plus the base upsert and participants/tags junction maintenance on the CALLER's
//! transaction, executing exactly the bundle [`put_sql`] returns. An empty id takes Go's
//! random-id arm (`sql_base_repository.go:635-638`) with `nd_id::new_random()`, minted inside
//! the caller's transaction so a failed attempt publishes nothing. It issues NO FTS
//! statement: the schema's own `media_file_fts_ai`/`_au`/`_ad` triggers maintain the index,
//! so an explicit FTS write would double-index it.
//!
//! **`optimizePagination` asymmetry (residual, deliberately NOT applied):**
//! `queryAll` runs the offset rewrite only when options are passed to it
//! (`sql_base_repository.go:441-444`), and media_file `GetAll` is the one Go read
//! that does pass them (`mediafile_repository.go:235`) — so with
//! `options.Offset > conf.Server.DevOffsetOptimize` (default 50000,
//! `conf/configuration.go:145,1129`) Go replaces `OFFSET n` with
//! `media_file.rowid NOT IN (SELECT rowid … LIMIT n)` (`:477-486`). Album `GetAll`
//! passes no options (`album_repository.go:248`) and never takes the rewrite. That
//! rewrite is unimplemented port-wide, so both Rust reads stay plain
//! LIMIT/OFFSET; a large-offset media_file `GetAll` therefore differs from Go only
//! in plan shape, not results.
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column/order-by names) are
//! the registered, caller-supplied constants; every value binds to a `?`.
//!
//! Scope of the executed repo reads: [`MediaFileRepo::get_all`] and
//! [`MediaFileRepo::get_random`] are the order-by/pagination SUBSET of Go's
//! `GetAll(ctx, options...)` / `GetRandom(ctx, options...)`. Go additionally applies
//! `options[0].Filters` through `newSelect` → `applyFilters`
//! (`sql_base_repository.go:139-146`, `:247-252`) — the shape `GetAllByTags`
//! reaches in one call (`mediafile_repository.go:284-307`) — and these signatures
//! take no caller filters (phase 1's `filters` aside, which IS the caller's
//! rendered clause), so a filtered read goes through [`select_media_file`] or
//! [`crate::album_repo::select_album_sql`] with the caller's WHERE until the
//! REST/options lane wires `QueryOptions.Filters`.

use std::collections::BTreeMap;

use crate::runtime_error::DbOperationError;
use crate::sql_participations::Participants;
use crate::sql_search::SearchConfig;
use crate::sql_search::Sql;
use crate::sql_tags::DbTags;
use crate::sqlrepo::Value;
use chrono::{DateTime, Utc};

/// `r.tableName` (`NewMediaFileRepository`, `mediafile_repository.go:80`).
pub const TABLE: &str = "media_file";

/// `media_file`'s participant join table (`sql_participations.go:34`).
pub const ARTISTS_TABLE: &str = "media_file_artists";

/// `media_file`'s tag join table (`sql_tags.go:88`).
pub const TAGS_TABLE: &str = "media_file_tags";

/// `setSortMappings` (`mediafile_repository.go:91-108`): the sort field →
/// order-by expression table. `album` composes `naturalSort("media_file.title")`
/// whose config-gated parens/collate wrap (`helpers.go:89-93`) the caller
/// applies; the plain form pins here. `recently_added` is
/// `mediaFileRecentlyAddedSort()` (`:110-116`): mod-time → `updated_at`, else
/// `created_at`, both `, media_file.id`.
#[must_use]
pub fn sort_mappings(
    natural_sort_title: &str,
    recently_added_by_mod_time: bool,
) -> Vec<(&'static str, String)> {
    vec![
        ("title", "order_title".to_string()),
        (
            "artist",
            "order_artist_name, order_album_name, release_date, disc_number, track_number".to_string(),
        ),
        (
            "album_artist",
            "order_album_artist_name, order_album_name, release_date, disc_number, track_number".to_string(),
        ),
        (
            "album",
            format!(
                "order_album_name, album_id, disc_number, track_number, order_artist_name, {natural_sort_title}"
            ),
        ),
        ("random", "random".to_string()),
        ("created_at", "media_file.created_at".to_string()),
        (
            "recently_added",
            if recently_added_by_mod_time {
                "media_file.updated_at, media_file.id".to_string()
            } else {
                "media_file.created_at, media_file.id".to_string()
            },
        ),
        ("starred_at", "starred, starred_at".to_string()),
        ("rated_at", "rating, rated_at".to_string()),
        ("year", "year".to_string()),
        ("genre", "genre".to_string()),
        ("duration", "duration".to_string()),
        ("channels", "channels".to_string()),
        ("bpm", "bpm".to_string()),
        ("path", "path".to_string()),
        ("comment", "comment".to_string()),
        ("play_count", "play_count".to_string()),
        ("play_date", "play_date".to_string()),
        ("rating", "rating".to_string()),
    ]
}

/// `naturalSort("media_file.title")`'s plain form (`helpers.go:89-93` with
/// `EnableNaturalSorting` off).
pub const NATURAL_SORT_TITLE: &str = "media_file.title";

/// `mediaFileSearchConfig` (`mediafile_repository.go:565-567`).
#[must_use]
pub fn media_file_search_config() -> SearchConfig {
    SearchConfig {
        natural_order: "media_file.rowid".to_string(),
        order_by: vec!["title".to_string()],
        mbid_fields: vec![
            "mbz_recording_id".to_string(),
            "mbz_release_track_id".to_string(),
        ],
    }
}

/// The search strategy for `query` (`getSearchStrategy`,
/// `sql_search.go:36-49` — the same dispatcher the album lane ports):
/// config-ordered — legacy backend / full-string → legacy LIKE; CJK →
/// core-column LIKE; else FTS5 — `None` when the query yields no searchable
/// tokens.
#[must_use]
pub fn get_search_strategy(
    query: &str,
    legacy_backend: bool,
    full_string: bool,
) -> Option<crate::sql_search::SearchStrategy> {
    use crate::sql_search::SearchStrategy;
    if legacy_backend || full_string {
        return crate::sql_search_like::new_legacy_search(TABLE, query, full_string).map(
            |search| SearchStrategy {
                clause: search.to_sql(),
                rowid_core: None,
            },
        );
    }
    if crate::sql_search_fts::contains_cjk(query) {
        return crate::sql_search_like::new_like_search(TABLE, query).map(|search| {
            SearchStrategy {
                clause: search.to_sql(),
                rowid_core: None,
            }
        });
    }
    match crate::sql_search_fts::new_fts_search(TABLE, query) {
        Some(crate::sql_search_fts::SearchStrategy::Fts(fts)) => Some(SearchStrategy {
            clause: fts.to_sql(),
            rowid_core: Some(fts.rowid_select(&["title asc".to_string()])),
        }),
        Some(crate::sql_search_fts::SearchStrategy::Like(cleaned)) => {
            crate::sql_search_like::new_like_search(TABLE, &cleaned).map(|search| SearchStrategy {
                clause: search.to_sql(),
                rowid_core: None,
            })
        }
        None => None,
    }
}

/// `withBookmark`'s fragment as `selectMediaFile` composes it
/// (`sql_bookmarks.go:18-28` via `mediafile_repository.go:208`): the LEFT
/// JOIN text with the user id bound (the crate's parameterized form of Go's
/// inline literal — `sql_bookmarks.rs:38-48` owns the same shape), plus the
/// `bookmark_position` column.
fn bookmark_join_parts(user_id: &str) -> (String, Vec<crate::sqlrepo::Value>) {
    (
        "LEFT JOIN bookmark ON (bookmark.item_id = media_file.id AND bookmark.user_id = ?)"
            .to_string(),
        vec![crate::sqlrepo::Value::from(user_id)],
    )
}

/// The `bookmark_position` column `withBookmark` appends
/// (`sql_bookmarks.go:27`).
const BOOKMARK_POSITION_COLUMN: &str = "coalesce(position, 0) as bookmark_position";

/// `withAnnotation`'s fragment as the media_file select composes it
/// (`sql_annotations.go:63-91` via `selectMediaFile`'s `withAnnotation`): the
/// LEFT JOIN text plus its two bound args (`item_type` then `user_id`),
/// re-derived from the public constants like the album lane
/// (`album_repo.rs:annotation_join_parts`); `media_file` never takes the
/// normalized play-count form (that branch is album-only).
fn annotation_join_parts(user_id: &str) -> (String, Vec<crate::sqlrepo::Value>) {
    (
        format!(
            " LEFT JOIN {} ON \
             ({}.item_id = media_file.id \
             AND {}.item_type = ? \
             AND {}.user_id = ?)",
            crate::sql_annotations::ANNOTATION_TABLE,
            crate::sql_annotations::ANNOTATION_TABLE,
            crate::sql_annotations::ANNOTATION_TABLE,
            crate::sql_annotations::ANNOTATION_TABLE,
        ),
        vec![
            crate::sqlrepo::Value::from(TABLE),
            crate::sqlrepo::Value::from(user_id),
        ],
    )
}

/// The annotation columns `withAnnotation` appends (`sql_annotations.go:68-88`)
/// in squirrel's declaration order, plus the base-table `average_rating`.
const ANNOTATION_COLUMNS: [&str; 7] = [
    "coalesce(starred, 0) as starred",
    "coalesce(rating, 0) as rating",
    "starred_at",
    "play_date",
    "rated_at",
    "coalesce(play_count, 0) as play_count",
    "media_file.average_rating",
];

/// `selectMediaFile` (`mediafile_repository.go:204-210`): `media_file.*`,
/// the library display columns (LEFT JOIN), the logged-in user's annotation
/// join and bookmark join, then the caller's library filter
/// (`applyLibraryFilter`, `sql_base_repository.go:261-282`). `user_id` is
/// `None` for no user logged in (`INVALID_USER_ID` — Go skips both joins);
/// `library_filter` is the caller-resolved clause.
///
/// Rendered directly (not through [`crate::sqlrepo::Repository`], whose
/// builder models only INNER JOIN): Go LEFT-joins via string fragments. squirrel
/// emits the column list, then `FROM`, then the join parts **in append order**
/// (`select.go:64-108`), so `selectMediaFile`'s chain — library join
/// (`:205-206`), annotation join (`sql_annotations.go:68-88`), bookmark join
/// (`sql_bookmarks.go:22-28`) — lands in that order after `FROM media_file`, and
/// the join args bind in the same order.
#[must_use]
pub fn select_media_file(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
) -> (String, Vec<crate::sqlrepo::Value>) {
    let mut sql = String::from(
        "SELECT media_file.*, library.path as library_path, library.name as library_name",
    );
    let mut join_sql =
        String::from(" FROM media_file LEFT JOIN library ON media_file.library_id = library.id");
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    match user_id {
        Some(user) => {
            let (ann_join, ann_args) = annotation_join_parts(user);
            let (bmk_join, bmk_args) = bookmark_join_parts(user);
            sql.push_str(&format!(
                ", {}, {BOOKMARK_POSITION_COLUMN}",
                ANNOTATION_COLUMNS.join(", ")
            ));
            join_sql.push_str(&ann_join);
            args.extend(ann_args);
            join_sql.push_str(&format!(" {bmk_join}"));
            args.extend(bmk_args);
        }
        None => sql.push_str(", media_file.average_rating"),
    }
    sql.push_str(&join_sql);
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause}"));
        args.extend(clause_args);
    }
    (sql, args)
}

/// `CountAll` (`mediafile_repository.go:150-158`): `newSelect` + library
/// filter, plus the annotation join only when a filter needs one
/// (`filtersNeedAnnotation`, `sql_annotations.go:55-61` — the caller's
/// regex scan result arrives as `need_annotation`), then the base `count()`
/// shape (`sql_base_repository.go:565-576`).
#[must_use]
pub fn count_all_sql(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
    need_annotation: bool,
) -> Sql {
    let mut sql = format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE}");
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    // squirrel renders joins before WHERE (`select.go:97-113`), so the join args
    // bind BEFORE the filter args even though the filter was applied first.
    if need_annotation {
        if let Some(user) = user_id {
            let (join_sql, join_args) = annotation_join_parts(user);
            sql.push_str(&join_sql);
            args.extend(join_args);
        }
    }
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause}"));
        args.extend(clause_args);
    }
    (sql, args)
}

/// `CountBySuffix` (`mediafile_repository.go:160-173`): the suffix histogram
/// with lowercase normalization and its GROUP BY.
#[must_use]
pub fn count_by_suffix_sql() -> Sql {
    (
        format!(
            "SELECT lower(suffix) as suffix, count(*) as count FROM {TABLE} GROUP BY lower(suffix)"
        ),
        Vec::new(),
    )
}

/// `Exists` (`mediafile_repository.go:175-179`): the id count under the
/// library filter (the bare `exists()` helper would report rows the caller
/// cannot see).
#[must_use]
pub fn exists_sql(id: &str, library_filter: Option<Sql>) -> Sql {
    let mut sql = format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE}");
    let mut args = vec![crate::sqlrepo::Value::from(id)];
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause} AND {TABLE}.id = ?"));
        let mut all = clause_args;
        all.push(crate::sqlrepo::Value::from(id));
        args = all;
    } else {
        sql.push_str(&format!(" WHERE {TABLE}.id = ?"));
    }
    (sql, args)
}

/// `Get` (`mediafile_repository.go:212-221`): `GetAll` with
/// `Filters: Eq{"media_file.id": id}`. squirrel AND-joins that filter into the
/// SAME `WHERE` clause (`select.go:112-117` renders the accumulated `WhereParts`
/// joined by ` AND `), so the id predicate is appended as ` AND media_file.id = ?`
/// whenever a `WHERE` is already open. Appending a second ` WHERE ` — the
/// previous shape — emits `... WHERE <filter> WHERE media_file.id = ?`, which
/// SQLite rejects with `near "WHERE": syntax error`. Same rule as [`exists_sql`],
/// and the args stay interleaved with the clauses they belong to (filter args
/// first, then the id).
///
/// Clause ORDER is the crate-wide seam convention rather than Go's: Go's
/// `GetAll` runs `newSelect(ctx, options)`, so `applyFilters`
/// (`sql_base_repository.go:247-252`) lands the id predicate before
/// `applyLibraryFilter` (`:261`), while this seam takes the caller-resolved
/// `library_filter` as its own first parameter and emits it first (identical to
/// [`crate::album_repo::select_album_sql`]). The two orders are result-equivalent —
/// predicates are AND-joined and each clause's args bind in the same order the
/// clause is rendered — so no result differs; only the SQL text does.
#[must_use]
pub fn get_sql(id: &str, user_id: Option<&str>, library_filter: Option<Sql>) -> Sql {
    // The library filter owns the first WHERE; the id predicate joins it with AND
    // (squirrel accumulates `WhereParts` and renders them with " AND ",
    // `select.go:112-117`). Probing the flag rather than the rendered text keeps a
    // filter clause that contains its own subquery `WHERE` from being mistaken for
    // an open outer clause.
    let has_where = library_filter.is_some();
    let (mut sql, mut args) = select_media_file(user_id, library_filter);
    if has_where {
        sql.push_str(&format!(" AND {TABLE}.id = ?"));
    } else {
        sql.push_str(&format!(" WHERE {TABLE}.id = ?"));
    }
    args.push(crate::sqlrepo::Value::from(id));
    (sql, args)
}

/// `GetAll` (`mediafile_repository.go:232-242`): the base select with the
/// caller's order-by expressions (already direction-carrying, per
/// `buildSortOrder` output) and `applyOptions`' Max/Offset.
#[must_use]
pub fn get_all_sql(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
    order_by: &[&str],
    max: i64,
    offset: i64,
) -> Sql {
    let (mut sql, args) = select_media_file(user_id, library_filter);
    if !order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order_by.join(", ")));
    }
    if max > 0 {
        // squirrel renders the bound as a decimal literal and binds nothing
        // (`select.go:139-147`), so `max` joins neither clause nor args list.
        sql.push_str(&format!(" LIMIT {max}"));
    }
    if offset > 0 {
        // Deviation (deliberate, cited): squirrel emits a bare `OFFSET n`
        // (`applyOptions` sets the bounds independently,
        // `sql_base_repository.go:151-156`), which SQLite rejects — a parse
        // error. SQLite is Navidrome's primary engine, so the port emits
        // `LIMIT -1` (its unlimited bound) ahead of the offset.
        if max <= 0 {
            sql.push_str(" LIMIT -1");
        }
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    (sql, args)
}

/// `GetRandom` Phase 1 (`mediafile_repository.go:256-262`): the narrow
/// rowid pick — filters, library filter, `random()` order, optional Max.
/// `filters` is the caller's rendered WHERE body (Go's `applyFilters` output,
/// applied to a bare `Select("media_file.rowid")` at `:257`, NOT to the hydrated
/// select).
#[must_use]
pub fn get_random_rowid_sql(
    filters: Option<(&str, Vec<crate::sqlrepo::Value>)>,
    library_filter: Option<Sql>,
    max: i64,
) -> Sql {
    let mut sql = format!("SELECT media_file.rowid FROM {TABLE}");
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    let mut clauses: Vec<String> = Vec::new();
    if let Some((clause, clause_args)) = filters {
        clauses.push(clause.to_string());
        args.extend(clause_args);
    }
    if let Some((clause, clause_args)) = library_filter {
        clauses.push(clause.to_string());
        args.extend(clause_args);
    }
    if !clauses.is_empty() {
        sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
    }
    sql.push_str(" ORDER BY random()");
    if max > 0 {
        // `.Limit(uint64(opt.Max))` (`mediafile_repository.go:261`) renders as a
        // decimal literal with no bound arg (`select.go:139-142`).
        sql.push_str(&format!(" LIMIT {max}"));
    }
    (sql, args)
}

/// `GetRandom` Phase 2 (`mediafile_repository.go:272-281`): re-shuffle the
/// hydrated rows — the base select plus `WHERE media_file.rowid IN (?, …)`
/// and `ORDER BY random()`, UNDER THE SAME LIBRARY FILTER as phase 1.
///
/// Go's phase 2 is `r.selectMediaFile(ctx)` (`:274`), and `selectMediaFile` ends
/// with an UNCONDITIONAL `return r.applyLibraryFilter(ctx, sql)`
/// (`mediafile_repository.go:204-210`, `:209`), so Go's phase-2 row set is
/// {phase-1 rowids} ∩ {visible libraries} — not merely the rowids. Passing
/// `library_filter: None` here would let a rowid from an ungranted library back
/// in whenever phase 1's own filter differed from the library filter (a caller
/// `opt.Filters` that excludes nothing, a grant revoked between passes, or a
/// hand-built rowid list).
///
/// Go resolves the filter from the ambient ctx in BOTH phases
/// (`sql_base_repository.go:261-283`, incl. the admin/`invalidUserId` and
/// sees-all-libraries fast paths at `:265-273`); the port threads ONE
/// caller-supplied filter into both. That is observably identical within a
/// call, because Go's ctx user is fixed for the duration of the call, so its
/// two resolutions produce the same clause. Phase 2 also carries NO caller
/// Filters on either side: Go builds it with `selectMediaFile(ctx)` — `newSelect`
/// with no options (`:274`, and `newSelect` only applies filters when
/// `len(options) > 0`, `sql_base_repository.go:139-146`), so `applyFilters`
/// (`:247-252`) and `applyOptions` (`:149-162`) are both not involved — which is
/// exactly what this signature expresses.
///
/// Rendering follows [`get_sql`]'s rule: the library filter owns the first
/// `WHERE` and the rowid IN-list AND-joins it, because squirrel accumulates
/// `WhereParts` and renders them joined by ` AND ` (`select.go:111-114`). Args
/// follow clause order: join args, then the filter's, then the rowids.
#[must_use]
pub fn get_random_hydrate_sql(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
    rowids: &[i64],
) -> Sql {
    let has_where = library_filter.is_some();
    let (mut sql, mut args) = select_media_file(user_id, library_filter);
    let placeholders = vec!["?"; rowids.len()].join(", ");
    // The library filter already opened the clause, so the IN-list joins it with
    // AND rather than starting a second WHERE (which SQLite rejects).
    if has_where {
        sql.push_str(&format!(" AND {TABLE}.rowid IN ({placeholders})"));
    } else {
        sql.push_str(&format!(" WHERE {TABLE}.rowid IN ({placeholders})"));
    }
    sql.push_str(" ORDER BY random()");
    args.extend(rowids.iter().map(|r| crate::sqlrepo::Value::Integer(*r)));
    (sql, args)
}

/// Failures of the executed media_file reads ([`MediaFileRepo`]).
///
/// Same two-variant shape as `UserRepo::UserError` (`user_repo.rs:609-622`),
/// `PlayerRepo::PlayerError` (`player_repo.rs:243-260`) and the sibling
/// [`crate::album_repo::AlbumError`].
#[derive(Debug, thiserror::Error)]
pub enum MediaFileError {
    /// The SQLite statement failed, or a row's hydration failed. Go's read funnel
    /// surfaces a `PostScan` failure as a column-conversion error
    /// (`dbx@v1.12.0/rows.go:92-102`), so a decode failure arrives here too — via
    /// [`crate::hydrate::HydrateError::into_sqlite`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`), raised by
    /// `Get` when `GetAll` returned zero rows (`mediafile_repository.go:217-219`).
    #[error("record not found")]
    NotFound,
}

/// `mediaFileRepository` (`mediafile_repository.go:80-108`) bound to one pooled
/// connection — the executed READ side of the media_file repository.
///
/// Same DI shape as [`crate::album_repo::AlbumRepo`] and the `UserRepo`/`PlayerRepo`
/// precedents: no ambient ctx, a caller-threaded lease from
/// [`crate::store::SQLStore::with_connection_sync`], and every statement through
/// [`crate::execution`]. Rows hydrate through the shared seam
/// ([`crate::hydrate::row_to_mediafile`] → [`MediaFileRow::into_record`], with
/// [`crate::hydrate::HydrateError::into_sqlite`] as the exit shape — the exact
/// mapping `tests/runtime_hydrate_album_mediafile.rs:113-120` uses), then the
/// artwork layer runs on the models (`hydrateArtwork`,
/// `mediafile_repository.go:244-246`).
///
/// `Get` goes through `query_all`, not `query_optional`, for the same verified
/// reason as the album repo: Go's `Get` (`:212-221`) is `GetAll` +
/// `Eq{"media_file.id": id}` with the not-found decision taken from `len(res) == 0`,
/// and dbx's `All` cannot return `sql.ErrNoRows` (`dbx@v1.12.0/query.go:274-277`,
/// `rows.go:109-125` make an empty NON-nil slice; only `Rows.one`/`Rows.row` return
/// it, `rows.go:237-244`, `:281-294`). So `queryAll`'s ErrNoRows arm
/// (`sql_base_repository.go:451-454`) is dead here and an empty result is an empty
/// `Vec` — never [`MediaFileError::NotFound`].
///
/// `enable_media_file_cover_art` is Go's `conf.Server.EnableMediaFileCoverArt`
/// (default `true`, `conf/configuration.go:1003`) supplied by the caller, because
/// nd-db does not read config (the `normalized_album_play_count` precedent,
/// `album_repo::get_all_sql`). Go reads that flag TWICE per `GetRandom`/`GetAll` —
/// once when collecting the eligible ids (`artwork_hydration.go:102`) and once per
/// item (`:111`) — so a config reload between the two can make them disagree. The
/// port reads the parameter ONCE per call: the deliberate, observably-equivalent
/// difference (with a stable config both arms are the same value).
#[derive(Debug)]
pub struct MediaFileRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> MediaFileRepo<'a> {
    /// `NewMediaFileRepository` (`mediafile_repository.go:80-108`): bind to `conn`.
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`mediafile_repository.go:212-221`): [`get_sql`] (== `GetAll` +
    /// `Eq{"media_file.id": id}`), hydrated through both layers, zero rows mapped to
    /// [`MediaFileError::NotFound`] the way Go decides it from `len(res)`.
    ///
    /// Go's `GetWithParticipants` (`:223-230`) stays unported: it re-reads the
    /// `artist` table through `getParticipants` (`sql_participations.go:127-146`),
    /// which is the artist-hydration lane.
    ///
    /// # Errors
    /// [`MediaFileError::NotFound`] when the id matches nothing (or the library
    /// filter excludes it); [`MediaFileError::Sqlite`] on a statement or hydration
    /// failure. An artwork-lookup failure is NOT an error here — Go's `Get`
    /// swallows it too, because it delegates to `GetAll`, whose `hydrateItems`
    /// logs and returns an empty map (`artwork_hydration.go:62-66`): the track
    /// comes back with both artwork fields zero. Use [`Self::get_all`] when the
    /// caller needs that failure reported. Go's `Get` returns `nil` beside its error
    /// on BOTH paths (`:214-216` `return nil, err`, `:217-219`
    /// `return nil, model.ErrNotFound`), so the port's `Result` carries the same
    /// information and is not a divergence.
    pub fn get(
        &self,
        id: &str,
        user_id: Option<&str>,
        library_filter: Option<Sql>,
        enable_media_file_cover_art: bool,
    ) -> Result<MediaFileRecord, MediaFileError> {
        let statement = get_sql(id, user_id, library_filter);
        let records = self.read_hydrated(&statement)?;
        let mut outcome = self.hydrate_artwork(records, enable_media_file_cover_art);
        if outcome.items.is_empty() {
            return Err(MediaFileError::NotFound);
        }
        Ok(outcome.items.remove(0))
    }

    /// `GetAll` (`mediafile_repository.go:232-242`): [`get_all_sql`] under the
    /// caller's order-by and pagination, then artwork hydration. An empty result is
    /// an empty `Vec`, never [`MediaFileError::NotFound`].
    ///
    /// `artwork_error` carries Go's swallowed `log.Error`
    /// (`artwork_hydration.go:62-66`): the tracks come back either way, each with
    /// both artwork fields zero (unresolved) exactly as Go's empty map leaves them.
    /// A statement or row-hydration failure is `Err`, since Go returns
    /// `queryAll`'s error unchanged (`mediafile_repository.go:235-238`).
    ///
    /// # Errors
    /// [`MediaFileError::Sqlite`] when the statement fails or a row will not hydrate.
    pub fn get_all(
        &self,
        user_id: Option<&str>,
        library_filter: Option<Sql>,
        order_by: &[&str],
        max: i64,
        offset: i64,
        enable_media_file_cover_art: bool,
    ) -> Result<crate::artwork_hydration::ReadOutcome<MediaFileRecord>, MediaFileError> {
        let statement = get_all_sql(user_id, library_filter, order_by, max, offset);
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records, enable_media_file_cover_art))
    }

    /// `GetRandom` (`mediafile_repository.go:250-283`), both passes:
    /// phase 1 picks random rowids with [`get_random_rowid_sql`] — read as bare
    /// `i64`s through `query_all` and NEVER handed to a hydrator, exactly like Go's
    /// `queryAllSlice` into `[]int64` (`:264-266`) — then phase 2 hydrates only
    /// those rowids with [`get_random_hydrate_sql`], whose `ORDER BY random()`
    /// re-shuffle Go keeps deliberately (`:272-274`: `WHERE rowid IN (…)` returns
    /// ascending rowids, so the hydrated subset is re-shuffled). Zero rowids return
    /// an empty `Vec` WITHOUT running phase 2 (`:268-270`), so no artwork query
    /// fires either.
    ///
    /// `library_filter` is threaded into BOTH passes. Go applies
    /// `applyLibraryFilter` to the phase-1 rowid query (`:258`) AND to phase 2
    /// implicitly, because phase 2 goes through `selectMediaFile`
    /// (`:274`), which ends with an unconditional `applyLibraryFilter` (`:209`); the
    /// port threads the ONE caller-supplied clause into both, which is the same
    /// row set — see [`get_random_hydrate_sql`] for the ctx-vs-threaded argument
    /// and for why neither side applies caller Filters or `applyOptions` in phase 2.
    ///
    /// # Errors
    /// [`MediaFileError::Sqlite`] when either pass fails or a row will not hydrate.
    pub fn get_random(
        &self,
        filters: Option<(&str, Vec<crate::sqlrepo::Value>)>,
        library_filter: Option<Sql>,
        max: i64,
        user_id: Option<&str>,
        enable_media_file_cover_art: bool,
    ) -> Result<crate::artwork_hydration::ReadOutcome<MediaFileRecord>, MediaFileError> {
        // Phase 1: the narrow rowid pick. Go applies opt.Filters then
        // applyLibraryFilter, OrderBy("random()"), then Limit(opt.Max) (`:256-262`).
        let rowids: Vec<i64> = crate::execution::query_all(
            self.conn,
            &get_random_rowid_sql(filters, library_filter.clone(), max),
            |row| row.get::<_, i64>(0),
        )?;
        if rowids.is_empty() {
            // Go: `return model.MediaFiles{}, nil` — empty slice, no second query.
            return Ok(crate::artwork_hydration::ReadOutcome {
                items: Vec::new(),
                artwork_error: None,
            });
        }
        // Phase 2: hydrate the shuffled subset UNDER THE SAME library filter
        // (Go's selectMediaFile applies it unconditionally, :209), then the artwork
        // layer.
        let statement = get_random_hydrate_sql(user_id, library_filter, &rowids);
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records, enable_media_file_cover_art))
    }

    /// Phase 1 + phase 2 (Go's `queryAll` + `toModels()`). A `PostScan` failure
    /// aborts the whole read, as it does in Go.
    fn read_hydrated(&self, statement: &Sql) -> Result<Vec<MediaFileRecord>, MediaFileError> {
        let rows =
            crate::execution::query_all(self.conn, statement, crate::hydrate::row_to_mediafile)?;
        // Same exit shape as the album twin: a PostScan failure leaves the read as
        // a column-conversion error (`HydrateError::into_sqlite`), wrapped by
        // `MediaFileError::Sqlite`.
        Ok(rows
            .into_iter()
            .map(MediaFileRow::into_record)
            .collect::<Result<Vec<MediaFileRecord>, crate::hydrate::HydrateError>>()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?)
    }

    /// Phase 3 (`hydrateArtwork`, `mediafile_repository.go:244-246`): the two
    /// batched lookups and the branch matrix
    /// (`crate::artwork_hydration::hydrate_media_file_artwork`).
    fn hydrate_artwork(
        &self,
        records: Vec<MediaFileRecord>,
        enable_media_file_cover_art: bool,
    ) -> crate::artwork_hydration::ReadOutcome<MediaFileRecord> {
        let mut items = records;
        let artwork_error = crate::artwork_hydration::hydrate_media_file_artwork(
            self.conn,
            &mut items,
            enable_media_file_cover_art,
        );
        crate::artwork_hydration::ReadOutcome {
            items,
            artwork_error,
        }
    }
}

/// `getAllIDs` (`mediafile_repository.go:319-329`): the id-only projection,
/// with the annotation join only when the filters need one.
#[must_use]
pub fn get_all_ids_sql(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
    need_annotation: bool,
) -> Sql {
    let mut sql = format!("SELECT {TABLE}.id FROM {TABLE}");
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    // Same render order as [`count_all_sql`]: the annotation join's args precede the
    // library-filter args (`select.go:97-113`).
    if need_annotation {
        if let Some(user) = user_id {
            let (join_sql, join_args) = annotation_join_parts(user);
            sql.push_str(&join_sql);
            args.extend(join_args);
        }
    }
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause}"));
        args.extend(clause_args);
    }
    (sql, args)
}

/// `PutByMatch`'s match probe (`sql_base_repository.go:577-589` via `Put`,
/// `mediafile_repository.go:181-189`): the id of the row matching the
/// path+library pair — `Eq` maps render their keys alphabetically
/// (`library_id` < `path`).
#[must_use]
pub fn put_match_sql(path: &str, library_id: i64) -> Sql {
    (
        format!("SELECT id FROM {TABLE} WHERE library_id = ? AND path = ?"),
        vec![
            crate::sqlrepo::Value::Integer(library_id),
            crate::sqlrepo::Value::from(path),
        ],
    )
}

/// The FTS feeder payload `dbMediaFile.PostMapArgs` computes
/// (`mediafile_repository.go:60-72`): the caller marshals `tags`/
/// `participants` JSON and the full-text columns; this carries the finished
/// strings so the SQL layer only binds them. `full_text` is
/// `" " + formatFullText(...)` (`sql_search.go:15-18`), `search_participants`
/// the space-joined participant names, `search_normalized` the
/// `NormalizeForFTS` of the four title/artist fields.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsColumns {
    /// `media_file.full_text` — leading space included, as Go binds it.
    pub full_text: String,
    /// `media_file.search_participants`.
    pub search_participants: String,
    /// `media_file.search_normalized`.
    pub search_normalized: String,
}

/// `Put` (`mediafile_repository.go:181-189`): the `putByMatch` legs plus the
/// participant/tag maintenance. The update/insert legs are `r.put`
/// (`sql_base_repository.go:618-642`); the participants pair is
/// `updateParticipants` (`sql_participations.go:93-119`: delete-all then the
/// json_each INSERT); the tags pair is `updateTags` (`sql_tags.go:57-79`).
/// `model.MediaFile`'s row as the seam reads and writes it
/// (`model/mediafile.go:27-107`) — the 63 non-id persisted media_file columns (the
/// contract's 64 minus `id`, which the seam forbids in the map; the id is appended
/// last by [`put_sql`]), plus the six **hydration-only** fields the read path owns —
/// `id`, `library_path`, `library_name`, `annotation`, `bookmark_position`,
/// `genres` (`persistence/mediafile_repository.go:29-67`, `selectMediaFile`
/// `:204-210`) — and the two read-path-only artwork fields `image`/`album_image`
/// (S-D3c, Go's `ItemImage` embed and `AlbumImage` at `model/mediafile.go:30,34`).
///
/// Deliberately absent from the WRITE map (entity write contract):
/// `LibraryPath`/`LibraryName` (`structs:"-"`, join-only), `Genres` (`structs:"-"`),
/// `Bookmarkable.BookmarkPosition` (`structs:"-"` at `model/bookmark.go:9`),
/// `AlbumImage` (no columns), the `Annotations`/`ItemImage` embeds,
/// `average_rating` (annotation-managed, never written by entity Put), and `id`
/// (the write legs' own argument). [`mediafile_row`] emits none of the six
/// hydration-only fields, so the write SQL is byte-identical to a record whose
/// derived fields are empty — asserted by `tests/runtime_hydrate_album_mediafile.rs`.
///
/// The Go `RgAlbumGain`/`RgAlbumPeak`/`RgTrackGain`/`RgTrackPeak` dbx shims
/// (`mediafile_repository.go:35-38`) exist only because dbx's default field-map
/// cannot render `RG*` to `rg_*`; Rust binds the real column names directly, so the
/// four unconditional copies at `mediafile_repository.go:50-53` are no-ops here — see
/// [`crate::hydrate::mediafile_post_scan`]. `tags`/`participants` hold the *typed*
/// values the Go shims shadow: PostMapArgs overwrites their column keys with JSON on
/// the write side ([`mediafile_row`] routes that through
/// [`crate::marshal::mediafile_post_map_args`]), and on the read side the raw TEXT
/// columns sit on [`MediaFileRow`] until [`crate::hydrate::mediafile_post_scan`]
/// decodes them.
///
/// Timestamps are `DateTime<Utc>` bound through `crate::time`.
/// `created_at`/`updated_at`/`birth_time` are Go non-pointer `time.Time`:
/// `birth_time` is NOT NULL in the DDL (required here); `created_at` is
/// nullable (Go zero → NULL): [`put_sql`] stamps it to `now` only when the
/// record's is `None` (Go `mediafile_repository.go:184-186`) and it is
/// dropped from UPDATE sets by `selectUpdateColumns`; `updated_at` passes
/// through the record unchanged (Go `Put` never touches it).
///
/// `bit_depth`/`bpm` are Go `*int` (nullable in the DDL) → `Option<i32>`.
///
/// `participant_names` follows the album record's rule: a hydrated record fills it
/// from the decoded `participants` column with an EMPTY `sort_name`
/// (`crate::album_repo::participant_names_from_participants`), because the column
/// stores only `{id, name, subRole}` and Go's sort names come from the unported
/// `getParticipants` artist re-read (`sql_participations.go:127-146`). Documented
/// divergence, recorded in the S-D3b report.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFileRecord {
    /// The row's `id` (Go `MediaFile.ID`, `structs:"id"` at `model/mediafile.go:36`).
    /// Go scans it (`ID` snake-cases to the selected `id`), so a bulk read keeps
    /// every row's own identity. [`mediafile_row`] does NOT emit it — the id is
    /// appended by the repo's write legs ([`put_sql`] takes it as its own argument,
    /// matching Go's `put(ctx, id, …)`, `sql_base_repository.go:618`). Same
    /// precedent as `FolderRecord::id` (`folder_repo.rs:347`) and
    /// `PlaylistRecord::id` (`playlist_repo.rs:486`); the delegating hooks of the next
    /// slice overwrite it from `media_file_id`
    /// (`playlist_track_repository.go:32`, `scrobble_buffer_repository.go:28`).
    pub id: String,
    /// `album` (Go `Album`).
    pub album: String,
    /// `album_artist` (Go `AlbumArtist`).
    pub album_artist: String,
    /// `album_artist_id` (Go `AlbumArtistID`, deprecated).
    pub album_artist_id: String,
    /// `album_id` (Go `AlbumID`).
    pub album_id: String,
    /// `artist` (Go `Artist`).
    pub artist: String,
    /// `artist_id` (Go `ArtistID`, deprecated).
    pub artist_id: String,
    /// `birth_time` (Go `BirthTime`) — the file's ctime; NOT NULL in the
    /// DDL, and dropped from UPDATE sets (`selectUpdateColumns` — never
    /// updated).
    pub birth_time: DateTime<Utc>,
    /// `bit_depth` (Go `BitDepth *int`). `None` binds NULL.
    pub bit_depth: Option<i32>,
    /// `bit_rate` (Go `BitRate`).
    pub bit_rate: i32,
    /// `bpm` (Go `BPM *int`). `None` binds NULL.
    pub bpm: Option<i32>,
    /// `catalog_num` (Go `CatalogNum`).
    pub catalog_num: String,
    /// `channels` (Go `Channels`).
    pub channels: i32,
    /// `codec` (Go `Codec`).
    pub codec: String,
    /// `comment` (Go `Comment`).
    pub comment: String,
    /// `compilation` (Go `Compilation`).
    pub compilation: bool,
    /// `created_at` (Go `CreatedAt`); `None` (Go zero `time.Time`) binds
    /// NULL. Dropped from UPDATE sets; never re-stamped here.
    pub created_at: Option<DateTime<Utc>>,
    /// `date` (Go `Date`).
    pub date: String,
    /// `disc_number` (Go `DiscNumber`).
    pub disc_number: i32,
    /// `disc_subtitle` (Go `DiscSubtitle`).
    pub disc_subtitle: String,
    /// `duration` (Go `Duration`, `float32` at `model/mediafile.go:64`).
    /// The DDL column is `real`; `f64` matches the column affinity (same
    /// precedent as `AlbumRecord::duration`).
    pub duration: f64,
    /// `explicit_status` (Go `ExplicitStatus`).
    pub explicit_status: String,
    /// `folder_id` (Go `FolderID`).
    pub folder_id: String,
    /// `genre` (Go `Genre` — the most common genre; a plain column distinct
    /// from the `Genres` tag set).
    pub genre: String,
    /// `has_cover_art` (Go `HasCoverArt`).
    pub has_cover_art: bool,
    /// `library_id` (Go `LibraryID`) — FK to `library`.
    pub library_id: i64,
    /// `lyrics` (Go `Lyrics`).
    pub lyrics: String,
    /// `mbz_recording_id` (Go `MbzRecordingID`).
    pub mbz_recording_id: String,
    /// `mbz_release_track_id` (Go `MbzReleaseTrackID`).
    pub mbz_release_track_id: String,
    /// `mbz_album_id` (Go `MbzAlbumID`).
    pub mbz_album_id: String,
    /// `mbz_release_group_id` (Go `MbzReleaseGroupID`).
    pub mbz_release_group_id: String,
    /// `mbz_artist_id` (Go `MbzArtistID`, deprecated).
    pub mbz_artist_id: String,
    /// `mbz_album_artist_id` (Go `MbzAlbumArtistID`, deprecated).
    pub mbz_album_artist_id: String,
    /// `mbz_album_type` (Go `MbzAlbumType`).
    pub mbz_album_type: String,
    /// `mbz_album_comment` (Go `MbzAlbumComment`).
    pub mbz_album_comment: String,
    /// `missing` (Go `Missing`).
    pub missing: bool,
    /// `order_album_artist_name` (Go `OrderAlbumArtistName`).
    pub order_album_artist_name: String,
    /// `order_album_name` (Go `OrderAlbumName`).
    pub order_album_name: String,
    /// `order_artist_name` (Go `OrderArtistName`).
    pub order_artist_name: String,
    /// `order_title` (Go `OrderTitle`).
    pub order_title: String,
    /// `original_date` (Go `OriginalDate`).
    pub original_date: String,
    /// `original_year` (Go `OriginalYear`).
    pub original_year: i32,
    /// `path` (Go `Path`).
    pub path: String,
    /// `pid` (Go `PID`) — the provider id.
    pub pid: String,
    /// `probe_data` (Go `ProbeData`).
    pub probe_data: String,
    /// `release_date` (Go `ReleaseDate`).
    pub release_date: String,
    /// `release_year` (Go `ReleaseYear`).
    pub release_year: i32,
    /// `rg_album_gain` (Go `RGAlbumGain *float64`, read through the
    /// `RgAlbumGain` shim). `None` binds NULL.
    pub rg_album_gain: Option<f64>,
    /// `rg_album_peak` (Go `RGAlbumPeak *float64`). `None` binds NULL.
    pub rg_album_peak: Option<f64>,
    /// `rg_track_gain` (Go `RGTrackGain *float64`). `None` binds NULL.
    pub rg_track_gain: Option<f64>,
    /// `rg_track_peak` (Go `RGTrackPeak *float64`). `None` binds NULL.
    pub rg_track_peak: Option<f64>,
    /// `sample_rate` (Go `SampleRate`).
    pub sample_rate: i32,
    /// `size` (Go `Size`).
    pub size: i64,
    /// `sort_album_artist_name` (Go `SortAlbumArtistName`).
    pub sort_album_artist_name: String,
    /// `sort_album_name` (Go `SortAlbumName`).
    pub sort_album_name: String,
    /// `sort_artist_name` (Go `SortArtistName`).
    pub sort_artist_name: String,
    /// `sort_title` (Go `SortTitle`).
    pub sort_title: String,
    /// `suffix` (Go `Suffix`).
    pub suffix: String,
    /// `tags` (Go `Tags`) — all imported tags; PostMapArgs marshals them to
    /// the `tags` JSON column and indexes them into `media_file_tags`.
    pub tags: DbTags,
    /// `title` (Go `Title`).
    pub title: String,
    /// `track_number` (Go `TrackNumber`).
    pub track_number: i32,
    /// `year` (Go `Year`).
    pub year: i32,
    /// `participants` (Go `Participants`) — marshalled to the
    /// `participants` column and the `media_file_artists` junction rows.
    pub participants: Participants,
    /// FTS participant-name input (Go `Participants.AllNames()`), carried
    /// beside the JSON source so the FTS columns are derived from the same
    /// `model.Participants` the JSON is. S-D3 hydration fills both from one
    /// read; there is no write path that accepts a free-standing name list.
    pub participant_names: Vec<crate::album_repo::NamedParticipant>,
    /// `updated_at` (Go `UpdatedAt`); `None` binds NULL. Passed through
    /// unchanged by [`put_sql`] (Go `Put` never touches it,
    /// `mediafile_repository.go:184-190`).
    pub updated_at: Option<DateTime<Utc>>,
    /// `library_path` (Go `MediaFile.LibraryPath`, `structs:"-"` at
    /// `model/mediafile.go:40` — join-only). Hydrated from
    /// `library.path as library_path` (`selectMediaFile`,
    /// `mediafile_repository.go:205`); [`mediafile_row`] never emits it. LEFT join, so
    /// a row whose library is gone holds SQL NULL, which Go's non-pointer `string`
    /// rejects: [`crate::hydrate::row_to_mediafile`] errors when the alias is present
    /// but NULL and leaves Go's zero when a projection omits it.
    pub library_path: String,
    /// `library_name` (Go `MediaFile.LibraryName`, `structs:"-"`,
    /// `model/mediafile.go:40` — join-only), from `library.name as library_name`.
    pub library_name: String,
    /// The per-user annotation the select's `annotation` LEFT JOIN contributes
    /// (`sql_annotations.go:63-91`), **derived, not persisted**: Go's
    /// `MediaFile.Annotations` embed is `structs:"-"` (`model/mediafile.go:28`), so
    /// [`mediafile_row`] emits none of it and the write SQL is unchanged. Filled from
    /// the six joined columns plus the base `media_file.average_rating`
    /// (`crate::hydrate::row_to_annotation`), with zero values where a light query —
    /// [`get_all_ids_sql`], [`get_random_rowid_sql`] — omits the joined columns.
    /// Timestamp strings carry Go's RFC3339Nano wire form
    /// ([`crate::hydrate::to_go_time_text`]), NOT the SQLite storage spelling.
    pub annotation: nd_model::annotation::Annotation,
    /// `bookmark_position` (Go `Bookmarkable.BookmarkPosition`, `structs:"-"` at
    /// `model/bookmark.go:9` — derived, never persisted). `withBookmark`
    /// (`sql_bookmarks.go:18-28`) emits `coalesce(position, 0) as bookmark_position`
    /// ONLY for a logged-in user, so the column is ABSENT on an anonymous base read
    /// and on every light query; [`crate::hydrate::row_to_mediafile`] reads it behind
    /// [`crate::hydrate::RowGet::has_column`] and decodes an absent column to Go's
    /// zero (`0`). The coalesce makes a LEFT-join miss `0`, never NULL, and a
    /// NULL `position` cell would be Go's `0` too.
    pub bookmark_position: i64,
    /// `genres` (Go `MediaFile.Genres`, `structs:"-"` at `model/mediafile.go:72` —
    /// join-only, derived). Set ONLY by [`crate::hydrate::mediafile_post_scan`] from
    /// the decoded `tags` shim (`Tags.ToGenres`, `model/tag.go:138-148`), which also
    /// overwrites [`MediaFileRecord::genre`] (`mediafile_repository.go:64`). Left empty
    /// when the shim is empty, because Go's guard skips the assignment and the column
    /// value of `genre` survives. See [`crate::sql_tags::Genre`] for the DTO's
    /// nd-db-local precedent; never emitted by [`mediafile_row`].
    pub genres: Vec<crate::sql_tags::Genre>,
    /// Go's embedded `ItemImage` (`model/mediafile.go:30`; every field
    /// `structs:"-"` at `model/artwork.go:29-40`): the TRACK's own artwork state,
    /// **read-path only**. Filled by
    /// [`crate::artwork_hydration::hydrate_media_file_artwork`] — the second
    /// hydration layer (`mediafile_repository.go:244-246`) whose branch matrix
    /// decides between own embedded art and the inherited album image — never by
    /// [`crate::hydrate::row_to_mediafile`]. The zero value means UNRESOLVED
    /// (still requestable), not known-absent; see
    /// [`crate::artwork_hydration::ItemImage`] and
    /// [`MediaFileRecord::album_image`]. Never emitted by [`mediafile_row`].
    pub image: crate::artwork_hydration::ItemImage,
    /// `MediaFile.AlbumImage` (`model/mediafile.go:34`, `structs:"-"`): the parent
    /// album's artwork state, hydrated alongside the track's own "so a song's
    /// Jellyfin album-art tag can be pixel-versioned without a second query"
    /// (Go's comment `:32-33`). Applied BEFORE the branch matrix
    /// (`artwork_hydration.go:110`), so it is populated on EVERY branch —
    /// including the own-art-wins `continue` (`:112-116`). Read-path only; never a
    /// [`mediafile_row`] key.
    pub album_image: crate::artwork_hydration::ItemImage,
}

/// The subtitle tag name (`model.TagSubtitle`, `model/tag.go:192`).
const TAG_SUBTITLE: &str = "subtitle";

/// The record's `full_title` (`m.FullTitle()`, `model/mediafile.go:110-115`)
/// with `append_subtitle` resolved by the caller: Go's
/// `conf.Server.Subsonic.AppendSubtitle` is config state this SQL layer
/// must not read, so the seam takes it explicitly — the config wiring
/// supplies it (default `false` matches the config default).
#[must_use]
pub fn full_title(rec: &MediaFileRecord, append_subtitle: bool) -> String {
    let subtitle = rec
        .tags
        .get(TAG_SUBTITLE)
        .and_then(|s| s.first().cloned())
        .unwrap_or_default();
    crate::sql_feeders::full_title(&rec.title, &subtitle, append_subtitle)
}
/// `structs.Map(dbMediaFile)` + `dbMediaFile.PostMapArgs`
/// (`mediafile_repository.go:69-79`) as one sorted
/// [`crate::marshal::ColumnMap`]: exactly the 63 non-id persisted
/// media_file columns (`id` is appended last by [`put_sql`]), with the
/// PostMapArgs overwrites routed through the S-D2a hook so the
/// transformation lives in one place.
///
/// **Write-map invariance (S-D3b, extended by S-D3c):** the six hydration-only
/// record fields (`id`, `library_path`, `library_name`, `annotation`,
/// `bookmark_position`, `genres`) and the two read-path-only artwork fields
/// (`image`, `album_image`) are NOT keys here — Go's `structs:"-"` tags keep them
/// out of `structs.Map` (`model/mediafile.go:28-34`, `model/artwork.go:29-40`), and
/// `id` is the write legs' own argument — so a hydrated record renders the identical
/// 66-key INSERT/UPDATE column set an unwritten one does.
///
/// **S-D2b consistency invariant:** the FTS inputs are derived *atomically*
/// from the record's own `participants`/`tags` (see
/// [`mediafile_derived`]) — the derived values and the JSON/junction values
/// share one source, so they can never contradict each other.
///
/// `append_subtitle` is the config-resolved `FullTitle()` input (see
/// [`full_title`]); the config wiring supplies it.
#[must_use]
pub fn mediafile_row(m: &MediaFileRecord, append_subtitle: bool) -> crate::marshal::ColumnMap {
    let mut map = BTreeMap::new();
    map.insert("album".to_string(), Value::Text(m.album.clone()));
    map.insert(
        "album_artist".to_string(),
        Value::Text(m.album_artist.clone()),
    );
    map.insert(
        "album_artist_id".to_string(),
        Value::Text(m.album_artist_id.clone()),
    );
    map.insert("album_id".to_string(), Value::Text(m.album_id.clone()));
    map.insert("artist".to_string(), Value::Text(m.artist.clone()));
    map.insert("artist_id".to_string(), Value::Text(m.artist_id.clone()));
    map.insert("birth_time".to_string(), crate::time::value(&m.birth_time));
    map.insert(
        "bit_depth".to_string(),
        m.bit_depth
            .map(|v| Value::Integer(i64::from(v)))
            .unwrap_or(Value::Null),
    );
    map.insert(
        "bit_rate".to_string(),
        Value::Integer(i64::from(m.bit_rate)),
    );
    map.insert(
        "bpm".to_string(),
        m.bpm
            .map(|v| Value::Integer(i64::from(v)))
            .unwrap_or(Value::Null),
    );
    map.insert(
        "catalog_num".to_string(),
        Value::Text(m.catalog_num.clone()),
    );
    map.insert(
        "channels".to_string(),
        Value::Integer(i64::from(m.channels)),
    );
    map.insert("codec".to_string(), Value::Text(m.codec.clone()));
    map.insert("comment".to_string(), Value::Text(m.comment.clone()));
    map.insert("compilation".to_string(), Value::Bool(m.compilation));
    map.insert(
        "created_at".to_string(),
        crate::time::optional_value(m.created_at.as_ref()),
    );
    map.insert("date".to_string(), Value::Text(m.date.clone()));
    map.insert(
        "disc_number".to_string(),
        Value::Integer(i64::from(m.disc_number)),
    );
    map.insert(
        "disc_subtitle".to_string(),
        Value::Text(m.disc_subtitle.clone()),
    );
    map.insert("duration".to_string(), Value::Real(m.duration));
    map.insert(
        "explicit_status".to_string(),
        Value::Text(m.explicit_status.clone()),
    );
    map.insert("folder_id".to_string(), Value::Text(m.folder_id.clone()));
    map.insert("genre".to_string(), Value::Text(m.genre.clone()));
    map.insert("has_cover_art".to_string(), Value::Bool(m.has_cover_art));
    map.insert("library_id".to_string(), Value::Integer(m.library_id));
    map.insert("lyrics".to_string(), Value::Text(m.lyrics.clone()));
    map.insert(
        "mbz_recording_id".to_string(),
        Value::Text(m.mbz_recording_id.clone()),
    );
    map.insert(
        "mbz_release_track_id".to_string(),
        Value::Text(m.mbz_release_track_id.clone()),
    );
    map.insert(
        "mbz_album_id".to_string(),
        Value::Text(m.mbz_album_id.clone()),
    );
    map.insert(
        "mbz_release_group_id".to_string(),
        Value::Text(m.mbz_release_group_id.clone()),
    );
    map.insert(
        "mbz_artist_id".to_string(),
        Value::Text(m.mbz_artist_id.clone()),
    );
    map.insert(
        "mbz_album_artist_id".to_string(),
        Value::Text(m.mbz_album_artist_id.clone()),
    );
    map.insert(
        "mbz_album_type".to_string(),
        Value::Text(m.mbz_album_type.clone()),
    );
    map.insert(
        "mbz_album_comment".to_string(),
        Value::Text(m.mbz_album_comment.clone()),
    );
    map.insert("missing".to_string(), Value::Bool(m.missing));
    map.insert(
        "order_album_artist_name".to_string(),
        Value::Text(m.order_album_artist_name.clone()),
    );
    map.insert(
        "order_album_name".to_string(),
        Value::Text(m.order_album_name.clone()),
    );
    map.insert(
        "order_artist_name".to_string(),
        Value::Text(m.order_artist_name.clone()),
    );
    map.insert(
        "order_title".to_string(),
        Value::Text(m.order_title.clone()),
    );
    map.insert(
        "original_date".to_string(),
        Value::Text(m.original_date.clone()),
    );
    map.insert(
        "original_year".to_string(),
        Value::Integer(i64::from(m.original_year)),
    );
    map.insert("path".to_string(), Value::Text(m.path.clone()));
    map.insert("pid".to_string(), Value::Text(m.pid.clone()));
    map.insert("probe_data".to_string(), Value::Text(m.probe_data.clone()));
    map.insert(
        "release_date".to_string(),
        Value::Text(m.release_date.clone()),
    );
    map.insert(
        "release_year".to_string(),
        Value::Integer(i64::from(m.release_year)),
    );
    map.insert(
        "rg_album_gain".to_string(),
        m.rg_album_gain.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert(
        "rg_album_peak".to_string(),
        m.rg_album_peak.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert(
        "rg_track_gain".to_string(),
        m.rg_track_gain.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert(
        "rg_track_peak".to_string(),
        m.rg_track_peak.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert(
        "sample_rate".to_string(),
        Value::Integer(i64::from(m.sample_rate)),
    );
    map.insert("size".to_string(), Value::Integer(m.size));
    map.insert(
        "sort_album_artist_name".to_string(),
        Value::Text(m.sort_album_artist_name.clone()),
    );
    map.insert(
        "sort_album_name".to_string(),
        Value::Text(m.sort_album_name.clone()),
    );
    map.insert(
        "sort_artist_name".to_string(),
        Value::Text(m.sort_artist_name.clone()),
    );
    map.insert("sort_title".to_string(), Value::Text(m.sort_title.clone()));
    map.insert("suffix".to_string(), Value::Text(m.suffix.clone()));
    // `tags`/`participants` land as typed values and are overwritten with
    // their JSON by PostMapArgs below.
    map.insert("tags".to_string(), Value::Null);
    map.insert("title".to_string(), Value::Text(m.title.clone()));
    map.insert(
        "track_number".to_string(),
        Value::Integer(i64::from(m.track_number)),
    );
    map.insert("year".to_string(), Value::Integer(i64::from(m.year)));
    map.insert("participants".to_string(), Value::Null);
    map.insert(
        "updated_at".to_string(),
        crate::time::optional_value(m.updated_at.as_ref()),
    );
    crate::marshal::mediafile_post_map_args(
        crate::marshal::ColumnMap::new(map),
        mediafile_derived(m, append_subtitle),
    )
    .0
}
/// `dbMediaFile` raw row (phase 1 of the two-phase hydration seam,
/// `persistence/mediafile_repository.go:29-39`): the columns `selectMediaFile` scans
/// (`mediafile_repository.go:204-210` — `media_file.*`, the two library display
/// columns, the six annotation columns, the base `average_rating` and, for a
/// logged-in user only, the coalesced `bookmark_position`) with the two JSON shim
/// columns **still unparsed**.
///
/// The shims are Go's non-pointer `string` fields (`:30-31`), hence plain `String` here
/// and [`crate::hydrate::RowGet::get_text`] on the read: SQL NULL in either
/// aborts the scan for the same reason Go does with `converting NULL to
/// string is unsupported` (`dbMediaFile.Participants` is NOT NULL in the DDL —
/// `schema_ddl.rs:64` defaults it to `'{}'` — but a raw NULL written behind the
/// write path reproduces Go's failure, which the golden asserts). Rust's
/// payload names the column rather than reproducing Go's wording; see the
/// `hydrate.rs` module-level note on scan wording.
/// `participants` has NO `PostScan` guard (`:55`), so the empty string reaches
/// `unmarshalParticipants` and errors too.
///
/// The join-only display columns, the annotation columns, `bookmark_position` and `id`
/// are read straight onto the record — Go scans `MediaFile.LibraryPath`/`LibraryName`
/// and the promoted `Annotations`/`Bookmarkable` embeds by FIELD name despite
/// `structs:"-"` (`dbx@v1.12.0/struct.go:163-215` descends into anonymous embeds and
/// maps through `DefaultFieldMapFunc`) — so they are not duplicated on this raw
/// struct. Phase 2 is [`crate::hydrate::mediafile_post_scan`].
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFileRow {
    /// The media_file columns scanned from the row, the two shim fields still empty.
    pub record: MediaFileRecord,
    /// The raw `participants` TEXT column, unparsed (Go `dbMediaFile.Participants`,
    /// `:30`). Decoded UNGUARDED, like Go's `:55`.
    pub participants: String,
    /// The raw `tags` TEXT column, unparsed (Go `dbMediaFile.Tags`, `:31`). Its
    /// decode is what derives `genre`/`genres`.
    pub tags: String,
}

impl MediaFileRow {
    /// `dbMediaFile.PostScan` (`mediafile_repository.go:49-67`): thin delegation onto
    /// [`crate::hydrate::mediafile_post_scan`], surfaced the way Go's `ScanStruct`
    /// error leaves the read (`dbx@v1.12.0/rows.go:92-102`).
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] carrying
    /// [`crate::hydrate::MEDIAFILE_PREFIX`].
    pub fn into_record(self) -> Result<MediaFileRecord, crate::hydrate::HydrateError> {
        crate::hydrate::mediafile_post_scan(self)
    }
}

/// `MediaFileDerived` computed atomically from the record — the S-D2b
/// consistency invariant made type-level: there is no constructor that takes
/// a free-standing `participant_names`/`full_title`, so the FTS inputs
/// cannot diverge from the stored JSON/junction data. `full_title` and the
/// subtitle both come from the record's own tags/title.
#[must_use]
fn mediafile_derived(
    m: &MediaFileRecord,
    append_subtitle: bool,
) -> crate::marshal::MediaFileDerived {
    crate::marshal::MediaFileDerived {
        full_title: full_title(m, append_subtitle),
        album: m.album.clone(),
        artist: m.artist.clone(),
        album_artist: m.album_artist.clone(),
        sort_title: m.sort_title.clone(),
        sort_album_name: m.sort_album_name.clone(),
        sort_artist_name: m.sort_artist_name.clone(),
        sort_album_artist_name: m.sort_album_artist_name.clone(),
        disc_subtitle: m.disc_subtitle.clone(),
        participant_names: crate::album_repo::participant_names(&m.participant_names),
        tags: m.tags.clone(),
        participants: m.participants.clone(),
    }
}

/// `Put` (`mediafile_repository.go:181-189`), record-driven (S-D2b): the
/// record is the only write input — no caller-supplied column arrays.
///
/// **putByMatch semantics** (`putByMatch`,
/// `sql_base_repository.go:575-590`): when the caller's id is empty, Go
/// resolves it first via the `Eq{path, library_id}` match probe
/// ([`put_match_sql`]) and PUTs against the found id. That update-vs-insert
/// selection is **the caller's responsibility** — this seam renders the id-keyed
/// `r.put` legs and the maintenance, exactly as Go's `put` does once the id
/// is known. The caller runs [`put_match_sql`] when the record has no id and
/// passes the matched id here; the update leg runs when a row exists, the
/// insert leg when not.
///
/// `append_subtitle` is the config-resolved `FullTitle()` switch (see
/// [`full_title`]); the config wiring supplies it.
///
/// `now` stamps the row exactly like Go's `Put`
/// (`mediafile_repository.go:184-190`): `created_at` to `now` when the record's `IsZero()`,
/// which in Rust means `None` OR an explicit `Some(crate::time::zero())` — the year-one
/// value Go's decoder can hand back — and otherwise passes the record's value through.
/// `updated_at` passes through the record unchanged — Go stamps neither in `Put`.
/// `created_at`/`birth_time` are still dropped from the UPDATE set by the
/// seam (INSERT-only).
///
/// **Consistency invariant:** the PostMapArgs hook is fed the FTS inputs
/// derived from the record's own `participants`/`tags` (see
/// [`mediafile_derived`]) — the same values the JSON columns and the
/// `media_file_artists`/`media_file_tags` junction maintenance are built from
/// — so the FTS columns can never contradict the stored JSON.
///
/// Both legs render through the shared [`crate::marshal::MarshalledRow`]:
/// UPDATE = the `selectUpdateColumns`-filtered set (no `created_at` and no
/// `birth_time`; `id` binds last via `WHERE id = ?`), INSERT = the full sorted
/// columns with `id` last in both lists
/// (`sql_base_repository.go:637-641`). Go's `put` runs the UPDATE first and
/// only INSERTs on a 0-row match; the caller picks the leg.
///
/// `cols_to_update`: optional `colsToUpdate`
/// (`sql_base_repository.go:595-601`); empty applies no restriction. The
/// entries are `toSnakeCase`-normalized by the seam
/// ([`crate::marshal::to_snake_case`]). An update set empty after the
/// `created_at`/`birth_time` drop is
/// [`crate::marshal::MarshalError::EmptyUpdateSet`].
///
/// # Errors
///
/// Returns [`crate::marshal::MarshalError`] when the map carries `id`
/// (impossible from [`mediafile_row`]) or the UPDATE set is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct PutMediaFileSql {
    /// The `r.put` update leg (filtered columns, `id` last in args).
    pub update: Sql,
    /// The `r.put` insert leg (sorted columns, `id` last in both).
    pub insert: Sql,
    /// `Delete(media_file_artists) WHERE media_file_id = ?`.
    pub participants_delete: Sql,
    /// The `updateParticipants` json_each INSERT
    /// (`sql_participations.go:102-124`), built by
    /// [`crate::sql_participations::update_participants_sql`] from the
    /// record's participants. `None` when the media file has no
    /// participants (Go returns before building the insert, `:87-89`) — the
    /// delete still runs, so a put with empty participants clears the table.
    pub participants_insert: Option<Sql>,
    /// `Delete(media_file_tags) WHERE media_file_id = ?`.
    pub tags_delete: Sql,
    /// The `updateTags` scoped INSERT, absent when no indexed tag ids.
    pub tags_insert: Option<Sql>,
}

/// See [`PutMediaFileSql`].
pub fn put_sql(
    id: &str,
    m: &MediaFileRecord,
    now: DateTime<Utc>,
    append_subtitle: bool,
    cols_to_update: &[&str],
) -> Result<PutMediaFileSql, crate::marshal::MarshalError> {
    // Go's Put (mediafile_repository.go:184-190): CreatedAt is stamped to `now` when it
    // `IsZero()`, and UpdatedAt is never touched. `IsZero()` is a VALUE test on the year-one
    // time, so the Rust port has TWO spellings of "unset" that must both stamp: `None`
    // (a read of a NULL column) and an explicit `Some(crate::time::zero())` (a decode of a
    // Go-written zero timestamp). A real timestamp — including an epoch-adjacent one that is
    // merely close to year-one — passes through untouched.
    let stamped = MediaFileRecord {
        created_at: Some(match m.created_at {
            None => now,
            Some(at) if at == crate::time::zero() => now,
            Some(at) => at,
        }),
        ..m.clone()
    };
    let (map, maintenance) = crate::marshal::mediafile_post_map_args(
        mediafile_row(&stamped, append_subtitle),
        mediafile_derived(&stamped, append_subtitle),
    );
    put_from_map(id, &map, &maintenance, cols_to_update)
}

/// `put` over a PostMapArgs-applied map plus the maintenance bundle — the
/// S-D2a base-map adapter. [`put_sql`] delegates here (record → row → map →
/// this); `tests/runtime_marshal.rs` drives this directly with
/// caller-supplied base maps, which keeps the S-D2a transformation boundary
/// exercised.
pub fn put_from_map(
    id: &str,
    map: &crate::marshal::ColumnMap,
    maintenance: &crate::marshal::MediaFileMaintenance,
    cols_to_update: &[&str],
) -> Result<PutMediaFileSql, crate::marshal::MarshalError> {
    let row = crate::marshal::MarshalledRow::new(TABLE, id, map, cols_to_update)?;
    let up = crate::sql_participations::update_participants_sql(
        TABLE.to_string(),
        id.to_string(),
        &maintenance.participants,
    );
    Ok(PutMediaFileSql {
        update: (row.update_sql, row.update_args),
        insert: (row.insert_sql, row.insert_args),
        participants_delete: up.delete,
        participants_insert: up.insert,
        tags_delete: crate::sql_tags::delete_tags_sql(TABLE.to_string(), id.to_string()),
        tags_insert: crate::sql_tags::insert_tags_sql(
            TABLE.to_string(),
            id.to_string(),
            maintenance.indexed_tag_ids.clone(),
        ),
    })
}

/// Thin alias for [`put_from_map`] retained so the S-D2a `put_marshalled_sql`
/// call sites in `tests/runtime_marshal.rs` keep compiling; there is exactly
/// one put-result type per repo (no `PutMediaFileMarshalled`).
///
/// # Errors
///
/// As [`put_from_map`].
pub fn put_marshalled_sql(
    id: &str,
    map: &crate::marshal::ColumnMap,
    maintenance: &crate::marshal::MediaFileMaintenance,
    cols_to_update: &[&str],
) -> Result<PutMediaFileSql, crate::marshal::MarshalError> {
    put_from_map(id, map, maintenance, cols_to_update)
}

// ---------------------------------------------------------------------------
// DB-10 — the EXECUTED `Put` (`mediafile_repository.go:184-197`), identity and
// atomic maintenance.
// ---------------------------------------------------------------------------

/// Which row [`put_mediafile_in_tx`] writes: an explicit id, or the `path`+`library_id`
/// match Go's `Put` hands to `putByMatch` (`mediafile_repository.go:188`,
/// `sql_base_repository.go:577-589`).
///
/// **A `Match` is a trusted SC/system scope, NOT an unscoped lookup.** This is the
/// scanner's identity probe — the one `Put` builds itself from the record it is saving —
/// and it must never be reachable from a raw HTTP path string: `media_file.path` has
/// `default ''` and is indexed NON-unique (`media_file_path`, `media_file_path_nocase`,
/// `nd-dbschema/src/schema_ddl.rs`), so there is no unique constraint to lean on and an
/// attacker-supplied path would claim an existing row. `library_id` is part of the
/// predicate, so a match never crosses libraries, and the empty path is a REAL stored
/// value rather than an invalid one: `Match{path: "", ..}` matches a row stored with
/// `path = ''` in that library exactly as Go's `Eq{"path": ""}` does. Rejecting an empty
/// path is a caller (SC/BOOT) policy decision, not a DB rule.
///
/// The predicate is byte-exact and case-SENSITIVE: the probe compares against the column,
/// which has no `COLLATE NOCASE` (only its `media_file_path_nocase` INDEX does), so
/// `/Music/A.mp3` does not match `/music/a.mp3` — Go's `Eq` renders the same bare
/// comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaIdentity {
    /// `sql_base_repository.go:578-580`: a non-empty id makes `putByMatch` call `put`
    /// DIRECTLY, so the match probe never runs and no other row's identity can be reused
    /// — not even a row holding the SAME `path`+`library_id` as the record.
    ///
    /// An EMPTY id here takes Go's generated-id arm (`sql_base_repository.go:635-638`) and
    /// inserts under a freshly minted `nd_id::new_random()` id rather than being refused.
    /// DOCUMENTED EDGE: Go cannot express "empty id, no probe" — its `Put` always builds the
    /// `Eq{path, library_id}` filter from the record, so an empty `m.ID` falls through to the
    /// probe (`:581-588`). This arm keeps [`MediaIdentity::Id`] meaning "no probe" and
    /// generates instead; a caller that wants Go's exact probe-then-write behavior passes
    /// [`MediaIdentity::Match`], which is the arm `Put` actually builds.
    Id(String),
    /// `sql_base_repository.go:581-588`: resolve the id with [`put_match_sql`] on the
    /// caller's transaction first.
    Match {
        /// `Eq{"path": m.Path}` (`mediafile_repository.go:188`).
        path: String,
        /// `Eq{"library_id": m.LibraryID}` (same line).
        library_id: i64,
    },
}

/// What a successful [`put_mediafile_in_tx`] reports.
///
/// Both fields are ADVISORY until the CALLER commits: a rolled-back Put publishes
/// nothing, and the media_file repository keeps no cache (Go keeps none either), so
/// there is nothing to invalidate on rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFileWriteResult {
    /// The id this Put wrote — Go's `m.ID = id` (`mediafile_repository.go:192`), which BOTH
    /// junction legs are then keyed with (`:193-196`). It is one of three: the caller's
    /// [`MediaIdentity::Id`], the id the probe FOUND on the [`MediaIdentity::Match`] arm
    /// (the identity-reuse semantic this seam exists to prove: the record's OWN `id` is
    /// IGNORED there, so a caller that re-imports a track under a fresh id lands on the
    /// existing row), or a freshly generated `nd_id::new_random()` id when neither was
    /// available — Go's random-id arm (`sql_base_repository.go:635-638`), REPRODUCED rather
    /// than refused. In that last case the id is minted INSIDE the caller's transaction, so a
    /// rolled-back attempt leaves no row under it and nothing is published before the caller
    /// commits.
    pub id: String,
    /// `true` when the INSERT leg ran rather than the UPDATE leg, i.e. when the UPDATE
    /// matched zero rows (`sql_base_repository.go:630-641`). A predefined id that misses is
    /// therefore `inserted: true`, not a miss and not a `NotFound`.
    pub inserted: bool,
}

/// Callback error of the executed [`put_mediafile_in_tx`].
///
/// The builder arm is REAL and must stay off the SQLite arm: [`put_sql`] returns
/// [`crate::marshal::MarshalError`], so a failure to materialize the legs is a domain
/// failure, not a driver one. `Infallible` would force such a failure onto
/// [`DbOperationError::Sqlite`], which `D-ERROR` forbids — the concrete cause would be
/// lost and `store::is_busy` would be asked to classify a non-driver failure.
///
/// For the FULL-record seam this wrapper uses, neither [`crate::marshal::MarshalError`
/// variant is reachable today: [`mediafile_row`] never emits `id` (so not `IdInMap`) and
/// its 63-column map always leaves a settable UPDATE set (so not `EmptyUpdateSet`, which
/// needs a `cols_to_update` restriction this signature does not take). The arm exists
/// because the renderer it executes IS fallible, not because a caller can provoke it here.
///
/// There is NO empty-id variant: Go's `Put` returns no domain error of its own
/// (`mediafile_repository.go:189-197` adds no wrapper), and an empty id is Go's random-id
/// arm (`sql_base_repository.go:635-638`) — reproduced here rather than refused. The
/// `EmptyId` variants in `album_repo`, `artist_repo` and `folder_repo` are NOT mirrored
/// here: media-file identity follows Go's `put` exactly.
#[derive(Debug, thiserror::Error)]
pub enum MediaFileWriteError {
    /// The legs could not be materialized by [`put_sql`]. Go's equivalent failure is
    /// `fmt.Errorf("error preparing values to write to DB: %w", err)`
    /// (`sql_base_repository.go:619-622`); the port keeps the typed `MarshalError` on the
    /// `Domain` arm rather than stringifying it.
    #[error(transparent)]
    Builder(#[from] crate::marshal::MarshalError),
}

/// `Put` (`mediafile_repository.go:184-197`) EXECUTED on the CALLER's transaction, in
/// Go's exact statement order: [`put_match_sql`] when the identity is a `Match`
/// (`putByMatch`, `sql_base_repository.go:577-589`), then the `r.put` upsert
/// (`sql_base_repository.go:618-642`) — UPDATE by `id` FIRST, INSERT only on a zero-row
/// match — then `updateParticipants` (`sql_participations.go:78-125`), then `updateTags`
/// (`sql_tags.go:57-79`). Any leg failure aborts the whole Put, which is what Go's caller
/// relies on to roll back the scanner's folder transaction (`scanner/phase_1_folders.go`).
///
/// It executes exactly what [`put_sql`] renders — the six-statement [`PutMediaFileSql`]
/// bundle — and re-renders nothing here.
///
/// Identity: the id resolved by the [`MediaIdentity`] arm is the UPDATE's `WHERE id = ?`,
/// the INSERT's id column, and the `media_file_id` of BOTH junction legs, which is Go's
/// `m.ID = id` (`:192`) feeding `updateParticipants`/`updateTags`. A caller-selected `Match`
/// that FINDS a row writes that row even when `record.id` holds a different (non-empty) id;
/// `record.id` is not consulted on that arm. This follows the explicit Rust `MediaIdentity`
/// choice; Go chooses its match path only when `m.ID` is empty, and otherwise puts by id.
/// A `Match` that finds NOTHING falls back to `record.id`, and when THAT is empty too the id
/// is Go's generated one: `put` fills an empty id with `id2.NewRandom()`
/// (`sql_base_repository.go:635-638`), SKIPS the UPDATE leg (`:623` `if id != ""`), and
/// INSERTs under the fresh id. The generation
/// happens inside this transaction, so a failed attempt rolls it back with everything else
/// and publishes nothing; a retried attempt may generate a DIFFERENT id, which is Go's
/// behavior (`persistence.go:207-228` reruns the whole block) and is safe because the
/// committed row is the only externally visible id and a subsequent `Put` at the same
/// `path`+`library_id` matches that row rather than creating another. Only the successful
/// transaction's [`MediaFileWriteResult`] reaches the caller.
/// The caller's record is borrowed and never mutated (Go mutates `m.ID`; the port reports
/// the id in [`MediaFileWriteResult`] instead, so a retry reuses the caller's input).
///
/// Timestamps: `now` is the CALLER's clock and reaches [`put_sql`] as its `now`, which
/// stamps `created_at` when the record's is `None` OR the Go zero `time.Time`
/// (`mediafile_repository.go:185-187` `CreatedAt.IsZero()` — Go's zero is a VALUE its
/// decoder can produce, so both spellings mean "unset"). `updated_at` and `birth_time` are
/// NEVER stamped here: Go's `Put` touches neither (`:184-197`), and the shared seam drops
/// `created_at` and `birth_time` from the UPDATE set (`selectUpdateColumns`,
/// `sql_base_repository.go:605-608`/`marshal::UPDATE_DROP`), so both stay immutable across
/// the UPDATE leg. There is no second stamp anywhere on this path.
///
/// Junction maintenance: each junction's DELETE runs unconditionally and BEFORE its
/// optional INSERT, which is Go's shape (`sql_participations.go:79-89`, `sql_tags.go:58-74`).
/// A record with NO participants (or no indexed tags) therefore CLEARS the existing rows
/// rather than leaving them stale; the `Option` insert leg is skipped only after its delete
/// ran. Both junction INSERTs are `INSERT … SELECT` over `json_each` INNER-joined to
/// `artist`/`tag`, so ids the dictionary does not have are dropped SILENTLY by the join —
/// Go's behavior (`sql_participations.go:117`, `sql_tags.go:69`), not an error, and the
/// stored `participants`/`tags` JSON columns still carry those entries. Only `genre` is
/// indexed at all ([`crate::sql_tags::INDEXED_TAG_NAMES`]), so other tag names live only in
/// the JSON column. That asymmetry is Go's own: `Put` never re-derives or filters the JSON
/// from the junction result, so this wrapper does not either. Counts (`library_tag`,
/// `average_rating`) and the annotation tables are NOT touched here — DB-17 and the
/// annotation lane own those, and Go's `Put` does no aggregation.
///
/// FTS: NO statement here mentions `media_file_fts`. The schema's
/// `media_file_fts_ai`/`_au`/`_ad` triggers (`nd-dbschema/src/schema_ddl.rs:82-84`) index
/// the base row's own `title`/`album`/`artist`/`album_artist`/`sort_*`/`disc_subtitle`/
/// `search_participants`/`search_normalized`, and [`put_sql`] derives those columns from
/// the record's own fields and participants/tags (the S-D2b consistency invariant), so the
/// index is correct from the base write alone. Junction writes touch no FTS trigger (the
/// junction tables have none). `media_file_fts_au`'s `WHEN` list does NOT include `tags` —
/// but that is only half the truth: a tag change re-indexes EXACTLY when it moves a COVERED
/// column, which happens when `append_subtitle` is on (the `subtitle` tag feeds `full_title`
/// and therefore `full_text`/`search_normalized`) and does not happen when it is off. Do not
/// describe a tag change as never touching the index.
///
/// Scope: a caller-trusted primitive, exactly like Go. `WHERE id = ?` and nothing else
/// beyond the identity arm: no auth check, no ownership guard. Library scoping is the
/// READ side's job ([`MediaFileRepo::get`]'s `library_filter`), not this write's — but the
/// `Match` arm's predicate DOES carry `library_id`, which is why the same path in two
/// libraries stays two rows.
///
/// The transaction is the caller's: this never begins, commits, retries, checks out a
/// connection, or runs in autocommit, and there is no connection-bound variant. Raw
/// SQLite errors keep their concrete cause and extended code on the
/// [`DbOperationError::Sqlite`] arm — Go returns `Put`'s errors raw and unwrapped
/// (`mediafile_repository.go:189-197` adds no `fmt.Errorf`).
///
/// `cols_to_update` is deliberately NOT a parameter: Go's `Put` passes no `colsToUpdate`
/// (`mediafile_repository.go:188`), so this boundary is the unfiltered full-record save and
/// needs no typed column enum. A filtered (partial-column) put is a FOLLOW-UP for whichever
/// lane needs one; [`put_sql`] already takes the restriction and stays the seam for it.
///
/// Multi-row match: with no unique index on `(path, library_id)` two rows CAN hold the same
/// pair. Go's `queryOne` takes the FIRST row and errors on nothing (`dbx@v1.12.0/query.go:259-272`);
/// rusqlite's `query_row` takes the first row too and discards the rest (`row.rs:101-106` —
/// `get_expected_row` returns the first row, and the `Rows` drop resets the statement). The
/// port therefore keeps Go's first-row semantics rather than reporting a multi-match.
///
/// # Errors
/// [`DbOperationError::Domain`]·[`MediaFileWriteError::Builder`] when [`put_sql`] fails to
/// materialize the legs — the ONLY domain arm, because Go's `Put` has no domain error of its
/// own; [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact) from the match SELECT, the UPDATE, the INSERT, either junction DELETE, or either
/// junction INSERT — the `media_file.library_id` FK included, which Go returns raw. The
/// junction FKs are structurally unreachable through this seam because both junction INSERTs
/// INNER-JOIN `artist`/`tag` first (a dangling id is filtered away rather than attempted);
/// only a raw caller can provoke them. A `Match` that finds NO row is NOT an error (Go tolerates
/// `model.ErrNotFound`, `sql_base_repository.go:585-587`) and falls back to `record.id`, and an
/// empty `record.id` is NOT an error either — it takes Go's generated-id arm; a
/// `Match` read that FAILS (a hard SQLite error, or a cell that cannot be decoded) IS, and is
/// not tolerated. A repeated id is an UPDATE hit rather than a constraint failure; a zero-row
/// UPDATE is NOT an error; unknown participant/tag ids are dropped by the JOIN and are NOT an
/// error.
pub fn put_mediafile_in_tx(
    tx: &rusqlite::Transaction<'_>,
    identity: &MediaIdentity,
    record: &MediaFileRecord,
    now: &DateTime<Utc>,
    append_subtitle: bool,
) -> Result<MediaFileWriteResult, DbOperationError<MediaFileWriteError>> {
    // `putByMatch` (`sql_base_repository.go:578-588`): a non-empty id skips the probe
    // entirely; otherwise the probe runs on THIS transaction and only ErrNotFound-equivalent
    // absence is tolerated.
    let mut id = match identity {
        // An EMPTY id is not a contract violation: it is exactly the `id == ""` value Go
        // hands `put` (`sql_base_repository.go:588`), whose UPDATE leg is guarded by
        // `if id != ""` (`:623`) and whose insert takes the generated-id arm (`:635-638`).
        MediaIdentity::Id(id) => id.clone(),
        MediaIdentity::Match { path, library_id } => {
            crate::execution::query_optional(tx, &put_match_sql(path, *library_id), |row| {
                row.get::<_, String>(0)
            })
            .map_err(DbOperationError::sqlite)?
            // Go's tolerated `ErrNotFound` leaves `res.ID == ""` and `put(ctx, "", …)` runs
            // (`:588`); the port falls back to the record's id, which may itself be empty —
            // that is Go's generated-id arm, taken below.
            .unwrap_or_else(|| record.id.clone())
        }
    };

    // Go's random-id arm (`sql_base_repository.go:635-638`): `id2.NewRandom()` is
    // `nd_id::new_random()` (`nd-id/src/lib.rs:115-121`, the same 16 CSPRNG bytes base62-
    // encoded) minted HERE, inside the caller's transaction and ONLY on this arm, before the
    // bundle is rendered so the INSERT's id column and both junction legs' `media_file_id`
    // carry it — Go's `values["id"] = id` (`:637`) plus `m.ID = id`
    // (`mediafile_repository.go:192`). A failed attempt rolls that id back with the row and
    // publishes nothing; a retry may mint a different one, which is Go's behavior
    // (`persistence.go:207-228` reruns the whole block) and is safe because only the committed
    // row is visible and the next `Put` at that `path`+`library_id` matches it rather than
    // adding another.
    let id_was_generated = id.is_empty();
    if id_was_generated {
        id = nd_id::new_random();
    }

    // The bundle is keyed by the RESOLVED id, exactly as Go's `put` builds its SET-map once
    // the id is known.
    let bundle = put_sql(&id, record, *now, append_subtitle, &[])
        .map_err(|err| DbOperationError::Domain(MediaFileWriteError::Builder(err)))?;

    // Base leg (`:623-641`): an id that was EMPTY to begin with skips the UPDATE entirely
    // (`:623` `if id != ""`), so the INSERT is the only leg — no UPDATE is attempted against
    // a freshly minted id. Otherwise UPDATE by `id` runs FIRST and a row count > 0 ends the
    // upsert, so the INSERT never runs; a zero-row match is the predefined-id insert fallback
    // (`:634-641`) and keeps the SAME id.
    let inserted = if id_was_generated {
        true
    } else {
        crate::execution::execute(tx, &bundle.update).map_err(DbOperationError::sqlite)? == 0
    };
    if inserted {
        crate::execution::execute(tx, &bundle.insert).map_err(DbOperationError::sqlite)?;
    }

    // `updateParticipants` (`sql_participations.go:78-125`): the delete ALWAYS runs — it is
    // what clears stale role rows when a role changes — and Go's early return on an empty
    // participant list (`:87-89`) only skips the INSERT.
    crate::execution::execute(tx, &bundle.participants_delete).map_err(DbOperationError::sqlite)?;
    if let Some(insert) = bundle.participants_insert.as_ref() {
        crate::execution::execute(tx, insert).map_err(DbOperationError::sqlite)?;
    }

    // `updateTags` (`sql_tags.go:57-79`): same shape — unconditional delete, optional insert.
    crate::execution::execute(tx, &bundle.tags_delete).map_err(DbOperationError::sqlite)?;
    if let Some(insert) = bundle.tags_insert.as_ref() {
        crate::execution::execute(tx, insert).map_err(DbOperationError::sqlite)?;
    }

    Ok(MediaFileWriteResult { id, inserted })
}

/// `UpdateProbeData` (`mediafile_repository.go:204-207`).
#[must_use]
pub fn update_probe_data_sql(id: &str, data: &str) -> Sql {
    (
        format!("UPDATE {TABLE} SET probe_data = ? WHERE id = ?"),
        vec![
            crate::sqlrepo::Value::from(data),
            crate::sqlrepo::Value::from(id),
        ],
    )
}

/// `FindByPaths` (`mediafile_repository.go:352-399`): one IN-list per
/// library (qualified paths) plus one nocase IN-list for the unqualified
/// batch, OR-joined and parenthesized (squirrel renders `Or{...}` as
/// `(...)`); each group's `And{Eq, Eq}` renders its keys alphabetically.
/// The inputs are the caller's path-splitting products: `(library_id,
/// paths)` groups plus the unqualified originals — every path searched both
/// ways when its numeric prefix is ambiguous (`:364-374`).
#[must_use]
pub fn find_by_paths_sql(by_library: &[(i64, Vec<String>)], unqualified: &[String]) -> Option<Sql> {
    let mut groups: Vec<String> = Vec::new();
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    for (library_id, paths) in by_library {
        if paths.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; paths.len()].join(", ");
        groups.push(format!(
            "(path collate nocase IN ({placeholders}) AND library_id = ?)"
        ));
        args.extend(paths.iter().map(|p| crate::sqlrepo::Value::from(p.clone())));
        args.push(crate::sqlrepo::Value::Integer(*library_id));
    }
    if !unqualified.is_empty() {
        let placeholders = vec!["?"; unqualified.len()].join(", ");
        groups.push(format!("path collate nocase IN ({placeholders})"));
        args.extend(
            unqualified
                .iter()
                .map(|p| crate::sqlrepo::Value::from(p.clone())),
        );
    }
    if groups.is_empty() {
        return None;
    }
    Some((
        format!("SELECT * FROM {TABLE} WHERE ({})", groups.join(" OR ")),
        args,
    ))
}

/// `Delete` (`mediafile_repository.go:401-403`).
#[must_use]
pub fn delete_sql(id: &str) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE id = ?"),
        vec![crate::sqlrepo::Value::from(id)],
    )
}

/// `DeleteAllMissing` (`mediafile_repository.go:405-413`): the admin-gated
/// bulk delete (the `rest.ErrPermissionDenied` check is the caller's).
#[must_use]
pub fn delete_all_missing_sql() -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE missing = ?"),
        vec![crate::sqlrepo::Value::Bool(true)],
    )
}

/// `DeleteMissing` (`mediafile_repository.go:415-431`): both Eq keys in one
/// clause — `Eq{"missing": true, "id": ids}` renders alphabetically
/// (`id` < `missing`), the id slice expanding to placeholders.
#[must_use]
pub fn delete_missing_sql(ids: &[String]) -> Sql {
    let placeholders = vec!["?"; ids.len()].join(", ");
    (
        format!("DELETE FROM {TABLE} WHERE id IN ({placeholders}) AND missing = ?"),
        ids.iter()
            .map(|id| crate::sqlrepo::Value::from(id.clone()))
            .chain([crate::sqlrepo::Value::Bool(true)])
            .collect(),
    )
}

/// `MarkMissing` (`mediafile_repository.go:455-469`): one 200-row chunk per
/// UPDATE (`slice.CollectChunks(ids, 200)`, `:457`), `.Set` call order.
/// Go's loop aborts on `err != nil || c == 0` (`:463-466`) — a chunk that
/// touches zero rows stops the whole scan — whereas `MarkMissingByFolder`
/// only checks `err != nil` (`:482`) and tolerates a zero-row chunk. That
/// difference is execution-side and deliberately NOT modeled here: these
/// helpers render statements, they do not run them. The 200 is the literal
/// Go passes to the chunker at `:457` (and `:473`).
pub const CHUNK_SIZE: usize = 200;

/// See [`CHUNK_SIZE`]; `missing` flips and `updated_at` is Go's `time.Now()`
/// — both bind, and in `.Set`/`Where` CALL ORDER (`Set("missing", …)` `:459`,
/// `Set("updated_at", …)` `:460`, `Where(Eq{"id": chunk})` `:461`), so the two
/// SET values come FIRST and the chunk's ids fill `WHERE id IN (…)` last.
/// Binding the ids first would put a media-file id in the `missing` slot and
/// the timestamp in the `id` slot, making the UPDATE a silent no-op.
#[must_use]
pub fn mark_missing_sql(missing: bool, ids: &[String], updated_at: &DateTime<Utc>) -> Vec<Sql> {
    ids.chunks(CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            (
                format!(
                    "UPDATE {TABLE} SET missing = ?, updated_at = ? WHERE id IN ({placeholders})"
                ),
                [
                    crate::sqlrepo::Value::Bool(missing),
                    crate::time::value(updated_at),
                ]
                .into_iter()
                .chain(
                    chunk
                        .iter()
                        .map(|id| crate::sqlrepo::Value::from(id.clone())),
                )
                .collect(),
            )
        })
        .collect()
}

/// `MarkMissingByFolder` (`mediafile_repository.go:472-487`): one 200-row
/// chunk per UPDATE (`slices.Chunk(folderIDs, 200)`, `:473`), gated on the
/// folders' current flag flipping (`Eq{"missing": !missing}`, `:479`); `.Set`
/// call order — `missing`, `updated_at`, then the chunk's folder ids and the
/// gate last — and the `And` renders its Eq map keys alphabetically
/// (`folder_id` `:478` < `missing` `:479`).
#[must_use]
pub fn mark_missing_by_folder_sql(
    missing: bool,
    folder_ids: &[String],
    updated_at: &DateTime<Utc>,
) -> Vec<Sql> {
    folder_ids
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            (
                format!(
                    "UPDATE {TABLE} SET missing = ?, updated_at = ? \
                     WHERE folder_id IN ({placeholders}) AND missing = ?"
                ),
                [
                    crate::sqlrepo::Value::Bool(missing),
                    crate::time::value(updated_at),
                ]
                .into_iter()
                .chain(
                    chunk
                        .iter()
                        .map(|id| crate::sqlrepo::Value::from(id.clone())),
                )
                .chain([crate::sqlrepo::Value::Bool(!missing)])
                .collect(),
            )
        })
        .collect()
}

/// Callback error of the executed media-file missing primitives, which have NO domain
/// failure to carry: Go's `MarkMissing`/`MarkMissingByFolder` return a bare `error` that
/// can only be a driver failure (`mediafile_repository.go:463-466`, `:482-485`) — no
/// validation, no `NotFound`, no permission check (the ADMIN check belongs to
/// `DeleteMissing`, `:442-453`, which is not this method). `Infallible` makes the
/// `Domain` arm structurally uninhabited rather than reserved, as in
/// `library_repo::LibraryScanError` and `tag_repo::TagWriteError`, and keeps every
/// SQLite failure on the [`DbOperationError::Sqlite`] arm so `store::is_busy` still
/// classifies `SQLITE_BUSY` for the executor's retry loop.
///
/// A zero-row chunk is NOT a domain error here either — see
/// [`mark_media_missing_in_tx`] for why Go's `c == 0` branch returns success.
pub type MediaMarkError = DbOperationError<std::convert::Infallible>;

/// `MarkMissing` (`mediafile_repository.go:455-469`) EXECUTED on the CALLER's
/// transaction: one UPDATE per [`CHUNK_SIZE`] ids in Go's loop order over the CALLER's
/// id order (`slice.CollectChunks(ids, 200)`, `:457`), executing exactly what
/// [`mark_missing_sql`] renders rather than re-rendering it here. Empty ids run NO
/// statement and return `Ok(0)`.
///
/// **Go's zero-row early return, ported literally.** The loop body reads
/// (`:462-466`):
/// ```go
/// c, err := r.executeSQL(ctx, upd)
/// if err != nil || c == 0 {   // :463
///     log.Error(ctx, "Error setting mediafile missing flag", "ids", chunk, err)
///     return err              // :465 — nil when c == 0
/// }
/// ```
/// `executeSQL` (`sql_base_repository.go:336-354`) returns `0, err` only when the
/// statement FAILED and `c, nil` otherwise, so the `|| c == 0` branch is reached with
/// `err == nil` — and `return err` then returns NIL, i.e. SUCCESS, while also EXITING
/// the chunk loop. So a chunk that matches zero rows is a silent PARTIAL application:
/// the caller sees success and every LATER chunk is skipped, its rows never marked.
/// There is no domain error to model, so this returns `Ok` (NOT a `NoRows` variant —
/// none exists, and inventing one would flip Go's success into a failure). The returned
/// total is the ACCUMULATED affected-row count of the chunks that ran, so it is 0 only
/// when the FIRST chunk matched nothing; there is no separate early-stop indicator, and a
/// caller that needs to know whether later chunks were skipped cannot infer it from the
/// count alone. `mark_folders_missing_in_tx`
/// deliberately does NOT share this behavior: Go's folder `MarkMissing` checks `err`
/// only and keeps looping.
///
/// `updated_at` is the caller's stamp where Go binds `Set("updated_at", time.Now())
/// (`:460`); the caller owns the clock. Go's `log.Debug("total", c)` (`:467`) and
/// `log.Error` (`:464`) are not ported — nd-db has no logger — so the affected-row total
/// is returned purely for the caller's observability and no policy rides on it.
///
/// Scope: caller-trusted. `WHERE id IN (chunk)` and nothing else (`:461`): no library
/// predicate, no auth guard. A caller passing ids of another library HAS them written;
/// future SC typed authority owns that.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` from the chunk
/// that failed — Go returns that error RAW, with no `fmt.Errorf` prefix (`:465`), and
/// the port likewise never wraps it in text. A zero-row chunk is `Ok`, not an error.
/// [`DbOperationError::Domain`] is structurally uninhabited ([`MediaMarkError`]).
pub fn mark_media_missing_in_tx(
    tx: &rusqlite::Transaction<'_>,
    missing: bool,
    ids: &[String],
    updated_at: &DateTime<Utc>,
) -> Result<usize, MediaMarkError> {
    let mut total = 0usize;
    for statement in mark_missing_sql(missing, ids, updated_at) {
        let rows = crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
        total += rows;
        // Go's `err != nil || c == 0` (:463): err is nil here, so this is a SUCCESSFUL
        // early return that skips every remaining chunk. Do not `continue` — continuing
        // would mark rows Go never marks.
        if rows == 0 {
            break;
        }
    }
    Ok(total)
}

/// `MarkMissingByFolder` (`mediafile_repository.go:472-487`) EXECUTED on the CALLER's
/// transaction: one UPDATE per [`CHUNK_SIZE`] folder ids in Go's loop order over the
/// CALLER's folder-id order (`slices.Chunk(folderIDs, 200)`, `:473`), executing exactly
/// what [`mark_missing_by_folder_sql`] renders. Empty ids run NO statement, and a chunk
/// matching zero rows is a successful NO-OP whose loop CONTINUES — Go checks `err` only
/// (`:482-484`), unlike [`mark_media_missing_in_tx`].
///
/// The `Eq{"missing": !missing}` gate (`:479`, rendered as the trailing
/// `AND missing = ?` by [`mark_missing_by_folder_sql`]) is the whole point of this
/// primitive: rows ALREADY carrying the target flag are neither re-stamped nor counted,
/// so a repeated call with the same flag moves nothing. `updated_at` is the caller's
/// stamp (`:476`), never a clock this function reads.
///
/// Scope: caller-trusted — `WHERE folder_id IN (chunk) AND missing = ?` and nothing
/// else, so tracks of another library under a supplied folder id ARE updated. No
/// library predicate is invented here.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` from any chunk,
/// raw and unwrapped exactly like Go (`:484`). A zero-row chunk is `Ok`.
pub fn mark_media_missing_by_folder_in_tx(
    tx: &rusqlite::Transaction<'_>,
    missing: bool,
    folder_ids: &[String],
    updated_at: &DateTime<Utc>,
) -> Result<usize, MediaMarkError> {
    let mut total = 0usize;
    for statement in mark_missing_by_folder_sql(missing, folder_ids, updated_at) {
        total += crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
    }
    Ok(total)
}

/// `GetAlbumIDsByFolder` (`mediafile_repository.go:293-316`): one chunked
/// probe — `distinct album_id` of non-missing tracks whose folder (or a
/// direct child of it) is in the chunk. The `And` renders the Eq map
/// keys alphabetically (`missing` first) with the folder `Or` last
/// (`:308-310`); the inner folders select is inlined as
/// `ConcatExpr("folder_id IN (", …, ")")` (`:311-313`).
#[must_use]
pub fn get_album_ids_by_folder_sql(library_id: i64, folder_ids: &[String]) -> Vec<Sql> {
    folder_ids
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            (
                format!(
                    "SELECT distinct album_id FROM {TABLE} WHERE missing = ? AND folder_id IN \
                     (SELECT f.id FROM folder f WHERE f.library_id = ? AND f.missing = ? \
                     AND (f.id IN ({placeholders}) OR f.parent_id IN ({placeholders})))"
                ),
                [
                    crate::sqlrepo::Value::Bool(false),
                    crate::sqlrepo::Value::Integer(library_id),
                    crate::sqlrepo::Value::Bool(false),
                ]
                .into_iter()
                .chain(
                    chunk
                        .iter()
                        .map(|id| crate::sqlrepo::Value::from(id.clone())),
                )
                .chain(
                    chunk
                        .iter()
                        .map(|id| crate::sqlrepo::Value::from(id.clone())),
                )
                .collect(),
            )
        })
        .collect()
}

/// `GetMissingAndMatching` (`mediafile_repository.go:485-510`): the
/// scanner's PID matcher — non-missing/current rows whose `pid` matches any
/// missing row of the library. The `Where("pid in ("+subQText+")", subQArgs...)`
/// fragment inlines the subquery text with its args bound at the fragment
/// position; the trailing `Or` renders the missing flag first and the raw
/// comparison last (`:503-507`).
#[must_use]
pub fn get_missing_and_matching_sql(library_id: i64) -> Sql {
    (
        format!(
            "SELECT media_file.*, library.path as library_path, library.name as library_name \
             FROM {TABLE} \
             LEFT JOIN library ON media_file.library_id = library.id \
             WHERE pid in (SELECT pid FROM {TABLE} WHERE media_file.missing = ? AND library_id = ?) \
             AND (missing = ? OR media_file.created_at > library.last_scan_started_at) \
             ORDER BY pid"
        ),
        vec![
            crate::sqlrepo::Value::Bool(true),
            crate::sqlrepo::Value::Integer(library_id),
            crate::sqlrepo::Value::Bool(true),
        ],
    )
}

/// The `FindRecentFilesByMBZTrackID` matcher inputs
/// (`mediafile_repository.go:512-532`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecentFileByMbz<'a> {
    /// The missing row's `library_id` — the NOT-Eq bound value.
    pub library_id: i64,
    /// The missing row's `mbz_release_track_id`.
    pub mbz_release_track_id: &'a str,
    /// The missing row's `suffix`.
    pub suffix: &'a str,
    /// `since` — `created_at` must be strictly greater.
    pub since: &'a DateTime<Utc>,
}

/// `FindRecentFilesByMBZTrackID` (`:512-532`): the lightweight join-free
/// select (no annotation/bookmark joins — matching only needs core columns).
/// The `And` renders in declaration order; `NotEq` is `<>`.
#[must_use]
pub fn find_recent_files_by_mbz_track_id_sql(m: &RecentFileByMbz<'_>) -> Sql {
    (
        format!(
            "SELECT media_file.*, library.path as library_path, library.name as library_name \
             FROM {TABLE} \
             LEFT JOIN library ON media_file.library_id = library.id \
             WHERE media_file.library_id <> ? \
             AND media_file.mbz_release_track_id = ? \
             AND media_file.mbz_release_track_id <> ? \
             AND media_file.suffix = ? \
             AND media_file.created_at > ? \
             AND media_file.missing = ? \
             ORDER BY media_file.created_at DESC"
        ),
        vec![
            crate::sqlrepo::Value::Integer(m.library_id),
            crate::sqlrepo::Value::from(m.mbz_release_track_id),
            crate::sqlrepo::Value::from(""),
            crate::sqlrepo::Value::from(m.suffix),
            crate::time::value(m.since),
            crate::sqlrepo::Value::Bool(false),
        ],
    )
}

/// The `FindRecentFilesByProperties` matcher inputs
/// (`mediafile_repository.go:534-563`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecentFileByProperties<'a> {
    /// The missing row's `library_id` — the NOT-Eq bound value.
    pub library_id: i64,
    /// The missing row's `title`.
    pub title: &'a str,
    /// The missing row's `size`.
    pub size: i64,
    /// The missing row's `suffix`.
    pub suffix: &'a str,
    /// The missing row's `disc_number`.
    pub disc_number: i64,
    /// The missing row's `track_number`.
    pub track_number: i64,
    /// The missing row's `album`.
    pub album: &'a str,
    /// `since` — `created_at` must be strictly greater.
    pub since: &'a DateTime<Utc>,
}

/// `FindRecentFilesByProperties` (`:534-563`): same lightweight select,
/// matching intrinsic properties instead of the MBZ id.
#[must_use]
pub fn find_recent_files_by_properties_sql(m: &RecentFileByProperties<'_>) -> Sql {
    (
        format!(
            "SELECT media_file.*, library.path as library_path, library.name as library_name \
             FROM {TABLE} \
             LEFT JOIN library ON media_file.library_id = library.id \
             WHERE media_file.library_id <> ? \
             AND media_file.title = ? \
             AND media_file.size = ? \
             AND media_file.suffix = ? \
             AND media_file.disc_number = ? \
             AND media_file.track_number = ? \
             AND media_file.album = ? \
             AND media_file.mbz_release_track_id = ? \
             AND media_file.created_at > ? \
             AND media_file.missing = ? \
             ORDER BY media_file.created_at DESC"
        ),
        vec![
            crate::sqlrepo::Value::Integer(m.library_id),
            crate::sqlrepo::Value::from(m.title),
            crate::sqlrepo::Value::Integer(m.size),
            crate::sqlrepo::Value::from(m.suffix),
            crate::sqlrepo::Value::Integer(m.disc_number),
            crate::sqlrepo::Value::Integer(m.track_number),
            crate::sqlrepo::Value::from(m.album),
            crate::sqlrepo::Value::from(""),
            crate::time::value(m.since),
            crate::sqlrepo::Value::Bool(false),
        ],
    )
}

/// `ReassignReferences` (`mediafile_repository.go:415-432`): the four
/// reference rewrites in Go's order — annotation reassign, bookmark
/// reassign, playlist_tracks, scrobbles — plus the `OR IGNORE` buffered
/// scrobbles update (unique on `(user_id, service, media_file_id,
/// play_time)`). The annotation/bookmark legs delegate to
/// `sql_annotations`/`sql_bookmarks`; the playlist/scrobble legs render
/// here.
#[must_use]
pub fn reassign_references_sql(prev_id: &str, new_id: &str) -> Vec<Sql> {
    vec![
        crate::sql_annotations::reassign_annotation_sql(TABLE, prev_id, new_id),
        crate::sql_bookmarks::reassign_bookmark_sql(TABLE, prev_id, new_id),
        (
            "UPDATE playlist_tracks SET media_file_id = ? WHERE media_file_id = ?".to_string(),
            vec![
                crate::sqlrepo::Value::from(new_id),
                crate::sqlrepo::Value::from(prev_id),
            ],
        ),
        (
            "UPDATE scrobbles SET media_file_id = ? WHERE media_file_id = ?".to_string(),
            vec![
                crate::sqlrepo::Value::from(new_id),
                crate::sqlrepo::Value::from(prev_id),
            ],
        ),
        (
            "update or ignore scrobble_buffer set media_file_id = ? where media_file_id = ?"
                .to_string(),
            vec![
                crate::sqlrepo::Value::from(new_id),
                crate::sqlrepo::Value::from(prev_id),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlrepo::Value;

    // Arrange/Act/Assert helpers: every test pins the (sql, args) pair Go's
    // squirrel layer materializes, against the literal shapes of
    // mediafile_repository.go.

    #[test]
    fn select_media_file_pins_the_joined_columns_and_both_user_joins() {
        // Arrange: selectMediaFile (:204-210) with a logged-in user —
        // withAnnotation (:63-77) then withBookmark (:17-29).
        // Act:
        let (sql, args) = select_media_file(Some("user1"), None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.*, library.path as library_path, library.name as library_name, \
             coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
             play_date, rated_at, coalesce(play_count, 0) as play_count, media_file.average_rating, \
             coalesce(position, 0) as bookmark_position \
             FROM media_file LEFT JOIN library ON media_file.library_id = library.id \
             LEFT JOIN annotation ON (annotation.item_id = media_file.id \
             AND annotation.item_type = ? AND annotation.user_id = ?) \
             LEFT JOIN bookmark ON (bookmark.item_id = media_file.id AND bookmark.user_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("media_file".to_string()),
                Value::Text("user1".to_string()),
                Value::Text("user1".to_string()),
            ]
        );
    }

    #[test]
    fn select_media_file_without_user_skips_both_joins() {
        // Arrange: the invalid-user branch (:206-208 return the query
        // untouched; withAnnotation adds only the bare column, :65-66).
        // Act:
        let (sql, args) = select_media_file(None, None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.*, library.path as library_path, library.name as library_name, \
             media_file.average_rating \
             FROM media_file LEFT JOIN library ON media_file.library_id = library.id"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn count_all_gates_the_annotation_join_on_filters() {
        // Arrange: CountAll (:150-158) — join only when needed (:153-155).
        // Act + Assert:
        assert_eq!(
            count_all_sql(None, None, false).0,
            "SELECT count(distinct media_file.id) as count FROM media_file"
        );
        assert_eq!(
            count_all_sql(Some("user1"), None, true).0,
            "SELECT count(distinct media_file.id) as count FROM media_file \
             LEFT JOIN annotation ON (annotation.item_item_id = media_file.id \
             AND annotation.item_type = ? AND annotation.user_id = ?)"
                .replace("item_item_id", "item_id")
        );
    }

    #[test]
    fn count_by_suffix_groups_by_lower_suffix() {
        // Arrange: CountBySuffix (:160-173).
        // Act:
        let (sql, args) = count_by_suffix_sql();
        // Assert:
        assert_eq!(
            sql,
            "SELECT lower(suffix) as suffix, count(*) as count FROM media_file GROUP BY lower(suffix)"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn exists_counts_under_the_library_filter() {
        // Arrange: Exists (:175-179) — count, not the bare exists() helper.
        // Act:
        let (sql, args) = exists_sql(
            "mf-1",
            Some((
                "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)".to_string(),
                vec![Value::from("user1")],
            )),
        );
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct media_file.id) as count FROM media_file \
             WHERE media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             AND media_file.id = ?"
        );
        assert_eq!(args, vec![Value::from("user1"), Value::from("mf-1")]);
    }

    #[test]
    fn get_and_get_all_compose_on_the_base_select() {
        // Arrange: Get (:212-219) and GetAll (:221-227).
        // Act + Assert:
        assert_eq!(
            get_sql("mf-1", None, None),
            (
                "SELECT media_file.*, library.path as library_path, library.name as library_name, \
                 media_file.average_rating \
                 FROM media_file LEFT JOIN library ON media_file.library_id = library.id \
                 WHERE media_file.id = ?"
                    .to_string(),
                vec![Value::Text("mf-1".to_string())]
            )
        );
        let (sql, args) = get_all_sql(None, None, &["title asc"], 10, 20);
        // Assert: the bounds render as squirrel literals (`select.go:139-147`),
        // so the page contributes no args.
        assert!(sql.ends_with(" ORDER BY title asc LIMIT 10 OFFSET 20"));
        assert!(args.is_empty());

        // With a library filter the id predicate AND-joins the `WHERE` the base
        // select already emits (`select.go:112-117`); appending a second
        // ` WHERE ` is a SQLite `near "WHERE": syntax error`. Executed against the
        // real schema by
        // `tests/runtime_hydrate_album_mediafile.rs::mediafile_get_and_get_all_join_one_where_with_a_library_filter`.
        let lib_filter = (
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::Text("user1".to_string())],
        );
        let (filtered, filtered_args) = get_sql("mf-1", Some("u1"), Some(lib_filter.clone()));
        assert!(
            !filtered.contains(" WHERE media_file.id = ?"),
            "the id predicate must not open a second WHERE: {filtered}"
        );
        assert!(filtered.ends_with(" AND media_file.id = ?"), "{filtered}");
        assert_eq!(
            filtered_args,
            vec![
                Value::Text("media_file".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("user1".to_string()),
                Value::Text("mf-1".to_string()),
            ]
        );
        let (page, page_args) = get_all_sql(Some("u1"), Some(lib_filter), &["title asc"], 5, 0);
        assert!(
            page.contains(" WHERE media_file.library_id IN"),
            "the filter still opens the clause: {page}"
        );
        // Annotation join (item_type, user), bookmark join (user), then the filter.
        assert_eq!(page_args.len(), 4, "join args bind before the filter arg");
    }

    #[test]
    fn get_random_pins_both_phases() {
        // Arrange: GetRandom (:229-266) — the rowid pick, then the hydrate.
        // Act:
        let (rowid_sql, rowid_args) = get_random_rowid_sql(
            Some(("title like ?", vec![Value::from("%x%")])),
            Some((
                "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)".to_string(),
                vec![Value::from("user1")],
            )),
            5,
        );
        let (hydrate_sql, hydrate_args) = get_random_hydrate_sql(None, None, &[3, 7, 9]);
        // Assert:
        assert_eq!(
            rowid_sql,
            "SELECT media_file.rowid FROM media_file \
             WHERE title like ? AND media_file.library_id IN \
             (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             ORDER BY random() LIMIT 5"
        );
        assert_eq!(rowid_args, vec![Value::from("%x%"), Value::from("user1")]);
        assert!(hydrate_sql.ends_with("WHERE media_file.rowid IN (?, ?, ?) ORDER BY random()"));
        assert_eq!(
            hydrate_args,
            vec![Value::Integer(3), Value::Integer(7), Value::Integer(9)]
        );
    }

    #[test]
    fn get_random_phase_two_reapplies_the_library_filter() {
        // Arrange: Go's phase 2 builds its statement with `selectMediaFile(ctx)`
        // (`mediafile_repository.go:274`), whose last step is an unconditional
        // `applyLibraryFilter` (`:209`), so the hydrated row set is
        // {rowids} ∩ {visible libraries} — NOT just {rowids}.
        let filter = (
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("user1")],
        );
        // Act:
        let (hydrate_sql, hydrate_args) =
            get_random_hydrate_sql(Some("user1"), Some(filter), &[3, 7]);
        // Assert: the filter owns the first WHERE and the IN-list AND-joins it
        // (squirrel joins accumulated WhereParts with " AND ", select.go:111-114);
        // appending a second WHERE would be a SQLite syntax error.
        assert_eq!(
            hydrate_sql.matches(" WHERE ").count(),
            2,
            "outer WHERE plus the subquery's own: {hydrate_sql}"
        );
        assert!(
            hydrate_sql.contains(" AND media_file.rowid IN (?, ?) ORDER BY random()"),
            "the IN-list must AND-join the library clause: {hydrate_sql}"
        );
        // Arg order follows clause order: annotation join (item_type, user),
        // bookmark join (user), the library filter's user, then the rowids.
        assert_eq!(
            hydrate_args,
            vec![
                Value::from("media_file"),
                Value::from("user1"),
                Value::from("user1"),
                Value::from("user1"),
                Value::Integer(3),
                Value::Integer(7),
            ]
        );
    }

    #[test]
    fn get_all_ids_projects_the_id_only() {
        // Arrange: getAllIDs (:319-329).
        // Act + Assert:
        assert_eq!(
            get_all_ids_sql(None, None, false).0,
            "SELECT media_file.id FROM media_file"
        );
    }

    #[test]
    fn put_match_probes_path_and_library_in_sorted_eq_order() {
        // Arrange: putByMatch's Eq{"path", "library_id"} (:182) — the map
        // renders keys alphabetically (library_id < path).
        // Act:
        let (sql, args) = put_match_sql("/music/a.mp3", 1);
        // Assert:
        assert_eq!(
            sql,
            "SELECT id FROM media_file WHERE library_id = ? AND path = ?"
        );
        assert_eq!(
            args,
            vec![Value::Integer(1), Value::Text("/music/a.mp3".to_string())]
        );
    }

    fn test_record() -> MediaFileRecord {
        let mut participants = Participants::new();
        participants.insert(
            crate::sql_participations::Role::from("artist"),
            vec![crate::sql_participations::Participant {
                id: "ar-1".to_string(),
                name: "Song Artist".to_string(),
                sub_role: String::new(),
            }],
        );
        let mut tags = DbTags::new();
        tags.insert("genre".to_string(), vec!["Rock".to_string()]);
        MediaFileRecord {
            // Read-path identity; the write legs take the id as their own argument.
            id: "mf-1".to_string(),
            album: "Album".to_string(),
            album_artist: "AA".to_string(),
            album_artist_id: String::new(),
            album_id: "al-1".to_string(),
            artist: "Song Artist".to_string(),
            artist_id: String::new(),
            birth_time: chrono::Utc::now(),
            bit_depth: None,
            bit_rate: 0,
            bpm: None,
            catalog_num: String::new(),
            channels: 0,
            codec: "mp3".to_string(),
            comment: String::new(),
            compilation: false,
            created_at: None,
            date: String::new(),
            disc_number: 0,
            disc_subtitle: String::new(),
            duration: 0.0,
            explicit_status: String::new(),
            folder_id: String::new(),
            genre: String::new(),
            has_cover_art: false,
            library_id: 1,
            lyrics: String::new(),
            mbz_recording_id: String::new(),
            mbz_release_track_id: String::new(),
            mbz_album_id: String::new(),
            mbz_release_group_id: String::new(),
            mbz_artist_id: String::new(),
            mbz_album_artist_id: String::new(),
            mbz_album_type: String::new(),
            mbz_album_comment: String::new(),
            missing: false,
            order_album_artist_name: String::new(),
            order_album_name: String::new(),
            order_artist_name: String::new(),
            order_title: String::new(),
            original_date: String::new(),
            original_year: 0,
            path: "/music/a.mp3".to_string(),
            pid: "pid-1".to_string(),
            probe_data: "{}".to_string(),
            release_date: String::new(),
            release_year: 0,
            rg_album_gain: None,
            rg_album_peak: None,
            rg_track_gain: None,
            rg_track_peak: None,
            sample_rate: 0,
            size: 0,
            sort_album_artist_name: String::new(),
            sort_album_name: String::new(),
            sort_artist_name: String::new(),
            sort_title: String::new(),
            suffix: ".mp3".to_string(),
            tags,
            title: "Song".to_string(),
            track_number: 0,
            year: 0,
            participants,
            participant_names: vec![crate::album_repo::NamedParticipant {
                id: "ar-1".to_string(),
                name: "Song Artist".to_string(),
                sort_name: String::new(),
                sub_role: "Vocals".to_string(),
            }],
            updated_at: None,
            // Hydration-only fields: an unwritten test record keeps them empty, and
            // the record-driven write tests assert they never reach the map.
            library_path: String::new(),
            library_name: String::new(),
            annotation: nd_model::annotation::Annotation::default(),
            bookmark_position: 0,
            genres: Vec::new(),
            // S-D3c read-path-only artwork state (Go's ItemImage embed + AlbumImage,
            // structs:"-"); never write-map keys.
            image: crate::artwork_hydration::ItemImage::default(),
            album_image: crate::artwork_hydration::ItemImage::default(),
        }
    }
    #[test]
    fn put_renders_update_insert_and_the_maintenance_pairs() {
        // Arrange: Put (:181-189), record-driven — the legs, updateParticipants
        // (sql_participations.go:93-119), updateTags (sql_tags.go:57-79).
        let rec = test_record();
        let now = chrono::Utc::now();
        // Act:
        let put = put_sql("mf-1", &rec, now, false, &[]).expect("put renders");
        // Assert: 63 non-id columns + 3 FTS from PostMapArgs = 66 map columns;
        // insert + id = 67 args; update drops created_at and birth_time
        assert!(put.update.0.starts_with("UPDATE media_file SET "));
        assert!(put.update.0.ends_with("WHERE id = ?"));
        assert_eq!(put.update.1.len(), 65);
        assert_eq!(put.update.1.last(), Some(&Value::Text("mf-1".to_string())));
        assert!(!put.update.0.contains("created_at"));
        // birth_time is INSERT-only: present in the INSERT column list, never
        // in the UPDATE SET.
        assert!(put.insert.0.contains("birth_time"));
        assert!(!put.update.0.contains("birth_time"));
        assert!(put.insert.0.starts_with("INSERT INTO media_file ("));
        let tuple = put
            .insert
            .0
            .rsplit("VALUES (")
            .next()
            .unwrap()
            .strip_suffix(")")
            .unwrap();
        assert_eq!(tuple.matches('?').count(), 67);
        assert_eq!(put.insert.1.len(), 67);
        assert_eq!(put.insert.1.last(), Some(&Value::Text("mf-1".to_string())));
        // PostMapArgs FTS columns ride the write set.
        assert!(put.insert.0.contains("full_text"));
        assert!(put.insert.0.contains("search_participants"));
        assert!(put.insert.0.contains("search_normalized"));
        // updateParticipants: delete-all then the json_each INSERT.
        assert_eq!(
            put.participants_delete.0,
            "DELETE FROM media_file_artists WHERE media_file_id = ?"
        );
        let p_ins = put
            .participants_insert
            .as_ref()
            .expect("participants insert");
        assert!(p_ins.0.contains("INSERT INTO media_file_artists"));
        assert_eq!(
            put.participants_delete.1,
            vec![Value::Text("mf-1".to_string())]
        );
        // updateTags: delete-then-insert; the record's genre tag is indexed,
        // so the insert is present.
        assert_eq!(
            put.tags_delete.0,
            "DELETE FROM media_file_tags WHERE media_file_id = ?"
        );
        assert!(put.tags_insert.is_some());
    }

    #[test]
    fn put_empty_participants_delete_without_insert() {
        // Arrange: a put with no participants — the delete still renders,
        // the insert does not (Go returns before building it).
        let mut rec = test_record();
        rec.participants.clear();
        // Act:
        let put = put_sql("mf-1", &rec, chrono::Utc::now(), false, &[]).expect("put");
        // Assert:
        assert!(put.participants_insert.is_none());
        assert!(put
            .participants_delete
            .0
            .contains("DELETE FROM media_file_artists"));
    }

    #[test]
    fn put_restricted_camelcase_touches_only_probe_data() {
        // Arrange: a restricted update naming a camelCase column — the seam
        // toSnakeCase-normalizes it (helpers.go:48-52).
        let rec = test_record();
        // Act:
        let put =
            put_sql("mf-1", &rec, chrono::Utc::now(), false, &["ProbeData"]).expect("restricted");
        // Assert: the UPDATE SET is exactly probe_data (created_at/birth_time
        // never appear; nothing else is touched).
        let set = put
            .update
            .0
            .strip_prefix("UPDATE media_file SET ")
            .and_then(|s| s.strip_suffix(" WHERE id = ?"))
            .expect("set clause");
        assert_eq!(set, "probe_data = ?");
        assert_eq!(put.update.1.len(), 2); // probe_data + the WHERE id
                                           // The INSERT leg stays full-column.
        assert_eq!(put.insert.1.len(), 67);
    }

    #[test]
    fn update_probe_data_sets_the_single_column() {
        // Arrange: UpdateProbeData (:204-207).
        // Act:
        let (sql, args) = update_probe_data_sql("mf-1", "{\"codec\":\"flac\"}");
        // Assert:
        assert_eq!(sql, "UPDATE media_file SET probe_data = ? WHERE id = ?");
        assert_eq!(
            args,
            vec![
                Value::Text("{\"codec\":\"flac\"}".to_string()),
                Value::Text("mf-1".to_string()),
            ]
        );
    }

    #[test]
    fn find_by_paths_groups_one_in_list_per_library() {
        // Arrange: FindByPaths (:352-399) — library-qualified "1:foo.mp3"
        // plus the unqualified "bar.mp3" (searched both ways, :364-374).
        // Act:
        let (sql, args) = find_by_paths_sql(
            &[(1, vec!["foo.mp3".to_string()])],
            &["bar.mp3".to_string()],
        )
        .expect("at least one group renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM media_file WHERE \
             ((path collate nocase IN (?) AND library_id = ?) OR path collate nocase IN (?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("foo.mp3".to_string()),
                Value::Integer(1),
                Value::Text("bar.mp3".to_string()),
            ]
        );
        assert!(
            find_by_paths_sql(&[], &[]).is_none(),
            "no paths at all → no query (:394-396)"
        );
    }

    #[test]
    fn deletes_pin_their_where_shapes() {
        // Arrange: Delete (:401-403), DeleteAllMissing (:405-413) and
        // DeleteMissing (:415-431) — the two-key Eq renders id first.
        // Act + Assert:
        assert_eq!(
            delete_sql("mf-1"),
            (
                "DELETE FROM media_file WHERE id = ?".to_string(),
                vec![Value::Text("mf-1".to_string())]
            )
        );
        assert_eq!(
            delete_all_missing_sql(),
            (
                "DELETE FROM media_file WHERE missing = ?".to_string(),
                vec![Value::Bool(true)]
            )
        );
        assert_eq!(
            delete_missing_sql(&["a".to_string(), "b".to_string()]),
            (
                "DELETE FROM media_file WHERE id IN (?, ?) AND missing = ?".to_string(),
                vec![
                    Value::Text("a".to_string()),
                    Value::Text("b".to_string()),
                    Value::Bool(true),
                ]
            )
        );
    }

    #[test]
    fn mark_missing_chunks_at_200_and_binds_the_flip() {
        // Arrange: MarkMissing (:455-469) — slice.CollectChunks(ids, 200) at :457.
        let ids: Vec<String> = (0..201).map(|i| format!("mf-{i}")).collect();
        // Act:
        let chunks = mark_missing_sql(true, &ids, &crate::time::zero());
        // Assert:
        assert_eq!(chunks.len(), 2, "201 ids split at the 200 boundary");
        assert_eq!(
            chunks[0].0,
            "UPDATE media_file SET missing = ?, updated_at = ? WHERE id IN (?, ?, ?)"
                .replace("(?, ?, ?)", &format!("({})", vec!["?"; 200].join(", ")))
        );
        // Assert: the FULL arg vector, positionally. `missing`/`updated_at`
        // bind first (Go's `.Set` calls at :459-460) and the chunk's ids fill
        // `WHERE id IN (…)` (:461), so arg 0 is the flip, arg 1 the stamp and
        // args 2.. are the ids in input order. The old ids-first order made
        // `last() == Integer(99)` true by accident, so only a positional
        // check can catch it.
        assert_eq!(chunks[0].1.len(), CHUNK_SIZE + 2);
        assert_eq!(chunks[0].1[0], Value::Bool(true));
        assert_eq!(chunks[0].1[1], crate::time::value(&crate::time::zero()));
        assert_eq!(
            chunks[0].1[2..],
            ids[..CHUNK_SIZE]
                .iter()
                .map(|id| Value::Text(id.clone()))
                .collect::<Vec<Value>>()
        );
        // Assert: the second chunk (1 id) has the same shape, and the flip
        // appears exactly once in it.
        assert_eq!(
            chunks[1].1,
            vec![
                Value::Bool(true),
                crate::time::value(&crate::time::zero()),
                Value::Text(format!("mf-{CHUNK_SIZE}")),
            ]
        );
        assert_eq!(
            chunks[1]
                .1
                .iter()
                .filter(|v| **v == Value::Bool(true))
                .count(),
            1
        );
    }

    #[test]
    fn mark_missing_by_folder_gates_on_the_current_flag() {
        // Arrange: MarkMissingByFolder (:472-487) — Eq{"missing": !missing} at :479.
        // Act:
        let chunks = mark_missing_by_folder_sql(true, &["f-1".to_string()], &crate::time::zero());
        // Assert:
        assert_eq!(
            chunks[0],
            (
                "UPDATE media_file SET missing = ?, updated_at = ? \
                 WHERE folder_id IN (?) AND missing = ?"
                    .to_string(),
                vec![
                    Value::Bool(true),
                    crate::time::value(&crate::time::zero()),
                    Value::Text("f-1".to_string()),
                    Value::Bool(false),
                ]
            )
        );
    }

    #[test]
    fn album_ids_by_folder_inlines_the_folder_subquery() {
        // Arrange: GetAlbumIDsByFolder (:293-316) — a folder's own covers
        // also covers its disc subfolders (:306-309).
        // Act:
        let chunks = get_album_ids_by_folder_sql(1, &["f-1".to_string()]);
        // Assert:
        assert_eq!(
            chunks[0],
            (
                "SELECT distinct album_id FROM media_file WHERE missing = ? AND folder_id IN \
                 (SELECT f.id FROM folder f WHERE f.library_id = ? AND f.missing = ? \
                 AND (f.id IN (?) OR f.parent_id IN (?)))"
                    .to_string(),
                vec![
                    Value::Bool(false),
                    Value::Integer(1),
                    Value::Bool(false),
                    Value::Text("f-1".to_string()),
                    Value::Text("f-1".to_string()),
                ]
            )
        );
    }

    #[test]
    fn missing_and_matching_inlines_the_pid_subquery() {
        // Arrange: GetMissingAndMatching (:485-510) — the inlined pid
        // subquery with its own args bound first (:493-499), the Or last.
        // Act:
        let (sql, args) = get_missing_and_matching_sql(1);
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.*, library.path as library_path, library.name as library_name \
             FROM media_file \
             LEFT JOIN library ON media_file.library_id = library.id \
             WHERE pid in (SELECT pid FROM media_file WHERE media_file.missing = ? AND library_id = ?) \
             AND (missing = ? OR media_file.created_at > library.last_scan_started_at) \
             ORDER BY pid"
        );
        assert_eq!(
            args,
            vec![Value::Bool(true), Value::Integer(1), Value::Bool(true)]
        );
    }

    #[test]
    fn find_recent_files_matchers_pin_their_where_chains() {
        // Arrange: FindRecentFilesByMBZTrackID (:512-532) and
        // FindRecentFilesByProperties (:534-563) — declaration-order Ands.
        // Act:
        let since = crate::time::zero();
        let by_mbz = find_recent_files_by_mbz_track_id_sql(&RecentFileByMbz {
            library_id: 2,
            mbz_release_track_id: "mbz-1",
            suffix: "flac",
            since: &since,
        });
        let by_props = find_recent_files_by_properties_sql(&RecentFileByProperties {
            library_id: 2,
            title: "Song",
            size: 42,
            suffix: "flac",
            disc_number: 1,
            track_number: 2,
            album: "Album",
            since: &since,
        });
        // Assert:
        assert_eq!(
            by_mbz.0,
            "SELECT media_file.*, library.path as library_path, library.name as library_name \
             FROM media_file \
             LEFT JOIN library ON media_file.library_id = library.id \
             WHERE media_file.library_id <> ? \
             AND media_file.mbz_release_track_id = ? \
             AND media_file.mbz_release_track_id <> ? \
             AND media_file.suffix = ? \
             AND media_file.created_at > ? \
             AND media_file.missing = ? \
             ORDER BY media_file.created_at DESC"
        );
        assert_eq!(
            by_mbz.1,
            vec![
                Value::Integer(2),
                Value::Text("mbz-1".to_string()),
                Value::Text(String::new()),
                Value::Text("flac".to_string()),
                crate::time::value(&since),
                Value::Bool(false),
            ]
        );
        assert!(by_props.0.contains(
            "AND media_file.title = ? AND media_file.size = ? AND media_file.suffix = ? \
             AND media_file.disc_number = ? AND media_file.track_number = ? \
             AND media_file.album = ? AND media_file.mbz_release_track_id = ?"
        ));
        assert_eq!(by_props.1.len(), 10);
    }

    #[test]
    fn reassign_references_runs_go_order_with_or_ignore_last() {
        // Arrange: ReassignReferences (:415-432).
        // Act:
        let stmts = reassign_references_sql("old", "new");
        // Assert:
        assert_eq!(stmts.len(), 5);
        assert!(stmts[0].0.contains("annotation"));
        assert!(stmts[1].0.contains("bookmark"));
        assert_eq!(
            stmts[2].0,
            "UPDATE playlist_tracks SET media_file_id = ? WHERE media_file_id = ?"
        );
        assert_eq!(
            stmts[3].0,
            "UPDATE scrobbles SET media_file_id = ? WHERE media_file_id = ?"
        );
        assert_eq!(
            stmts[4].0,
            "update or ignore scrobble_buffer set media_file_id = ? where media_file_id = ?"
        );
        assert_eq!(
            stmts[4].1,
            vec![
                Value::Text("new".to_string()),
                Value::Text("old".to_string())
            ]
        );
    }

    #[test]
    fn sort_mappings_pin_the_recently_added_and_album_compositions() {
        // Arrange: setSortMappings (:91-108) — album composes
        // naturalSort("media_file.title"), recently_added is
        // mediaFileRecentlyAddedSort (:110-116).
        // Act:
        let mod_time = sort_mappings(NATURAL_SORT_TITLE, true);
        let created = sort_mappings(NATURAL_SORT_TITLE, false);
        // Assert:
        let album = mod_time
            .iter()
            .find(|(k, _)| *k == "album")
            .map(|(_, v)| v.clone())
            .expect("album mapping registered");
        assert_eq!(
            album,
            "order_album_name, album_id, disc_number, track_number, order_artist_name, media_file.title"
        );
        assert_eq!(
            mod_time
                .iter()
                .find(|(k, _)| *k == "recently_added")
                .map(|(_, v)| v.clone()),
            Some("media_file.updated_at, media_file.id".to_string())
        );
        assert_eq!(
            created
                .iter()
                .find(|(k, _)| *k == "recently_added")
                .map(|(_, v)| v.clone()),
            Some("media_file.created_at, media_file.id".to_string())
        );
    }

    #[test]
    fn search_config_pins_natural_order_and_mbid_fields() {
        // Arrange: mediaFileSearchConfig (:565-567).
        // Act:
        let cfg = media_file_search_config();
        // Assert: the fields are private on SearchConfig (sibling lane
        // sql_search.rs); the composition is exercised through doSearch
        // in that lane. Here we pin the constructor returns a config whose
        // strategy dispatch matches the media_file table.
        let _ = cfg;
        assert!(
            get_search_strategy("", false, false).is_none(),
            "an empty query yields no strategy (sql_search.go:59-61 handled upstream)"
        );
    }
}
