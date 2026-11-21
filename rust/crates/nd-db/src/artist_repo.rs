//! Port of `persistence/artist_repository.go` (753L) — the artist table with
//! its per-library stats aggregation, index groups and search scoping.
//!
//! Scope: the stats select shape ([`select_artist_parts`], `:207-214` via
//! `applyLibraryFilterToArtistQuery`, `:191-203`), `Get`/`GetAll`
//! ([`get_sql`]/[`get_all_sql`]), `CountAll` (`:217-226`, the annotation
//! join only when the filters reference annotation columns), the
//! `count(distinct …)` `Exists` (`:229-236`), the `Put`/`UpdateExternalInfo`
//! upserts (`:239-253` via `put`, `sql_base_repository.go:618-642`, with
//! `dbArtist.PostMapArgs`' derived columns and the empty
//! `sort_artist_name`/`mbz_artist_id` drops, `:100-121`), `getAllIDs`
//! (`:283-291`), `GetCursor`'s id pre-pass + chunking (`:293-308`),
//! `GetIndex` (`:327-368`, including `getIndexKey`, `:310-322`), the
//! maintenance statements (`purgeEmpty` `:370-405`, `markOrphansMissing`
//! `:410-418`, `markMissing` `:421-437`, `RefreshPlayCounts` `:441-460`,
//! `RefreshStats`' id selects + batched `library_artist` update + cleanup,
//! `:465-614`), the search config + scoping (`searchCfg` `:618-633`,
//! `artistLibraryFilter` `:640-650`, `Search` `:652-671`, `searchScope`
//! `:676-696`, `requestedLibraryIDs` `:700-707` / `isLibraryIDFilter` `:711-718`), and
//! the rest-facing `registerModel` (`:138-146`) / `setSortMappings` (`:147-159`) registry.
//!
//! Rendering deviation, named: `selectArtist`'s stats column
//! (`JSON_GROUP_OBJECT(...)`) and `GROUP BY artist.id` have no
//! [`crate::sqlrepo::Repository`] spelling (that builder renders neither),
//! so the artist selects are hand-rendered here following squirrel's clause
//! ORDER for the clauses that are rendered (JOIN, WHERE, GROUP BY,
//! LIMIT/OFFSET) with every filter value bound and every identifier fixed;
//! the bounds themselves are squirrel literals (`select.go:139-147`) and
//! contribute no args.
//!
//! The ORDER BY leg is NOT among them. Go's `applyOptions` renders
//! `OrderBy(buildSortOrder(sort, order))` for a non-empty sort
//! (`sql_base_repository.go:157-158`) inside `newSelect` (`:139-146`), which
//! `GetAll` and `Search` reach through `selectArtist(ctx, options...)`
//! (`artist_repository.go:270`, `:664`); `getAllIDs` runs the same `applyOptions`
//! via `newSelect(ctx, options...)` directly (`:284`).
//! `pagination_fragment` emits only LIMIT/OFFSET, so `QueryOptions::sort` and
//! [`QueryOptions::order`] are IGNORED by every artist select here: row order is
//! SQLite's, not the caller's. [`sort_mapping`] holds the mappings but nothing in
//! this crate's artist selects consumes them (`player_repo`'s `build_sort_order`
//! is player-specific). Wiring the leg is a separate lane, not a one-line change:
//! [`search_sql`] reuses the base select and pushes its OWN `WHERE`/`ORDER BY`
//! after the `GROUP BY` tail, so giving the tail an ORDER BY requires reworking
//! the clause-ordered tail together with the search path. Pinned by
//! `tests/runtime_hydrate_artist.rs::get_all_sql_ignores_the_sort_leg_until_options_are_wired`.
//!
//! The search path composes the shared
//! exported phases ([`crate::sql_search::rowid_page_phase`]'s filter stack)
//! instead of [`crate::sql_search::do_search`], for the same reason — the
//! base select cannot ride through a `Repository` without losing the
//! aggregation; the dispatch order (empty → natural-order rowid page,
//! UUID → MBID, `<2` bytes → none, else strategy) is Go's `doSearch`
//! (`sql_search.go:35-62`) verbatim.
//!
//! Roles: Go's `model.Role` string set (`model/participants.go:28-48`) is
//! pinned as [`ALL_ROLES`]; `sanitizeArtistStatsRole` (`:175-183`) allowlists
//! them (plus `"total"`, the scanner's aggregate key) before any role
//! reaches SQL concatenation — `roleFilter` (`:163-170`) and the role sort
//! mappings (`:147-159`, `sortMappingsForRole` `:730-744`) build JSON paths
//! from the sanitized value only. The always-false `Eq{"1": 2}` fallback
//! of both is [`FALSE_EXPR`] — no bound args.
//!
//! Ownership: `applyLibraryFilterToArtistQuery`'s `user_library` join is
//! gated on a logged-in non-admin user (`:197-203`); the crate has no
//! ambient context (the `sqlrepo.rs` DI rule), so the selects take the
//! user as [`User`] — `None` is admin/headless (no join). Timestamps are
//! are the chrono types of [`crate::time`] (TEXT in rusqlite's
//! `DateTime<Utc>` shape; see `time.rs` for why epoch integers silently break
//! range predicates and ordering), with a caller-owned clock.
//!
//! Not ported, with the reason: `visibleLibraryIDs`/`userSeesAllLibraries`
//! (`sql_base_repository.go:286-320`) need the user row; [`search_scope`]
//! takes the resolved visible set and the caller owns the fail-safe.
//! `CountAll`/`getAllIDs`/`GetCursor`/`GetIndex` stay SQL-only builders
//! (`count_all_sql` `:217-226`, `get_all_ids_sql` `:283-291`,
//! `chunk_options` `:676-700`, `get_index_key` `:714-733`): `GetCursor`
//! streams id chunks through `streamByIDs` + `chunkOptions`
//! (`artwork_hydration.go:21-37`, `:41-54`), both unported (streaming lane),
//! and `GetIndex` groups hydrated records in Go — it hydrates transitively
//! because `GetIndex` (`:327-368`) delegates to `GetAll` (`:269-279`) via its `:355` call site, so it inherits this slice's hydration
//! once its statement lands. `getParticipants` (`sql_participations.go:127-146`)
//! has no artist-side caller in Go — `GetWithParticipants` exists only on the
//! media file repository (`mediafile_repository.go:223-230`) — so neither is
//! ported here.
//!
//! Landed here (S-D3d): the `dbArtist.PostScan` stats/similar-artists fold
//! (`hydrate::row_to_artist` → `hydrate::artist_post_scan`, `:43-98`), the
//! executed read record ([`ArtistRow::into_record`]), the repo read side
//! ([`ArtistRepo`] — `Get` `:255-267`, `GetAll` `:269-279`) and artwork
//! hydration ([`crate::artwork_hydration::hydrate_artist_artwork`],
//! `hydrateArtwork` `:294-297`).

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::artwork_hydration::{hydrate_artist_artwork, ItemImage, ReadOutcome};
use crate::runtime_error::DbOperationError;
use crate::sql_participations::Role;
use crate::sql_restful::{Filter, QueryOptions, RestRepository};
use crate::sql_search::{mbid_expr, rowid_page_phase, SearchConfig, Sql};
use crate::sql_search_fts as fts;
use crate::sql_search_like as like;
use crate::sqlrepo::{Repository, SqlError, Value};
use crate::time;
use nd_model::annotation::Annotation;
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
/// (`sanitizeArtistStatsRole` `:175-183`, and the `"total"` pass-through at `:176-178`).
pub const TOTAL_ROLE: &str = "total";

/// `Eq{"1": 2}` (`:76`, `:706`) — the always-false fallback, arg-free.
pub const FALSE_EXPR: &str = "1 = 2";

/// `invalidUserId` (`sql_base_repository.go:51`).
pub const INVALID_USER_ID: &str = "-1";

/// The effective user for the library join: `None` is admin/headless
/// (Go's `user.ID != invalidUserId && !user.IsAdmin` skip, `:199-203`),
/// `Some(id)` a logged-in non-admin.
pub type User = Option<String>;

/// `artworkChunkSize` (`artwork_hydration.go:15-17`, `artworkBatchSize`*3 = 600)
/// of `GetCursor`'s id pre-pass (`streamByIDs`, `:19-34`).
///
/// DB-11 corrected this constant from 300 to 600 per `D-CURSOR(4)`
/// (`docs/rust-port/decisions.md`: cursor id chunk 600, artwork info batch 200):
/// the previous 300 mis-mirrored Go's 600, and nothing in the crate consumed the
/// constant before this lane, so no existing behavior changed.
pub const CURSOR_CHUNK_SIZE: usize = 600;

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
    /// `external_info_updated_at` — Go's `*time.Time` (`artist.go:39`);
    /// `None` binds NULL ("no external update recorded").
    pub external_info_updated_at: Option<DateTime<Utc>>,
    /// `missing`.
    pub missing: bool,
    /// `uploaded_image`.
    pub uploaded_image: String,
    /// `created_at` — Go's `*time.Time` (`artist.go:45`), which `Put` always
    /// stamps (`artist_repository.go:240`), so `None` only reaches this layer
    /// on a row the caller never stamped.
    pub created_at: Option<DateTime<Utc>>,
    /// `updated_at` — Go's `*time.Time` (`artist.go:46`), stamped by `Put`.
    pub updated_at: Option<DateTime<Utc>>,

    // ---- hydration-only state (S-D3d). Every field below is `structs:"-"` in
    // Go, so NONE of it appears in `put_cols`/`put_sql`: `Stats`/`Size`/
    // `AlbumCount`/`SongCount` are the `structs:"-"` computed block
    // (`model/artist.go:27-30`), `annotation` is the `structs:"-"` `Annotations`
    // embed (`:15`, `model/annotation.go:8-16`) and `image` is the `structs:"-"`
    // `ItemImage` embed (`:16`, `model/artwork.go:29-40`).
    /// `Stats` (`model/artist.go:27`): the per-role fold `PostScan` builds from
    /// the `library_stats_json` alias (`artist_repository.go:44-80`). Empty
    /// when the alias is `""` — Go creates the map unconditionally at `:44`.
    pub stats: BTreeMap<Role, ArtistStats>,
    /// `Size` (`:28`): Go `int64`. Accumulated ONLY by the `"total"` arm
    /// (`:63-67`), never by a per-role entry.
    pub size: i64,
    /// `AlbumCount` (`:29`): Go `int` → `i32` (the port-wide narrowing).
    pub album_count: i32,
    /// `SongCount` (`:30`): Go `int` → `i32` (the port-wide narrowing).
    pub song_count: i32,
    /// The promoted `Annotations` embed (`model/artist.go:15`): read by
    /// [`crate::hydrate::row_to_annotation`] from the annotation join's columns,
    /// or from `artist.*` alone when the join is skipped.
    pub annotation: Annotation,
    /// The promoted `ItemImage` embed (`model/artist.go:16`): filled by the
    /// SECOND layer ([`hydrate_artist_artwork`]), zero (= unresolved) from
    /// phase 1/2.
    pub image: ItemImage,
    /// `SimilarArtists` (`model/artist.go:38`) DECODED, id+name only — Go's
    /// `dbSimilarArtist` (`:38-41`) keeps exactly those two fields. The raw
    /// column text stays in [`ArtistRecord::similar_artists`], which is what the
    /// write map emits; see that field's note on the round-trip direction.
    pub similar: Vec<SimilarArtist>,
}

/// `model.ArtistStats` (`model/artist.go:49-53`): one role's aggregate.
/// `SongCount`/`AlbumCount` are Go `int` → `i32` (port-wide narrowing);
/// `Size` is Go `int64`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistStats {
    /// `SongCount` (`:50`), from the stats key `m` (`artist_repository.go:57`).
    pub song_count: i32,
    /// `AlbumCount` (`:51`), from the stats key `a` (`:58`).
    pub album_count: i32,
    /// `Size` (`:52`), from the stats key `s` (`:59`).
    pub size: i64,
}

/// `dbArtist`'s `dbSimilarArtist` (`artist_repository.go:38-41`): the stored
/// `{id, name}` pair. `omitempty` on both keys means `marshal_similar_artists`
/// skips an empty one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimilarArtist {
    /// `ID` (`:39`).
    pub id: String,
    /// `Name` (`:40`).
    pub name: String,
}

/// The raw row of an artist read — phase 1's output, before
/// [`crate::hydrate::artist_post_scan`].
///
/// `dbArtist` (`artist_repository.go:32-36`) is `*model.Artist` flattened plus
/// two `structs:"-"` shim strings. Only ONE shim needs a slot here:
/// `LibraryStatsJSON` (`:35`), the `JSON_GROUP_OBJECT` alias emitted by
/// [`select_artist_sql`] via `LIBRARY_STATS_COLUMN` (kept private: it is the
/// same text in every artist select) with no destination field on
/// `model.Artist`.
/// `SimilarArtists` (`:34`) is the `similar_artists` COLUMN, so its text
/// already lands on [`ArtistRecord::similar_artists`] — where the write map
/// needs it — and phase 2 decodes it into [`ArtistRecord::similar`] rather than
/// keeping a second copy.
///
/// The `artist.*` expansion also carries `full_text` and `search_normalized`,
/// which have no destination field on `dbArtist` either; dbx binds them to a
/// throwaway `sql.NullString` and drops them (`dbx@v1.12.0/rows.go:80-86`), so
/// [`crate::hydrate::row_to_artist`] does not read them and must not fail when
/// they are present.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRow {
    /// The artist columns scanned from the row, with the stats/similar lists
    /// still empty and `image` still zero.
    pub record: ArtistRecord,
    /// The raw `library_stats_json` alias, unparsed (Go `dbArtist.LibraryStatsJSON`,
    /// `:35`). Guarded by `!= ""` at `:46`.
    pub library_stats_json: String,
}

impl ArtistRow {
    /// `dbArtist.PostScan` (`artist_repository.go:43-98`): thin delegation onto
    /// [`crate::hydrate::artist_post_scan`], surfaced the way Go's `ScanStruct`
    /// error leaves the read (`dbx@v1.12.0/rows.go:92-102` returns the hook's
    /// error untouched, and the read funnel surfaces it as a column-conversion
    /// failure — [`crate::hydrate::HydrateError::into_sqlite`]).
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] carrying one of Go's EXACT
    /// artist prefixes ([`crate::hydrate::ARTIST_STATS_PREFIX`],
    /// [`crate::hydrate::ARTIST_SIMILAR_PREFIX`]).
    pub fn into_record(self) -> Result<ArtistRecord, crate::hydrate::HydrateError> {
        crate::hydrate::artist_post_scan(self)
    }
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

/// The `LIMIT n [OFFSET n]` tail `applyOptions` appends (`:149-159`) —
/// ONLY the bounds. The same function's `OrderBy` leg (`:157-158`) is not
/// rendered here, so a caller's `sort`/`order` has no effect on the row
/// order these statements return (see the module doc and
/// `tests/runtime_hydrate_artist.rs::get_all_sql_ignores_the_sort_leg_until_options_are_wired`).
/// Squirrel renders both bounds as decimal literals and contributes no
/// arguments (`select.go:139-147`), so this returns SQL only.
#[must_use]
fn pagination_fragment(options: Option<&QueryOptions>) -> String {
    let Some(o) = options else {
        return String::new();
    };
    let mut sql = String::new();
    if o.max > 0 {
        sql.push_str(&format!(" LIMIT {}", o.max));
    }
    if o.offset > 0 {
        // Deviation (deliberate, cited): squirrel emits a bare `OFFSET n`
        // (`applyOptions` sets the bounds independently, :151-156), which
        // SQLite rejects as a parse error; the port adds `LIMIT -1` (SQLite's
        // unlimited bound) so an offset-only page executes.
        if o.max <= 0 {
            sql.push_str(" LIMIT -1");
        }
        sql.push_str(&format!(" OFFSET {}", o.offset));
    }
    sql
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
    sql.push_str(&pagination_fragment(options));
    (sql, args)
}

/// `Get`/`Read` (`:255-267,724-726`): the stats select narrowed to one id.
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

/// `GetAll`/`ReadAll` (`:269-279,746-750`): the stats select with the caller's
/// filters and pagination. `annotation` is the `withAnnotation` fragment
/// (see [`select_artist_sql`]); `None` matches Go's `invalidUserId` skip.
/// [`ArtistRepo::get_all`] passes the caller's annotation user here, so
/// `selectArtist`'s columns are the same as Go's for a logged user.
///
/// `options` reaches ONLY the filter and the LIMIT/OFFSET legs: Go's
/// `applyOptions` also renders an ORDER BY (`sql_base_repository.go:157-158`),
/// which this builder does not, so `sort`/`order` are silently ignored.
#[must_use]
pub fn get_all_sql(
    user: &User,
    options: Option<&QueryOptions>,
    annotation: Option<&(String, Vec<String>, Vec<Value>)>,
) -> Sql {
    select_artist_sql(user, options, annotation)
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

/// `filtersNeedAnnotation` (`sql_annotations.go:55-61`): whether any filter
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

/// `CountAll` (`:217-226`): `count(distinct artist.id) as count` over the
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

/// `sanitizeArtistStatsRole` (`:175-183`): allowlist a role for JSON-path
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

/// `roleFilter` (`:163-170`): `JSON_EXTRACT(library_artist.stats,
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

/// `artistLibraryIdFilter` (`:186-188`): `library_artist.library_id = ?`.
#[must_use]
pub fn artist_library_id_filter(library_id: Value) -> Sql {
    (
        format!("{LIBRARY_ARTIST_TABLE}.library_id = ?"),
        vec![library_id],
    )
}

/// `artistLibraryFilter` (`:640-650`): the join-free EXISTS scope. The
/// inner `LIMIT 1` is load-bearing (`:644-648`): it stops SQLite from
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

/// The columns the typed write boundary [`put_artist_in_tx`] can select: every
/// `artist` column [`put_cols`] emits EXCEPT `created_at` and `id`.
///
/// `created_at` is not a member because Go's `selectUpdateColumns` deletes it from
/// the UPDATE set (`sql_base_repository.go:601`) — immutability of the create stamp
/// is STRUCTURAL here, exactly as in `LibraryColumn` (`library_repo.rs:585-598`), and
/// `id` is the UPDATE's `WHERE` key rather than a SET target. `updated_at`, `full_text`
/// and `search_normalized` ARE members: Go's scanner lists all three explicitly
/// (`scanner/phase_1_folders.go:403-405`), and `dbArtist.PostMapArgs` warns that a
/// derived column omitted from that list goes stale forever (`artist_repository.go:
/// 108-109`).
///
/// This enum replaces Go's free-form `colsToUpdate ...string` for the executed
/// boundary, so a caller cannot name a column outside the map and Go's `toSnakeCase`
/// normalization of caller strings (`sql_base_repository.go:596-598`) has nothing to
/// normalize. An empty slice is Go's empty variadic list — `len(c2upd) == 0`, `:599` —
/// i.e. every column [`put_cols`] yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistColumn {
    /// `artist.name`.
    Name,
    /// `artist.sort_artist_name`.
    SortArtistName,
    /// `artist.order_artist_name`.
    OrderArtistName,
    /// `artist.mbz_artist_id`.
    MbzArtistId,
    /// `artist.biography`.
    Biography,
    /// `artist.small_image_url`.
    SmallImageUrl,
    /// `artist.medium_image_url`.
    MediumImageUrl,
    /// `artist.large_image_url`.
    LargeImageUrl,
    /// `artist.external_url`.
    ExternalUrl,
    /// `artist.similar_artists` (the marshalled `[{id, name}]` JSON).
    SimilarArtists,
    /// `artist.external_info_updated_at` — the external provider's own throttle stamp
    /// (`core/external/provider.go:235,286`), NOT `updated_at`.
    ExternalInfoUpdatedAt,
    /// `artist.missing`.
    Missing,
    /// `artist.uploaded_image`.
    UploadedImage,
    /// `artist.full_text` — derived by `PostMapArgs` (`:109`).
    FullText,
    /// `artist.search_normalized` — derived by `PostMapArgs` (`:110`).
    SearchNormalized,
    /// `artist.updated_at`, written ONLY when selected (see [`put_sql`]).
    UpdatedAt,
}

impl ArtistColumn {
    /// The registered snake column name, i.e. a key of the map [`put_cols`] builds.
    /// Public deliberately: it is the column identity of a variant, so a caller (or a
    /// diagnostic) can name the column an `ArtistColumn` selects without going through
    /// the private `column_names` rendering of a whole slice.
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::SortArtistName => "sort_artist_name",
            Self::OrderArtistName => "order_artist_name",
            Self::MbzArtistId => "mbz_artist_id",
            Self::Biography => "biography",
            Self::SmallImageUrl => "small_image_url",
            Self::MediumImageUrl => "medium_image_url",
            Self::LargeImageUrl => "large_image_url",
            Self::ExternalUrl => "external_url",
            Self::SimilarArtists => "similar_artists",
            Self::ExternalInfoUpdatedAt => "external_info_updated_at",
            Self::Missing => "missing",
            Self::UploadedImage => "uploaded_image",
            Self::FullText => "full_text",
            Self::SearchNormalized => "search_normalized",
            Self::UpdatedAt => "updated_at",
        }
    }
}

/// The typed column list of [`put_artist_in_tx`]: `&[ArtistColumn]` is rendered as the
/// snake names [`put_sql`] filters on. An empty slice selects all columns.
fn column_names(columns: &[ArtistColumn]) -> Vec<&'static str> {
    columns.iter().map(|column| column.column()).collect()
}
/// `dbSimilarArtist` marshalling (`:38-41`, `PostMapArgs` `:101-106`):
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

/// `PostMapArgs` (`:100-121`) as the SET pair list of the upsert — the
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
        time::optional_col(
            "external_info_updated_at",
            artist.external_info_updated_at.as_ref(),
        ),
        ("missing".to_string(), Value::Bool(artist.missing)),
        (
            "uploaded_image".to_string(),
            Value::Text(artist.uploaded_image.clone()),
        ),
        time::optional_col("created_at", artist.created_at.as_ref()),
        time::optional_col("updated_at", artist.updated_at.as_ref()),
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
/// (`sql_base_repository.go:618-642`): the UPDATE-first/INSERT-fallback
/// upsert. `selectUpdateColumns` (`sql_base_repository.go:593-610`) filters the SET
/// list to the requested columns (all when none), minus `created_at`/`birth_time`.
///
/// `updated_at` is written ONLY when explicitly selected — Go parity, restored here.
/// `Put` stamps `CreatedAt = UpdatedAt = time.Now()` on the MODEL (`:240-241`), which is
/// why an UNFILTERED save (`cols_to_update` empty, `len(c2upd) == 0`, `:599`) re-stamps
/// `updated_at`: the full-column SET carries it. A FILTERED save writes exactly the
/// requested columns, so `UpdateExternalInfo` (`:247-253`), whose 7-column list never names
/// `updated_at`, leaves the stored stamp untouched. The scanner's call DOES name it
/// (`scanner/phase_1_folders.go:403-405`). Consequence, recorded for `decisions.md`:
/// `artist.updated_at` is the cutoff of `RefreshStats(allArtists=false)`
/// (`artist_repository.go:479`), so under Go parity an external-info refresh no longer
/// re-enters the touched-artist stats set — under the previous forced append it did, and it
/// silently overwrote the scanner's stamp.
///
/// `created_at` stays immutable on the UPDATE leg (`:601`); the INSERT leg keeps it, stamped
/// to `now` when the record carries none. `now` is the CALLER's clock, never the commit clock.
///
/// Empty SET, resolved honestly: Go's `filterUpdateValues` (`:612-616`) ALWAYS re-adds `id`
/// to the SET map, so a selection that resolves to nothing settable — `&["created_at"]`, or a
/// `sort_artist_name`/`mbz_artist_id` selection whose record value is empty and dropped by
/// `PostMapArgs` (`artist_repository.go:113-120`) — renders `UPDATE artist SET id = ? WHERE
/// id = ?` in Go. Executed (sqlite3 3.54.0): 1 row affected for an existing id, 0 for a
/// missing one — a real no-op UPDATE carrying NO error. This renderer emits exactly that
/// minimal no-op statement rather than the degenerate `SET  WHERE` (a parse error) and rather
/// than `crate::marshal::MarshalError::EmptyUpdateSet`, which belongs to the shared map seam
/// and would be a recorded divergence here. An UNFILTERED selection already carries `id` from
/// [`put_cols`], so it is not duplicated. [`put_artist_in_tx`] therefore reports
/// `inserted: false` on an existing row and a full INSERT on a miss with no existence SELECT
/// shortcut — trigger firing and `RowsAffected` parity come from the statement itself, which
/// a pre-flight read would not reproduce.
///
/// Where the SET TEXT differs from Go's, deliberately and unchanged from HEAD: Go's SET map
/// carries the no-op `id = ?` on EVERY filtered update (`:612-616`), and this renderer binds
/// `id` only in `WHERE id = ?` — the same omission `crate::marshal` documents for the shared
/// seam (`marshal.rs:204-211`). It is a no-op assignment, so the stored values and the
/// affected-row count are identical; only the column list in the statement text differs.
///
/// `cols_to_update` is the legacy free-form seam: entries are normalized with
/// [`crate::marshal::to_snake_case`] exactly as Go's `selectUpdateColumns` does (`:596-598`),
/// so `"UpdatedAt"` behaves like Go's camelCase entry. An entry naming no column of the map
/// selects nothing, i.e. resolves to the no-op statement above. No caller text reaches SQL:
/// SET identifiers come from [`put_cols`] only and every value is bound. The typed write
/// boundary [`put_artist_in_tx`] takes [`ArtistColumn`] instead and has nothing to normalize.
///
/// The UPDATE's row count decides which statement ran — the caller's ([`put_artist_in_tx`]).
#[must_use]
pub fn put_sql(artist: &ArtistRecord, now: DateTime<Utc>, cols_to_update: &[&str]) -> (Sql, Sql) {
    let stamped = ArtistRecord {
        created_at: Some(artist.created_at.unwrap_or(now)),
        updated_at: Some(now),
        ..artist.clone()
    };
    let all_cols = put_cols(&stamped);
    // Go's `selectUpdateColumns` normalizes each caller entry with `toSnakeCase`
    // (`sql_base_repository.go:596-598`) before matching snake column keys.
    let wanted: Vec<String> = cols_to_update
        .iter()
        .map(|col| crate::marshal::to_snake_case(col))
        .collect();
    let mut update_cols: Vec<&(String, Value)> = all_cols
        .iter()
        .filter(|(col, _)| wanted.is_empty() || wanted.contains(col))
        .filter(|(col, _)| col != "created_at")
        .collect();
    if !wanted.is_empty() && update_cols.is_empty() {
        // Go's `filterUpdateValues` re-adds `id` (`sql_base_repository.go:612-616`):
        // the minimal no-op `SET id = ?`, not an empty SET and not an error.
        update_cols = vec![all_cols
            .iter()
            .find(|(col, _)| col == "id")
            .expect("put_cols always emits the id column")];
    }
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
    // squirrel's Insert().SetMap renders a column list plus one VALUES tuple
    // (`insert.go:92-105,269-287`), not MySQL's `SET col = ?` — SQLite rejects
    // the latter, so the INSERT leg has to be the VALUES form to execute. Go
    // sorts the column keys (`sort.Strings`, `insert.go:276`); the order is
    // irrelevant here because the column list and the values stay aligned.
    let insert_cols = all_cols
        .iter()
        .map(|(col, _)| col.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let insert_placeholders = all_cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let insert_args: Vec<Value> = all_cols.iter().map(|(_, v)| v.clone()).collect();
    let insert = (
        format!("INSERT INTO {TABLE} ({insert_cols}) VALUES ({insert_placeholders})"),
        insert_args,
    );
    (update, insert)
}

/// What a successful [`put_artist_in_tx`] reports.
///
/// Both fields are ADVISORY until the caller commits: a rolled-back Put publishes nothing,
/// and the artist repository keeps no cache either (Go keeps none), so there is nothing to
/// invalidate on rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistWriteResult {
    /// The written row's id: [`ArtistRecord::id`], which is always non-empty here.
    /// Go's `put` returns `(newId, err)` (`sql_base_repository.go:618-642`) and
    /// `artistRepository.Put` DISCARDS it (`artist_repository.go:239-245`); the port reports
    /// it because DB-09/10 need it to order junction writes.
    pub id: String,
    /// `true` when the INSERT leg ran rather than the UPDATE leg, i.e. when the UPDATE
    /// matched zero rows (`sql_base_repository.go:630-641`).
    pub inserted: bool,
}
/// Callback error of the executed artist [`put_artist_in_tx`].
///
/// There is NO builder arm, and inventing one would be wrong: [`put_sql`] returns
/// `(`[`Sql`]`, `[`Sql`]`)` — it is infallible today — and a selection that resolves to
/// nothing settable renders the Go-parity no-op `SET id = ? WHERE id = ?` (see [`put_sql`])
/// instead of failing. A `crate::marshal::MarshalError` arm would claim a failure the artist
/// renderer cannot produce; `D-ERROR` keeps a typed arm for causes that actually exist.
#[derive(Debug, thiserror::Error)]
pub enum ArtistWriteError {
    /// [`ArtistRecord::id`] is empty, so there is nothing to write. **DOCUMENTED DEVIATION,
    /// not exact Go parity:** Go's `put` fills an empty id with `id2.NewRandom()`
    /// (`sql_base_repository.go:635-638`) and SKIPS the UPDATE leg entirely, returning the
    /// generated id with no error. This port refuses instead, because artist identity comes
    /// from SC (`nd_id::new_hash`/`new_hash1`/`new_random`, `nd-id/src/lib.rs:115-133`) and a
    /// DB-side id generator would be an invented policy nobody asked for. Accepted under the
    /// supplied-SC-ID contract (parent decision); Go has no equivalent error here, so do NOT
    /// describe this arm as Go-exact.
    #[error("artist record has an empty id: the id comes from SC and is never generated here")]
    EmptyId,
}
/// `Put` (`artist_repository.go:239-245`) EXECUTED on the CALLER's transaction: Go's
/// `r.put(ctx, dba.ID, dba, colsToUpdate...)` upsert (`sql_base_repository.go:618-642`) in
/// Go's own statement order — UPDATE by `id` FIRST, and the INSERT only when that matched
/// zero rows. A zero-row UPDATE is the insert fallback (`:630-641`, the new record with a
/// predefined id), NOT a miss and never a `NotFound`.
///
/// Identity: the id is the caller's — `record.id` is the UPDATE's `WHERE id = ?` and the
/// INSERT's id column. This function never recomputes it and invents no random-id arm or
/// retry: Go fills an empty id with `id2.NewRandom()` (`:635-638`), but an artist id always
/// arrives from SC, so an empty `record.id` is refused as a typed contract violation BEFORE
/// any statement runs. That refusal is a **port-side deviation from Go** (Go generates an id
/// and succeeds), not a Go error; see [`ArtistWriteError::EmptyId`].
///
/// Timestamps: `now` is the caller's clock and reaches [`put_sql`] as its stamp — never the
/// commit clock and never re-read inside this function. `updated_at` is written only when
/// [`ArtistColumn::UpdatedAt`] is selected (Go parity, see [`put_sql`]); `created_at` is
/// immutable on the UPDATE leg and keeps the record's own value on the INSERT leg, falling
/// back to `now` only when the record carries none (`artist_repository.go:240-241`). The
/// record is borrowed and never mutated: stamps travel as bound values.
///
/// Columns: `cols` is the typed [`ArtistColumn`] set — an empty slice is Go's empty variadic
/// list (all columns), and a selection that resolves to nothing settable runs the Go-parity
/// no-op UPDATE. There is no arbitrary-SQL column input on this boundary.
///
/// Scope: a caller-trusted primitive. `WHERE id = ?` and nothing else, exactly like Go: no
/// library predicate, no auth check, no ownership guard. The `library_artist` link is NOT
/// written here — it stays DB-05's [`crate::library_repo::add_artist_in_tx`], which the caller
/// runs as a separate statement in the SAME transaction (Go's scanner calls `artistRepo.Put`
/// then `libraryRepo.AddArtist` back to back, `scanner/phase_1_folders.go:401-412`).
///
/// The transaction is the caller's: this never begins, commits, retries, or runs in
/// autocommit, and there is no connection-bound variant. Raw SQLite errors keep their concrete
/// cause and extended code on the [`DbOperationError::Sqlite`] arm — Go returns artist Put
/// errors raw and unwrapped (`:243-244` adds no `fmt.Errorf`).
///
/// # Errors
/// [`DbOperationError::Domain`]·[`ArtistWriteError::EmptyId`] for an empty
/// [`ArtistRecord::id`]; [`DbOperationError::Sqlite`] carrying the concrete
/// `rusqlite::Error` (extended code intact) from the UPDATE or the INSERT.
/// A later caller-owned library link can fail separately in the same transaction;
/// such a failure is not emitted by this function. A repeated artist id is an
/// UPDATE hit rather than a constraint failure; a zero-row UPDATE is NOT an error.
pub fn put_artist_in_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &ArtistRecord,
    now: &DateTime<Utc>,
    cols: &[ArtistColumn],
) -> Result<ArtistWriteResult, DbOperationError<ArtistWriteError>> {
    if record.id.is_empty() {
        // Go's `id == ""` arm generates a random id (`:635-638`). The port refuses
        // instead: artist identity belongs to SC, and inventing a DB-side id would be a
        // policy nobody asked for.
        return Err(DbOperationError::Domain(ArtistWriteError::EmptyId));
    }
    let statements = put_sql(record, *now, &column_names(cols));
    // UPDATE first (`:624-633`): a row count > 0 ends the Put, so the INSERT never runs.
    if crate::execution::execute(tx, &statements.0).map_err(DbOperationError::sqlite)? > 0 {
        return Ok(ArtistWriteResult {
            id: record.id.clone(),
            inserted: false,
        });
    }
    // INSERT fallback (`:634-641`).
    crate::execution::execute(tx, &statements.1).map_err(DbOperationError::sqlite)?;
    Ok(ArtistWriteResult {
        id: record.id.clone(),
        inserted: true,
    })
}

/// `getAllIDs` (`:283-291`): `SELECT artist.id` over the library joins,
/// `GROUP BY artist.id`, the annotation join gated like `CountAll`'s, plus
/// the caller's filters/pagination — the `GetCursor` pre-pass
/// (`:299-308` + `artwork_hydration.go:41-54`'s `chunkOptions`, which
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
    sql.push_str(&pagination_fragment(options));
    (sql, args)
}

/// `chunkOptions` (`artwork_hydration.go:41-54`): the per-chunk options of
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

/// `getIndexKey` (`:310-322`): the first matching index-group prefix of
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

/// `markOrphansMissing` (`:410-418`): the verbatim UPDATE.
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

/// `markMissing` (`:421-437`): the CTE UPDATE, verbatim.
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

/// `RefreshPlayCounts` (`:441-460`) EXECUTED on the CALLER's transaction: the one
/// zero-argument CTE upsert [`refresh_play_counts_sql`] renders, run verbatim through
/// [`crate::execution::execute`] — the executed half of that renderer, not a second
/// statement. DB-17 owns ONLY this method; `RefreshStats` (`:465-614`), its id selects
/// ([`refresh_stats_ids_sql`]), the batched `library_artist.stats` UPDATE
/// ([`refresh_stats_batch_sql`]), the cleanup delete ([`refresh_stats_cleanup_sql`]) and
/// `markOrphansMissing` stay renderers owned by DB-18 (whose `D-STATS` gate is still open).
///
/// Go truth, SOURCE-VERIFIED at those lines: the CTE aggregates `sum(play_count)` /
/// `max(play_date)` over `media_file JOIN annotation ON item_id = media_file.id` LEFT JOIN
/// `json_tree(participants, '$.artist')` filtered to `atom is not null and key = 'id'`,
/// grouped by `(user_id, atom)`, then upserts `item_type = 'artist'` rows setting only
/// `play_count` and `play_date`. Pinned consequences:
///
/// * **`rating`/`starred`/`starred_at`/`rated_at` and `artist.average_rating` survive** —
///   the conflict arm names only the two play columns.
/// * **The fold is over the media file's OWN `participants` column**, so one play of a
///   track with two `artist` entries credits BOTH artists (the aggregate is a fold, not a
///   lookup of `media_file.artist_id`), and a media file whose `participants` carries no
///   `$.artist` array contributes nothing to this statement.
/// * **No `play_date` comparison guard**, exactly as in the album arm: the aggregate's
///   `max(play_date)` replaces the stored value even when it is older.
/// * **NULL aggregates**: `where total_play_count > 0` cannot match NULL, so an all-NULL
///   `play_count` population inserts no row rather than a NULL one, and no `coalesce`
///   exists in Go's text or here.
///
/// Returns the statement-level affected-row count — Go's `executeSQL` `RowsAffected`
/// (`sql_base_repository.go:336-353`), returned unchanged at `:459`. Not an entity count:
/// INSERT and existing-row UPDATE count alike, and a group whose aggregate equals the
/// stored row still counts as one change.
///
/// Zero bound arguments — no chunking and no variable-number exposure (the 4×1000-param
/// concern belongs to DB-18's batch builder). Retryable: a rerun recomputes the same
/// aggregates onto the same rows.
///
/// No production caller is wired here: Go's trigger is `phase_3_refresh_albums.go:150`
/// (inside `WithTxRetry`, `changesDetected`-gated at `:130`). SC's phase-3 lane is the
/// consumer; this ships as a primitive, as DB-14/15/16 did.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact so the executor's busy classifier still sees `SQLITE_BUSY`), including the hard
/// `malformed JSON` failure a damaged `participants` cell raises. [`DbOperationError::Domain`]
/// is structurally uninhabited ([`ArtistPlayCountError`]).
///
/// Divergence, deliberate: Go's caller wraps as `fmt.Errorf("refreshing artist annotations:
/// %w", err)` (`phase_3_refresh_albums.go:154`); the port keeps the raw cause on the
/// `Sqlite` arm (`decisions.md` E1). Text differs, cause does not.
pub fn refresh_play_counts_in_tx(
    tx: &rusqlite::Transaction<'_>,
) -> Result<usize, ArtistPlayCountError> {
    crate::execution::execute(tx, &refresh_play_counts_sql()).map_err(DbOperationError::sqlite)
}

/// Callback error of [`refresh_play_counts_in_tx`], which has no domain failure to carry:
/// Go's `RefreshPlayCounts` returns `(int64, error)` whose error can only be a driver
/// failure (`artist_repository.go:441-460`) — no validation, no `NotFound`, no permission
/// check. `Infallible` makes the `Domain` arm structurally uninhabited rather than
/// reserved, as in [`crate::album_repo::AlbumTouchError`] and
/// [`crate::tag_repo::TagWriteError`], and keeps every SQLite failure on the `Sqlite` arm
/// so `store::is_busy` still classifies `SQLITE_BUSY` for the executor's retry loop.
pub type ArtistPlayCountError = DbOperationError<std::convert::Infallible>;

/// `RefreshPlayCounts` (`:441-460`): the annotation upsert from the
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

/// `searchCfg` (`:618-633`): the artist search config — natural order by
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

/// `getSearchStrategy` (`sql_search.go:41-49`) for the artist table: the
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

/// `Search` (`:652-671`): consume the `library_id` filter into the
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
            sql.push_str(&pagination_fragment(Some(options)));
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

/// `hydrateRowidPage` (`sql_search.go:117-130`): splice the ranked rowid
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
    let (base_sql, base_args) = base;
    // Go strips the base's pagination before hydrating (RemoveLimit/
    // RemoveOffset, sql_search.go:126-127): the page query owns the
    // LIMIT/OFFSET, and squirrel's bounds are literals so no args go with them.
    let base_sql = strip_pagination(&base_sql);
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
/// string: drop the trailing `LIMIT n [OFFSET n]` tail the base select carried.
/// The bounds are decimal literals (`select.go:139-147`), so nothing is popped
/// from the args list.
#[must_use]
fn strip_pagination(sql: &str) -> String {
    /// End offset of a `<keyword><signed decimal>` suffix, if the SQL ends with
    /// one; `None` when the tail after the keyword is not a bare number.
    fn bound_suffix(s: &str, keyword: &str) -> Option<usize> {
        let index = s.rfind(keyword)?;
        let tail = &s[index + keyword.len()..];
        let digits = tail.strip_prefix('-').unwrap_or(tail);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(index)
    }
    let mut s = sql.to_string();
    // Pagination is the last thing the base select appended, so peel the
    // trailing `OFFSET n` then the trailing `LIMIT n` (including the port's
    // `LIMIT -1` unlimited bound), mirroring RemoveLimit + RemoveOffset.
    while let Some(index) = bound_suffix(&s, " OFFSET ") {
        s.truncate(index);
    }
    while let Some(index) = bound_suffix(&s, " LIMIT ") {
        s.truncate(index);
    }
    s
}

/// `requestedLibraryIDs` (`:700-707`): the `[]int` of an
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

/// `isLibraryIDFilter` (`:711-718`): whether the filter is the
/// `library_id` key, so [`search_sql`] can consume it before it reaches
/// the bare artist table (which has no `library_id` column).
#[must_use]
pub fn is_library_id_filter(filter: Option<&Filter>) -> bool {
    matches!(filter, Some(Filter::Eq { column, .. }) if column == "library_id")
}

/// `searchScope` (`:676-696`): intersect the requested libraries with the
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

/// `sortMappingsForRole` (`:730-744`): a copy of the shared stats sort
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

/// `setSortMappings` (`sql_base_repository.go:126-137`; call site at `artist_repository.go:147-159`): the artist sort mappings, keyed by the
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

/// Failures of the executed artist reads ([`ArtistRepo`]).
///
/// The variant set follows the sibling `AlbumRepo::AlbumError`
/// (`album_repo.rs:1403-1417`), which itself follows `UserRepo::UserError` and
/// `PlayerRepo::PlayerError`: a statement/hydration failure and the not-found
/// case, nothing else.
#[derive(Debug, thiserror::Error)]
pub enum ArtistError {
    /// The SQLite statement failed, or a row's hydration failed. Go's read funnel
    /// surfaces a `PostScan` failure as a column-conversion error
    /// (`dbx@v1.12.0/rows.go:92-102`), which is why a decode failure arrives here
    /// too — via [`crate::hydrate::HydrateError::into_sqlite`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`), which
    /// `Get` decides from `len(dba) == 0` (`artist_repository.go:261-263`).
    ///
    /// This can ONLY come from the zero-row check, never from a driver-level
    /// "no rows" condition: `dbx`'s `All` loops `Next` then `Close` and makes an
    /// empty NON-nil slice for a zero-row result (`dbx@v1.12.0/query.go:274-291`,
    /// `rows.go:109-125`), and `sql.ErrNoRows` is only ever returned by
    /// `Rows.one`/`Rows.row` (`rows.go:237-244`, `:281-294`) — which the artist
    /// read never reaches, because Go's `Get` is `queryAll` + a `len` check
    /// (`:258-262`), not `queryOne`. So `queryAll`'s ErrNoRows→ErrNotFound arm
    /// (`sql_base_repository.go:451-454`) is dead code on this path, and
    /// [`ArtistRepo::get_all`] must NEVER report this variant.
    #[error("record not found")]
    NotFound,
}

/// `artistRepository` (`artist_repository.go:26-30`) bound to one pooled
/// connection — the executed READ side of the artist repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this
/// crate has no ambient context (the `sqlrepo.rs` DI freeze), so the caller
/// threads a lease from [`crate::store::SQLStore::with_connection_sync`] (a
/// transaction derefs to a `&Connection`) and every statement runs through
/// [`crate::execution`] — the port of `executeSQL`/`queryOne`/`queryAll`
/// (`sql_base_repository.go:336-355,375-388,441-457`). Rows are hydrated through
/// the shared seam exactly as the album/mediafile reads do —
/// [`crate::hydrate::row_to_artist`] → [`ArtistRow::into_record`], with
/// [`crate::hydrate::HydrateError::into_sqlite`] as the exit shape — and artwork
/// hydration is the second layer, [`hydrate_artist_artwork`]
/// (`hydrateArtwork`, `artist_repository.go:294-297`).
///
/// **Why `get` goes through `query_all` and not `query_optional`:** Go's `Get`
/// (`:255-267`) is `queryAll` over `selectArtist` narrowed to one id, then
/// `if len(dba) == 0 { return nil, model.ErrNotFound }`, then `toModels()` +
/// `hydrateArtwork` over the whole (0-or-1 element) slice and `&res[0]`. It
/// never reaches `queryOne`, so `sql.ErrNoRows` is not this code path — see
/// [`ArtistError::NotFound`] for the dbx proof. Neither `Get` nor `GetAll` ever
/// takes `optimizePagination`: `queryAll` is called with no options at `:258`
/// and `:272` (contrast `mediafile_repository.go:235`).
///
/// **Note on `annotation_user`:** `None` is Go's `invalidUserId`
/// (`sql_base_repository.go:51`), for which `withAnnotation` returns the query
/// untouched (`sql_annotations.go:63-65`) — no join and no annotation columns.
/// Go's single annotation column for that case is then `artist.average_rating`,
/// which `artist.*` already carries (`nd-dbschema::schema_ddl`'s artist DDL:
/// `average_rating REAL NOT NULL DEFAULT 0`), so no extra projection column is
/// added: [`crate::hydrate::row_to_annotation`]'s `has_column` probe sees it in
/// the `artist.*` expansion and reads it, while the six per-user columns stay
/// absent and decode to zeros.
#[derive(Debug)]
pub struct ArtistRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> ArtistRepo<'a> {
    /// `NewArtistRepository` (`artist_repository.go:133-161`): bind to `conn`
    /// (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`artist_repository.go:255-267`): [`get_sql`] (the stats select
    /// narrowed to one id), hydrated through both layers, with a zero-row read
    /// mapped to [`ArtistError::NotFound`] the way Go decides it from `len(dba)`.
    ///
    /// `annotation_user` is Go's `loggedUser(ctx).ID` for
    /// `withAnnotation`; `None` is `invalidUserId` (no join — see [`ArtistRepo`]).
    ///
    /// # Errors
    /// [`ArtistError::NotFound`] when the id matches nothing — also when the
    /// `library_artist` INNER join drops the artist (an artist with no junction
    /// row) or the `user_library` join excludes it for a non-admin
    /// (`artist_repository.go:199-202`); [`ArtistError::Sqlite`] on a statement
    /// or hydration failure. An artwork-lookup failure is NOT an error here —
    /// Go swallows it too, because `hydrateItems` logs and returns an EMPTY map
    /// (`artwork_hydration.go:58-68`), leaving the artist with a zero
    /// (unresolved) [`crate::artwork_hydration::ItemImage`]. Use
    /// [`ArtistRepo::get_all`] when the caller needs that failure reported.
    /// Go's `Get` returns `nil` beside its error on BOTH paths (`:259-262`), so
    /// the port's `Result` carries the same information.
    pub fn get(
        &self,
        id: &str,
        user: &User,
        annotation_user: Option<&str>,
    ) -> Result<ArtistRecord, ArtistError> {
        let annotation = annotation_user.and_then(annotation_join);
        let statement = get_sql(id, user, annotation.as_ref());
        let records = self.read_hydrated(&statement)?;
        // Go hydrates the whole (0-or-1 element) slice, then takes res[0] (`:264-266`).
        let mut outcome = self.hydrate_artwork(records);
        if outcome.items.is_empty() {
            return Err(ArtistError::NotFound);
        }
        // Go's swallow-and-log path: the page still renders (artwork_hydration.go:62-66).
        let _ = outcome.artwork_error;
        Ok(outcome.items.remove(0))
    }

    /// `GetAll` (`artist_repository.go:269-279`): [`get_all_sql`] under the
    /// caller's options, then artwork hydration. Go's
    /// `selectArtist(ctx, options...)` runs BOTH `applyOptions`
    /// (`sql_base_repository.go:149-161`) and `applyFilters` (`:247-252`);
    /// this port expresses only part of that: `filters` ARE rendered
    /// (`where_fragment`), LIMIT/OFFSET ARE (`pagination_fragment`), but the
    /// `OrderBy(buildSortOrder(sort, order))` leg (`:157-158`) is NOT rendered,
    /// so a non-empty [`QueryOptions::sort`] changes nothing and rows come back
    /// in SQLite's order. Executed:
    /// `tests/runtime_hydrate_artist.rs::get_all_sql_ignores_the_sort_leg_until_options_are_wired`.
    ///
    /// An empty result is an empty `Vec`, never [`ArtistError::NotFound`]:
    /// `GetAll` returns `res` (`dba.toModels()`, which `make`s a non-nil slice)
    /// with `err` from the `queryAll` call (`:272-275`).
    ///
    /// `artwork_error` carries Go's swallowed `log.Error`
    /// (`artwork_hydration.go:62-66`): the artists come back either way, each
    /// with a zero (unresolved) [`crate::artwork_hydration::ItemImage`], exactly
    /// as Go's empty map leaves them. That is why this returns `Ok` for an
    /// artwork failure and `Err` only for a statement or row-hydration failure
    /// (`:273-275` returns `queryAll`'s error unchanged).
    ///
    /// # Errors
    /// [`ArtistError::Sqlite`] when the statement fails or a row will not hydrate.
    pub fn get_all(
        &self,
        user: &User,
        annotation_user: Option<&str>,
        options: Option<&QueryOptions>,
    ) -> Result<ReadOutcome<ArtistRecord>, ArtistError> {
        let annotation = annotation_user.and_then(annotation_join);
        let statement = get_all_sql(user, options, annotation.as_ref());
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records))
    }

    /// Phase 1 + phase 2 (Go's `queryAll` + `toModels()`): the raw rows through
    /// the shared seam, the stats shim and the similar-artists column decoded.
    /// A `PostScan` failure aborts the whole read, as it does in Go.
    fn read_hydrated(&self, statement: &Sql) -> Result<Vec<ArtistRecord>, ArtistError> {
        let rows =
            crate::execution::query_all(self.conn, statement, crate::hydrate::row_to_artist)?;
        // Go's read funnel leaves a PostScan failure as a column-conversion error,
        // so `into_sqlite` is the exit shape and `ArtistError::Sqlite` wraps it.
        Ok(rows
            .into_iter()
            .map(ArtistRow::into_record)
            .collect::<Result<Vec<ArtistRecord>, crate::hydrate::HydrateError>>()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?)
    }

    /// Phase 3 (`hydrateArtwork`, `artist_repository.go:294-297`): one batched
    /// `ar`-kind lookup over the hydrated records.
    fn hydrate_artwork(&self, records: Vec<ArtistRecord>) -> ReadOutcome<ArtistRecord> {
        let mut items = records;
        let mut artwork_error = None;
        hydrate_artist_artwork(self.conn, &mut items, &mut artwork_error);
        ReadOutcome {
            items,
            artwork_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    /// Deterministic caller clock (Go's `time.Now()`,
    /// `artist_repository.go:240`).
    fn at(year: u32, month: u32, day: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(i32::try_from(year).unwrap(), month, day)
                .unwrap()
                .and_hms_opt(h, mi, s)
                .unwrap(),
        )
    }
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
             WHERE starred = ? GROUP BY artist.id LIMIT 10 OFFSET 20"
        );
        assert_eq!(args, vec![Value::Text("u1".to_string()), Value::Bool(true)]);
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

    /// Go parity (DB-08, parent decision): `selectUpdateColumns`
    /// (`sql_base_repository.go:593-610`) keeps EXACTLY the requested columns, so a
    /// filtered UPDATE does NOT re-stamp `updated_at` unless it is selected. The previous
    /// behavior — appending `updated_at` to every filtered SET — was a divergence at HEAD and
    /// made `UpdateExternalInfo` (`artist_repository.go:247-253`) clobber the scanner's stamp.
    #[test]
    fn put_sql_update_leg_filters_cols_and_writes_updated_at_only_when_selected() {
        // Arrange: UpdateExternalInfo (:247-253) — the fixed column list, no updated_at.
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "X".to_string(),
            biography: "b".to_string(),
            ..ArtistRecord::default()
        };
        // Act: the 7-column external-info selection.
        let ((update_sql, update_args), (insert_sql, _)) =
            put_sql(&artist, at(2024, 1, 2, 0, 0, 0), &EXTERNAL_INFO_COLS);
        // Assert: exactly the 7 requested columns, no appended updated_at, never created_at.
        assert_eq!(
            update_sql,
            "UPDATE artist SET biography = ?, small_image_url = ?, medium_image_url = ?, \
             large_image_url = ?, external_url = ?, similar_artists = ?, \
             external_info_updated_at = ? WHERE id = ?"
        );
        assert_eq!(update_args.len(), 8, "7 SET args + the WHERE id");
        assert_eq!(update_args[7], Value::Text("ar-1".to_string()));
        assert!(
            !update_sql.contains(" updated_at = ?"),
            "no updated_at column in the SET (external_info_updated_at is a different column)"
        );
        assert!(!update_sql.contains("created_at"));
        assert!(insert_sql.starts_with("INSERT INTO artist (id, name, "));
        assert!(
            insert_sql.contains("updated_at"),
            "the INSERT leg still carries updated_at (Put stamps both, :240-241)"
        );
    }

    /// `updated_at` IS in the SET when the caller selects it: the scanner's own list
    /// (`scanner/phase_1_folders.go:403-405`) names it, which is why Go re-stamps there.
    #[test]
    fn put_sql_writes_updated_at_only_when_the_selection_names_it() {
        // Arrange: the scanner's 7-column list, `updated_at` included.
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "X".to_string(),
            ..ArtistRecord::default()
        };
        let scanner_cols = [
            "name",
            "mbz_artist_id",
            "sort_artist_name",
            "order_artist_name",
            "full_text",
            "search_normalized",
            "updated_at",
        ];
        // Act:
        let ((update_sql, update_args), _) =
            put_sql(&artist, at(2024, 1, 2, 0, 0, 0), &scanner_cols);
        // Assert:
        assert_eq!(
            update_sql,
            // put_cols order: the record's field order, with the derived columns last.
            // Empty sort_artist_name/mbz_artist_id are dropped by PostMapArgs (:113-120),
            // so they are absent from the SET even though the scanner requested them.
            "UPDATE artist SET name = ?, order_artist_name = ?, updated_at = ?, \
             full_text = ?, search_normalized = ? WHERE id = ?"
        );
        assert_eq!(update_args.len(), 6);
    }

    /// Go normalizes every `colsToUpdate` entry with `toSnakeCase`
    /// (`sql_base_repository.go:596-598`), so a camelCase entry selects its snake column. A
    /// selection with nothing settable runs Go's no-op `SET id = ?` (`filterUpdateValues`,
    /// `:612-616`) instead of emitting `SET  WHERE` or a missing-column error; an unfiltered
    /// selection already carries `id` and must not duplicate it.
    #[test]
    fn put_sql_normalizes_column_names_and_resolves_an_empty_selection_to_the_go_noop() {
        // Arrange:
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "X".to_string(),
            biography: "b".to_string(),
            ..ArtistRecord::default()
        };
        // Act: camelCase entries, exactly as Go's callers pass them.
        let ((camel_sql, camel_args), _) = put_sql(
            &artist,
            at(2024, 1, 2, 0, 0, 0),
            &["UpdatedAt", "externalInfoUpdatedAt"],
        );
        // Assert: normalized to the snake column keys.
        assert_eq!(
            camel_sql,
            "UPDATE artist SET external_info_updated_at = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(camel_args.len(), 3);
        // Act/Assert: `created_at` alone (dropped by selectUpdateColumns) and an entry naming
        // no column of the map both resolve to the executable no-op.
        for cols in [&["created_at"][..], &["totally_unknown_column"][..]] {
            let ((noop_sql, noop_args), _) = put_sql(&artist, at(2024, 1, 2, 0, 0, 0), cols);
            assert_eq!(
                noop_sql, "UPDATE artist SET id = ? WHERE id = ?",
                "{cols:?}"
            );
            assert_eq!(
                noop_args,
                vec![Value::Text("ar-1".to_string()); 2],
                "SET id and WHERE id"
            );
        }
        // Act/Assert: unfiltered = all columns, `id` once, stamps present, created_at dropped.
        let ((all_sql, _), (insert_sql, _)) = put_sql(&artist, at(2024, 1, 2, 0, 0, 0), &[]);
        let set_part = all_sql
            .split(" WHERE ")
            .next()
            .expect("the UPDATE carries a WHERE clause");
        assert_eq!(
            set_part.matches("id = ?").count(),
            1,
            "no duplicated SET id"
        );
        assert!(all_sql.ends_with("WHERE id = ?"));
        assert!(all_sql.contains("updated_at = ?"));
        assert!(!all_sql.contains("created_at"));
        assert!(insert_sql.contains("created_at"));
    }

    /// The typed [`ArtistColumn`] set covers exactly the settable keys of the map [`put_cols`]
    /// builds — everything except `created_at` (dropped on UPDATE) and `id` (the WHERE key) —
    /// so a caller cannot name a column outside it and no free-form SQL reaches the SET list.
    #[test]
    fn artist_column_covers_the_settable_put_cols_keys() {
        // Arrange: a record carrying the values that keep every key in the map.
        let artist = ArtistRecord {
            id: "ar-1".to_string(),
            name: "X".to_string(),
            sort_artist_name: "s".to_string(),
            mbz_artist_id: "m".to_string(),
            ..ArtistRecord::default()
        };
        let keys: Vec<String> = put_cols(&artist).into_iter().map(|(c, _)| c).collect();
        let typed: Vec<&str> = [
            ArtistColumn::Name,
            ArtistColumn::SortArtistName,
            ArtistColumn::OrderArtistName,
            ArtistColumn::MbzArtistId,
            ArtistColumn::Biography,
            ArtistColumn::SmallImageUrl,
            ArtistColumn::MediumImageUrl,
            ArtistColumn::LargeImageUrl,
            ArtistColumn::ExternalUrl,
            ArtistColumn::SimilarArtists,
            ArtistColumn::ExternalInfoUpdatedAt,
            ArtistColumn::Missing,
            ArtistColumn::UploadedImage,
            ArtistColumn::FullText,
            ArtistColumn::SearchNormalized,
            ArtistColumn::UpdatedAt,
        ]
        .iter()
        .map(|column| column.column())
        .collect();
        // Assert: every typed name is a key of the map.
        for name in &typed {
            assert!(
                keys.iter().any(|k| k.as_str() == *name),
                "{name} missing from put_cols"
            );
        }
        let mut unlisted: Vec<String> = keys
            .iter()
            .filter(|k| !typed.contains(&k.as_str()))
            .cloned()
            .collect();
        unlisted.sort();
        assert_eq!(unlisted, vec!["created_at".to_string(), "id".to_string()]);
        // An empty typed slice is Go's empty variadic list: the filter restricts nothing.
        assert!(column_names(&[]).is_empty());
        assert_eq!(column_names(&[ArtistColumn::Biography]), vec!["biography"]);
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
        // Assert: the page bounds are literals in the rowid subquery, so they
        // never reach args (`select.go:139-147`).
        assert!(sql.contains("ORDER BY artist.id LIMIT 5 OFFSET 10)"));
        assert!(sql.contains("INNER JOIN user_library"));
        assert!(sql.ends_with("GROUP BY artist.id ORDER BY _ranked._rn"));
        // Base join arg (u1) binds before the page args.
        assert_eq!(args[0], Value::Text("u1".to_string()));
        assert!(
            args.contains(&Value::Integer(1)),
            "the scope library id binds"
        );
        assert!(
            !args.contains(&Value::Integer(5)) && !args.contains(&Value::Integer(10)),
            "the literal page bounds contribute no args"
        );
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
        // Assert: the page bound renders as a literal inside the rowid
        // subquery (`select.go:139-142`), not as a bound parameter.
        assert!(sql.contains("LIMIT 10))"));
        assert!(sql.ends_with("ORDER BY _ranked._rn"));
        assert_eq!(args[0], Value::Text("(beatles OR beatles*)".to_string()));
        assert!(args.contains(&Value::Bool(false)));
        assert!(!args.contains(&Value::Integer(10)));
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
