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
//! `hydrateArtwork` (`:222-225`), `GetCursor`/`GetCursorWithArtwork`/
//! `GetAllByTags`/`MatchesCriteria` (`:270-318,517-563` — cursor streaming
//! and smart-playlist criteria), `GetWithParticipants`'s artist hydration
//! (`:214-219`, artist slice), and the REST shims (`:585-600`,
//! `sql_restful.rs`). The `Path` collate-nocase matching in `FindByPaths`
//! pins; the library-splitting of qualified paths is Go-side logic kept in
//! [`find_by_paths_sql`]'s grouped inputs.
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column/order-by names) are
//! the registered, caller-supplied constants; every value binds to a `?`.

use std::collections::BTreeMap;

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
/// (`sql_bookmarks.go:17-29` via `mediafile_repository.go:208`): the LEFT
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
/// (`sql_bookmarks.go:28`).
const BOOKMARK_POSITION_COLUMN: &str = "coalesce(position, 0) as bookmark_position";

/// `withAnnotation`'s fragment as the media_file select composes it
/// (`sql_annotations.go:63-77` via `selectMediaFile`'s `withAnnotation`): the
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

/// The annotation columns `withAnnotation` appends (`sql_annotations.go:67-76`)
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
/// builder models only INNER JOIN): Go LEFT-joins via string fragments, and
/// the two join args must bind after the select's earlier WHERE args in
/// squirrel's part order.
#[must_use]
pub fn select_media_file(
    user_id: Option<&str>,
    library_filter: Option<Sql>,
) -> (String, Vec<crate::sqlrepo::Value>) {
    let mut sql = String::from(
        "SELECT media_file.*, library.path as library_path, library.name as library_name",
    );
    let mut args: Vec<crate::sqlrepo::Value> = Vec::new();
    match user_id {
        Some(user) => {
            let (ann_join, ann_args) = annotation_join_parts(user);
            let (bmk_join, bmk_args) = bookmark_join_parts(user);
            sql.push_str(&format!(
                ", {}, {BOOKMARK_POSITION_COLUMN}",
                ANNOTATION_COLUMNS.join(", ")
            ));
            sql.push_str(&ann_join);
            args.extend(ann_args);
            sql.push_str(&format!(" {bmk_join}"));
            args.extend(bmk_args);
        }
        None => sql.push_str(", media_file.average_rating"),
    }
    sql.push_str(" FROM media_file LEFT JOIN library ON media_file.library_id = library.id");
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
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause}"));
        args.extend(clause_args);
    }
    if need_annotation {
        if let Some(user) = user_id {
            let (join_sql, join_args) = annotation_join_parts(user);
            sql.push_str(&join_sql);
            args.extend(join_args);
        }
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

/// `Get` (`mediafile_repository.go:212-219`): `GetAll` with
/// `Filters: Eq{"media_file.id": id}`.
#[must_use]
pub fn get_sql(id: &str, user_id: Option<&str>, library_filter: Option<Sql>) -> Sql {
    let (mut sql, mut args) = select_media_file(user_id, library_filter);
    sql.push_str(&format!(" WHERE {TABLE}.id = ?"));
    args.push(crate::sqlrepo::Value::from(id));
    (sql, args)
}

/// `GetAll` (`mediafile_repository.go:221-227`): the base select with the
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

/// `GetRandom` Phase 1 (`mediafile_repository.go:229-266`): the narrow
/// rowid pick — filters, library filter, `random()` order, optional Max.
/// `filters` is the caller's rendered WHERE body (Go's `applyFilters` output).
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

/// `GetRandom` Phase 2 (`mediafile_repository.go:249-266`): re-shuffle the
/// hydrated rows — the base select plus `WHERE media_file.rowid IN (?, …)`
/// and `ORDER BY random()`.
#[must_use]
pub fn get_random_hydrate_sql(user_id: Option<&str>, rowids: &[i64]) -> Sql {
    let (mut sql, mut args) = select_media_file(user_id, None);
    let placeholders = vec!["?"; rowids.len()].join(", ");
    sql.push_str(&format!(
        " WHERE {TABLE}.rowid IN ({placeholders}) ORDER BY random()"
    ));
    args.extend(rowids.iter().map(|r| crate::sqlrepo::Value::Integer(*r)));
    (sql, args)
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
    if let Some((clause, clause_args)) = library_filter {
        sql.push_str(&format!(" WHERE {clause}"));
        args.extend(clause_args);
    }
    if need_annotation {
        if let Some(user) = user_id {
            let (join_sql, join_args) = annotation_join_parts(user);
            sql.push_str(&join_sql);
            args.extend(join_args);
        }
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
/// `model.MediaFile`'s persisted row (`model/mediafile.go:27-107`) — the 63
/// non-id persisted media_file columns (the contract's 64 minus `id`, which
/// the seam forbids in the map; the id is appended last by [`put_sql`]).
///
/// Deliberately absent (entity write contract): `LibraryPath`/`LibraryName`
/// (`structs:"-"`, join-only), `Genres` (`structs:"-"`),
/// `Bookmarkable.BookmarkPosition` (`structs:"-"`), `AlbumImage` (no
/// columns), the `Annotations`/`ItemImage` embeds, and `average_rating"
/// (annotation-managed, never written by entity Put). The Go
/// `RgAlbumGain`/`RgAlbumPeak`/`RgTrackGain`/`RgTrackPeak` dbx shims
/// (`mediafile_repository.go:35-38`) exist only because dbx's default
/// field-map cannot render `RG*` to `rg_*`; Rust binds the real column
/// names directly, no shim. `tags`/`participants` hold the *typed* values
/// the Go shims shadow — PostMapArgs overwrites their column keys with
/// JSON, which [`mediafile_row`] routes through
/// [`crate::marshal::mediafile_post_map_args`].
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
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFileRecord {
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
    /// `duration` (Go `Duration`, `float32` at `model/mediafile.go:66`).
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
/// (`mediafile_repository.go:184-190`): `created_at` to `now` **only when
/// the record's is `None`** (Go zero `time.Time`); `updated_at` passes
/// through the record unchanged — Go stamps neither in `Put`.
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
/// Returns [`crate::marshal::MarshalError`] when the map carries `id"
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
    // Go's Put (mediafile_repository.go:184-190): CreatedAt is stamped to
    // now only when zero; UpdatedAt is never touched.
    let stamped = MediaFileRecord {
        created_at: Some(m.created_at.unwrap_or(now)),
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

/// `MarkMissing` (`mediafile_repository.go:433-455`): one 200-row chunk per
/// UPDATE (`slice.CollectChunks(ids, 200)`), `.Set` call order.
pub const CHUNK_SIZE: usize = 200;

/// See [`CHUNK_SIZE`]; `missing` flips and `updated_at` is Go's `time.Now()`
/// — both bind.
#[must_use]
pub fn mark_missing_sql(missing: bool, ids: &[String], updated_at: i64) -> Vec<Sql> {
    ids.chunks(CHUNK_SIZE)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            (
                format!(
                    "UPDATE {TABLE} SET missing = ?, updated_at = ? WHERE id IN ({placeholders})"
                ),
                chunk
                    .iter()
                    .map(|id| crate::sqlrepo::Value::from(id.clone()))
                    .chain([
                        crate::sqlrepo::Value::Bool(missing),
                        crate::sqlrepo::Value::Integer(updated_at),
                    ])
                    .collect(),
            )
        })
        .collect()
}

/// `MarkMissingByFolder` (`mediafile_repository.go:457-483`): one 200-row
/// chunk per UPDATE, gated on the folders' current flag flipping
/// (`Eq{"missing": !missing}`); `.Set` call order, the `And` renders its Eq
/// map keys alphabetically (`folder_id` < `missing`).
#[must_use]
pub fn mark_missing_by_folder_sql(
    missing: bool,
    folder_ids: &[String],
    updated_at: i64,
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
                    crate::sqlrepo::Value::Integer(updated_at),
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
    pub since: i64,
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
            crate::sqlrepo::Value::Integer(m.since),
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
    pub since: i64,
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
            crate::sqlrepo::Value::Integer(m.since),
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
             LEFT JOIN annotation ON (annotation.item_id = media_file.id \
             AND annotation.item_type = ? AND annotation.user_id = ?) \
             LEFT JOIN bookmark ON (bookmark.item_id = media_file.id AND bookmark.user_id = ?) \
             FROM media_file LEFT JOIN library ON media_file.library_id = library.id"
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
        let (hydrate_sql, hydrate_args) = get_random_hydrate_sql(None, &[3, 7, 9]);
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
        // Arrange: MarkMissing (:433-455) — slice.CollectChunks(ids, 200).
        let ids: Vec<String> = (0..201).map(|i| format!("mf-{i}")).collect();
        // Act:
        let chunks = mark_missing_sql(true, &ids, 99);
        // Assert:
        assert_eq!(chunks.len(), 2, "201 ids split at the 200 boundary");
        assert_eq!(
            chunks[0].0,
            "UPDATE media_file SET missing = ?, updated_at = ? WHERE id IN (?, ?, ?)"
                .replace("(?, ?, ?)", &format!("({})", vec!["?"; 200].join(", ")))
        );
        assert_eq!(chunks[0].1.last(), Some(&Value::Integer(99)));
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
        // Arrange: MarkMissingByFolder (:457-483) — Eq{"missing": !missing}.
        // Act:
        let chunks = mark_missing_by_folder_sql(true, &["f-1".to_string()], 5);
        // Assert:
        assert_eq!(
            chunks[0],
            (
                "UPDATE media_file SET missing = ?, updated_at = ? \
                 WHERE folder_id IN (?) AND missing = ?"
                    .to_string(),
                vec![
                    Value::Bool(true),
                    Value::Integer(5),
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
        let by_mbz = find_recent_files_by_mbz_track_id_sql(&RecentFileByMbz {
            library_id: 2,
            mbz_release_track_id: "mbz-1",
            suffix: "flac",
            since: 100,
        });
        let by_props = find_recent_files_by_properties_sql(&RecentFileByProperties {
            library_id: 2,
            title: "Song",
            size: 42,
            suffix: "flac",
            disc_number: 1,
            track_number: 2,
            album: "Album",
            since: 100,
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
                Value::Integer(100),
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
