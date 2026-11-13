//! Port of `persistence/album_repository.go` (480L) — the album repository.
//!
//! Go's repository is a `sqlRepository` over `album` whose select joins
//! `library` (LEFT — `library_path`/`library_name` are display columns) and
//! the logged-in user's `annotation` row (`selectAlbum`, `:166-172`). Text
//! search reuses the shared `doSearch` contract: the album repo only builds
//! [`album_search_config`] (`:435-440`) and hands the caller-resolved
//! strategy ([`get_search_strategy`]) to `crate::sql_search::do_search` —
//! the search SQL itself lives in the `sql_search*` modules and is not
//! reimplemented here, per the crate's lane split.
//!
//! Every method builds a squirrel statement and hands it to `queryOne`/
//! `queryAll`/`count`/`queryAllSlice`/`executeSQL`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution
//! stays with the caller (`store.rs` seam), so this port stops at the same
//! [`Sql`] pairs.
//!
//! `dbAlbum.PostScan`/`PostMapArgs` (`:30-86`) marshal the JSON columns
//! (`discs`, `participants`, `tags`, `folder_ids`, `full_text`,
//! `search_participants`, `search_normalized`) — the write path is the
//! record-driven [`put_sql`] (S-D2b): [`AlbumRecord`] → [`album_row`] (the
//! `structs.Map(dbAlbum)` + `dbAlbum.PostMapArgs` map, with the derived FTS
//! inputs computed atomically from the record's own participants/discs/
//! tags) → the shared [`crate::marshal::MarshalledRow`] seam — no
//! caller-supplied column arrays for the write path.
//!
//! Deliberately absent (other lanes/slices own them): `hydrateArtwork`
//! (`:180-183`, the artwork-hydration slice), the streaming/cursor machinery
//! `queryWithStableResults`/`wrapAlbumCursor`/`streamByIDs`/`chunkOptions`
//! (`:296-324`, `persistence/artwork_hydration.go` — their WHERE shapes pin
//! here, see [`get_touched_albums_sql`]/[`cursor_chunk_sql`]), the
//! `getSortMappings`/`naturalSort`/`mapSortOrder` config-gated transforms
//! (`sql_base_repository.go:125-137` — the mapping table pins here as data,
//! [`sort_mappings`]) and the REST shims `Count`/`Read`/`ReadAll`
//! (`:446-459`, `sql_restful.rs` owns the options parsing).
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column/order-by names) are
//! the registered, caller-supplied constants; every value binds to `?`.
//! Order-by identifiers arrive caller-supplied and direction-carrying per
//! `buildSortOrder` output — no user text reaches an ORDER BY fragment.

use std::collections::BTreeMap;

use crate::sql_annotations::ANNOTATION_TABLE;
use crate::sql_participations::{participant_id_filter, Participants};
use crate::sql_restful::{boolean_filter, Filter};
use crate::sql_search::{do_search, SearchConfig, SearchOutcome, SearchStrategy, Sql};
use crate::sql_search_fts::{new_fts_search, SearchStrategy as FtsFallback};
use crate::sql_search_like::{new_legacy_search, new_like_search};
use crate::sql_tags::{album_genres, delete_tags_sql, genre_id_filter, insert_tags_sql, DbTags};
use crate::sqlrepo::{Repository, SqlError, Value};
use chrono::{DateTime, Utc};

/// `r.tableName` (`NewAlbumRepository`, `album_repository.go:120`).
pub const ALBUM_TABLE: &str = "album";

/// `setSortMappings` (`album_repository.go:134-146`): the album sort field →
/// order-by expression table as data. `natural_sort_album_name` is
/// `naturalSort("album.name")` (`helpers.go:89-93` — the config-gated
/// collate parens are the caller's; the plain column form pins here as
/// [`NATURAL_SORT_ALBUM_NAME`]). `recently_added` is `recentlyAddedSort()`
/// (`:152-157`): mod-time → `album.updated_at`, else `album.created_at`,
/// both `, album.id`.
#[must_use]
pub fn sort_mappings(
    natural_sort_album_name: &str,
    recently_added_by_mod_time: bool,
) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert(
        "name".to_string(),
        "order_album_name, order_album_artist_name".to_string(),
    );
    m.insert(
        "artist".to_string(),
        "compilation, order_album_artist_name, order_album_name".to_string(),
    );
    m.insert(
        "album_artist".to_string(),
        "compilation, order_album_artist_name, order_album_name".to_string(),
    );
    m.insert(
        "max_year".to_string(),
        format!(
            "coalesce(nullif(original_date,''), cast(max_year as text)), \
             release_date, {natural_sort_album_name}"
        ),
    );
    m.insert("random".to_string(), "random".to_string());
    m.insert(
        "recently_added".to_string(),
        if recently_added_by_mod_time {
            "album.updated_at, album.id".to_string()
        } else {
            "album.created_at, album.id".to_string()
        },
    );
    m.insert("starred_at".to_string(), "starred, starred_at".to_string());
    m.insert("rated_at".to_string(), "rating, rated_at".to_string());
    m
}

/// `naturalSort("album.name")`'s plain form (`helpers.go:89-93` with
/// `EnableNaturalSorting` off): the bare column, no collate parens.
pub const NATURAL_SORT_ALBUM_NAME: &str = "album.name";

/// `albumSearchConfig` (`album_repository.go:435-440`).
#[must_use]
pub fn album_search_config() -> SearchConfig {
    SearchConfig {
        natural_order: "album.rowid".to_string(),
        order_by: vec!["name".to_string()],
        mbid_fields: vec![
            "mbz_album_id".to_string(),
            "mbz_release_group_id".to_string(),
        ],
    }
}

/// `Search` (`album_repository.go:441-447`): dispatch through the shared
/// `doSearch` (`crate::sql_search::do_search`, `sql_search.go:35-62`).
/// `sq` arrives pre-built by the caller ([`select_album_sql`] + filters);
/// `options` carries the same filters/pagination Phase 1 must apply.
/// `library_filter` is the caller-resolved `applyLibraryFilter` clause.
pub fn search_sql(
    sq: Repository,
    q: &str,
    options: Option<&crate::sql_restful::QueryOptions>,
    library_filter: Option<Sql>,
    strategy: Option<SearchStrategy>,
) -> Result<SearchOutcome, SqlError> {
    do_search(
        ALBUM_TABLE,
        sq,
        q,
        &album_search_config(),
        options,
        library_filter,
        strategy,
    )
}

/// The search strategy for `query` (`getSearchStrategy`,
/// `sql_search.go:36-49`), config-ordered: legacy backend / full-string →
/// legacy LIKE; CJK → core-column LIKE; else FTS5 (whose degraded queries
/// fall back to the core-column LIKE, `sql_search_fts.go:331`). `None` when
/// the query yields no searchable tokens (Go's nil strategy). The LIKE
/// family carries no two-phase split (`rowid_core: None`,
/// `sql_search_like.go:16-24`); FTS5's phase-1 core orders by the BM25 rank
/// expression first (`sql_search_fts.go:233-240`).
#[must_use]
pub fn get_search_strategy(
    query: &str,
    legacy_backend: bool,
    full_string: bool,
) -> Option<SearchStrategy> {
    fn like(search: crate::sql_search_like::LikeSearch) -> SearchStrategy {
        SearchStrategy {
            clause: search.to_sql(),
            rowid_core: None,
        }
    }
    if legacy_backend || full_string {
        return new_legacy_search(ALBUM_TABLE, query, full_string).map(like);
    }
    if crate::sql_search_fts::contains_cjk(query) {
        return new_like_search(ALBUM_TABLE, query).map(like);
    }
    match new_fts_search(ALBUM_TABLE, query) {
        Some(FtsFallback::Fts(fts)) => Some(SearchStrategy {
            clause: fts.to_sql(),
            rowid_core: Some(fts.rowid_select(&["name asc".to_string()])),
        }),
        Some(FtsFallback::Like(cleaned)) => new_like_search(ALBUM_TABLE, &cleaned).map(like),
        None => None,
    }
}

/// The `selectAlbum` shape (`album_repository.go:166-172`): `album.*`, the
/// library display columns (Go LEFT-joins `library` — the path/name are
/// display-only), then the logged-in user's annotation LEFT JOIN
/// (`withAnnotation`, `sql_annotations.go:63-77`). The sqlrepo builder models
/// only INNER JOIN, so — like the sibling `playlist` lane — the whole select
/// renders as one literal with the joins at their FROM position; the join's
/// `?` args bind BEFORE the WHERE args (squirrel binds join args at the join
/// position).
///
/// `user_id: None` keeps only `album.average_rating`
/// (`sql_annotations.go:63-65` — Go's invalid-user branch adds the bare
/// column). `library_filter` is the caller-resolved `applyLibraryFilter`
/// clause (`sql_base_repository.go:261-282`). `where_clauses` are the
/// caller's already-materialized filter fragments; `order_by`/`max`/
/// `offset` follow `applyOptions` (`sql_base_repository.go:105-112`).
#[must_use]
pub fn select_album_sql(
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
    where_clauses: &[Sql],
    order_by: &[String],
    max: i64,
    offset: i64,
) -> Sql {
    let play_count = if normalized_album_play_count {
        "round(coalesce(round(cast(play_count as float) / coalesce(album.song_count, 1), 1), 0)) as play_count"
    } else {
        "coalesce(play_count, 0) as play_count"
    };
    let mut sql = match user_id {
        Some(_user) => format!(
            "SELECT album.*, library.path as library_path, library.name as library_name, \
             coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
             play_date, rated_at, {play_count}, album.average_rating \
             FROM album LEFT JOIN library ON library.id = album.library_id \
             LEFT JOIN {ANNOTATION_TABLE} ON ({ANNOTATION_TABLE}.item_id = album.id \
             AND {ANNOTATION_TABLE}.item_type = ? AND {ANNOTATION_TABLE}.user_id = ?)"
        ),
        None => "SELECT album.*, library.path as library_path, library.name as library_name, \
             album.average_rating FROM album \
             LEFT JOIN library ON library.id = album.library_id"
            .to_string(),
    };
    let mut args: Vec<Value> = Vec::new();
    if let Some(user) = user_id {
        args.push(Value::from(ALBUM_TABLE));
        args.push(Value::from(user));
    }
    let mut first_clause = true;
    if let Some((filter_sql, filter_args)) = library_filter {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
        first_clause = false;
    }
    for (clause_sql, clause_args) in where_clauses {
        sql.push_str(if first_clause { " WHERE " } else { " AND " });
        sql.push_str(clause_sql);
        args.extend(clause_args.iter().cloned());
        first_clause = false;
    }
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

/// `Get` (`album_repository.go:174-179`): `GetAll` with
/// `Filters: Eq{"album.id": id}` — [`select_album_sql`] by id.
#[must_use]
pub fn get_sql(
    id: &str,
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
) -> Sql {
    select_album_sql(
        user_id,
        normalized_album_play_count,
        library_filter,
        &[("album.id = ?".to_string(), vec![Value::from(id)])],
        &[],
        0,
        0,
    )
}

/// `GetAll` (`album_repository.go:181-189`): [`select_album_sql`] with the
/// caller's order-by and pagination.
#[must_use]
pub fn get_all_sql(
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
    order_by: Vec<String>,
    max: i64,
    offset: i64,
) -> Sql {
    select_album_sql(
        user_id,
        normalized_album_play_count,
        library_filter,
        &[],
        &order_by,
        max,
        offset,
    )
}

/// `CountAll` (`album_repository.go:149-156`): `newSelect` + library filter +
/// the annotation join when the filters need one, then the base `count()`
/// shape (`sql_base_repository.go:565-576`: `count(distinct album.id) as
/// count`, pagination/order dropped). `filtersNeedAnnotation`
/// (`sql_annotations.go:55-61` — the regex scan) is the caller's;
/// `need_annotation` carries its result.
#[must_use]
pub fn count_all_sql(
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
    need_annotation: bool,
) -> Sql {
    let mut sql = format!("SELECT count(distinct {ALBUM_TABLE}.id) as count FROM {ALBUM_TABLE}");
    let mut args: Vec<Value> = Vec::new();
    if let Some((filter_sql, filter_args)) = library_filter {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
    }
    if need_annotation {
        if let Some(user) = user_id {
            sql.push_str(&format!(
                " LEFT JOIN {ANNOTATION_TABLE} ON ({ANNOTATION_TABLE}.item_id = album.id \
                 AND {ANNOTATION_TABLE}.item_type = ? AND {ANNOTATION_TABLE}.user_id = ?)"
            ));
            args.push(Value::from(ALBUM_TABLE));
            args.push(Value::from(user));
            let _ = normalized_album_play_count;
        }
    }
    (sql, args)
}

/// `Exists` (`album_repository.go:158-162`): the base count by id (Go counts
/// instead of `exists()` so the library filter applies).
#[must_use]
pub fn exists_sql(id: &str, library_filter: Option<Sql>) -> Sql {
    let mut sq = Repository::new(ALBUM_TABLE);
    if let Some((sql, args)) = library_filter {
        sq = sq.where_raw(sql, args);
    }
    sq.where_eq("album.id", id)
        .count(ALBUM_TABLE)
        .to_sql()
        .unwrap_or_default()
}

/// `albumFilters` (`album_repository.go:123-151`): the registered filter
/// map, fixed keys. The album-level tag filters (one per `AlbumLevelTags()`
/// name) and `role_<role>_id` (one per `AllRoles` role) are model-table-driven
/// — their membership lives with the model slice, so those keys drop here
/// (Go's `artistRoleFilter` invalid-role guard is `Gt{"": nil}`, unreachable
/// through the fixed map). Values bind as `?`.
#[must_use]
pub fn album_filter(field: &str, value: &str) -> Option<Filter> {
    let field = field.to_lowercase();
    let filter = match field.as_str() {
        // idFilter("album") (sql_restful.go:106-108).
        "id" => Filter::Eq {
            column: "album.id".to_string(),
            value: Value::from(value),
        },
        // name → fullTextFilter composes the search-strategy slice's
        // mbid/LIKE products; it is not substituted here (Go registers a
        // filterFunc the album lane does not own), so it drops.
        "compilation" | "missing" => boolean_filter(&field, value),
        // artistFilter → ParticipantIDFilter roles albumartist+artist (:137-138).
        "artist_id" => {
            let (sql, raw_args) =
                participant_id_filter(ALBUM_TABLE, value, &["albumartist", "artist"]);
            Filter::Raw { sql, raw_args }
        }
        // yearFilter (:142-148): Or{And{Gt,LtOrEq,GtOrEq}, Eq}.
        "year" => {
            let year = Value::Integer(value.parse::<i64>().unwrap_or_default());
            Filter::Raw {
                sql: "((min_year > ? AND min_year <= ? AND max_year >= ?) OR max_year = ?)"
                    .to_string(),
                raw_args: vec![Value::Integer(0), year.clone(), year.clone(), year],
            }
        }
        // recentlyPlayedFilter (:139-141).
        "recently_played" => Filter::Raw {
            sql: "play_count > ?".to_string(),
            raw_args: vec![Value::Integer(0)],
        },
        // annotationBoolFilter("starred"/"rating"/"play_count")
        // (sql_annotations.go:80-88).
        "starred" => Filter::Raw {
            sql: annotation_bool("starred", value),
            raw_args: Vec::new(),
        },
        "has_rating" => Filter::Raw {
            sql: annotation_bool("rating", value),
            raw_args: Vec::new(),
        },
        "played" => Filter::Raw {
            sql: annotation_bool("play_count", value),
            raw_args: Vec::new(),
        },
        // genreFilter(AlbumGenres) (sql_tags.go:92-99).
        "genre_id" => {
            let (sql, raw_args) = genre_id_filter(album_genres(), vec![value.to_string()]);
            Filter::Raw { sql, raw_args }
        }
        // allRolesFilter → ParticipantIDFilter with no roles (:149-150).
        "role_total_id" => {
            let (sql, raw_args) = participant_id_filter(ALBUM_TABLE, value, &[]);
            Filter::Raw { sql, raw_args }
        }
        // libraryIdFilter (sql_base_repository.go:256-258).
        "library_id" => Filter::Eq {
            column: "library_id".to_string(),
            value: Value::from(value),
        },
        _ => return None,
    };
    Some(filter)
}

/// `annotationBoolFilter` (`sql_annotations.go:80-88`), reached from three
/// registered album filters.
fn annotation_bool(field: &str, value: &str) -> String {
    crate::sql_annotations::annotation_bool_filter(field, value)
}

/// `model.Album`'s persisted row (`model/album.go:15-73`) — the 45 non-id
/// persisted album columns (the contract's 46 minus `id`, which the seam
/// forbids in the map; the id is appended last by [`put_sql`]).
///
/// Deliberately absent (entity write contract): the DDL orphan column
/// `releases` (no Go field, no Go writer — a put never emits it, so it stays
/// at its DDL default 0), `average_rating` (annotation-managed, never
/// written by entity Put), and the non-persisted fields (`LibraryPath`/
/// `LibraryName`/`Genres` are `structs:"-"` join-only; the `Annotations`/
/// `ItemImage` embeds carry no columns). The Go `RgAlbumGain`/`RgAlbumPeak`
/// dbx shims (`album_repository.go:37-38`) exist only because dbx's default
/// field-map cannot render `RGAlbumGain` to `rg_album_gain`; Rust binds the
/// real column names directly, no shim. `discs`/`participants`/`tags`/
/// `folder_ids` hold the *typed* values the Go shims shadow —
/// PostMapArgs overwrites their column keys with JSON, which
/// [`album_row`] routes through [`crate::marshal::album_post_map_args`].
///
/// Timestamps are `DateTime<Utc>` bound through `crate::time`. `created_at`/
/// `updated_at`/`external_info_updated_at` are nullable (Go zero
/// `time.Time` / `*time.Time` nil → NULL); `imported_at` is NOT NULL — Go
/// stamps `ImportedAt = Now` at `Put` entry (`album_repository.go:210`),
/// mirrored in [`put_sql`].
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRecord {
    /// `library_id` (Go `LibraryID`) — FK to `library`.
    pub library_id: i64,
    /// `name` (Go `Name`).
    pub name: String,
    /// `embed_art_path` (Go `EmbedArtPath`).
    pub embed_art_path: String,
    /// `album_artist` (Go `AlbumArtist`).
    pub album_artist: String,
    /// `album_artist_id` (Go `AlbumArtistID`, deprecated).
    pub album_artist_id: String,
    /// `min_year` (Go `MinYear`).
    pub min_year: i32,
    /// `max_year` (Go `MaxYear`).
    pub max_year: i32,
    /// `date` (Go `Date`).
    pub date: String,
    /// `min_original_year` (Go `MinOriginalYear`).
    pub min_original_year: i32,
    /// `max_original_year` (Go `MaxOriginalYear`).
    pub max_original_year: i32,
    /// `original_date` (Go `OriginalDate`).
    pub original_date: String,
    /// `release_date` (Go `ReleaseDate`).
    pub release_date: String,
    /// `compilation` (Go `Compilation`).
    pub compilation: bool,
    /// `comment` (Go `Comment`).
    pub comment: String,
    /// `song_count` (Go `SongCount`).
    pub song_count: i32,
    /// `duration` (Go `Duration`, `float32` at `model/album.go:41`). The DDL
    /// column is `real` either way; `f64` matches the column affinity (same
    /// precedent as `PlaylistRecord::duration`).
    pub duration: f64,
    /// `size` (Go `Size`).
    pub size: i64,
    /// `discs` (Go `Discs`, `map[int]string`). `None` (Go nil map) stores
    /// `null`; `Some(empty)` stores `{}`. Marshalled via
    /// `sql_json::marshal_discs` (string-key order `1`, `10`, `2`).
    pub discs: Option<BTreeMap<i64, String>>,
    /// `sort_album_name` (Go `SortAlbumName`).
    pub sort_album_name: String,
    /// `sort_album_artist_name` (Go `SortAlbumArtistName`).
    pub sort_album_artist_name: String,
    /// `order_album_name` (Go `OrderAlbumName`).
    pub order_album_name: String,
    /// `order_album_artist_name` (Go `OrderAlbumArtistName`).
    pub order_album_artist_name: String,
    /// `catalog_num` (Go `CatalogNum`).
    pub catalog_num: String,
    /// `mbz_album_id` (Go `MbZAlbumID`).
    pub mbz_album_id: String,
    /// `mbz_album_artist_id` (Go `MbZAlbumArtistID`).
    pub mbz_album_artist_id: String,
    /// `mbz_album_type` (Go `MbZAlbumType`).
    pub mbz_album_type: String,
    /// `mbz_album_comment` (Go `MbZAlbumComment`).
    pub mbz_album_comment: String,
    /// `mbz_release_group_id` (Go `MbZReleaseGroupID`).
    pub mbz_release_group_id: String,
    /// `folder_ids` (Go `FolderIDs []string`). `None` (Go nil) stores
    /// `null`; `Some(empty)` stores `[]`.
    pub folder_ids: Option<Vec<String>>,
    /// `explicit_status` (Go `ExplicitStatus`).
    pub explicit_status: String,
    /// `rg_album_gain` (Go `RGAlbumGain *float64`, read through the
    /// `RgAlbumGain` shim). `None` binds NULL.
    pub rg_album_gain: Option<f64>,
    /// `rg_album_peak` (Go `RGAlbumPeak *float64`). `None` binds NULL.
    pub rg_album_peak: Option<f64>,
    /// `description` (Go `Description`) — an external-metadata column.
    pub description: String,
    /// `small_image_url` (Go `SmallImageUrl`).
    pub small_image_url: String,
    /// `medium_image_url` (Go `MediumImageUrl`).
    pub medium_image_url: String,
    /// `large_image_url` (Go `LargeImageUrl`).
    pub large_image_url: String,
    /// `external_url` (Go `ExternalUrl`).
    pub external_url: String,
    /// `external_info_updated_at` (Go `ExternalInfoUpdatedAt *time.Time`).
    /// `None` binds NULL.
    pub external_info_updated_at: Option<DateTime<Utc>>,
    /// `genre` (Go `Genre` — the most common genre; a plain column distinct
    /// from the `Genres` tag set).
    pub genre: String,
    /// `tags` (Go `Tags`) — all imported tags; PostMapArgs marshals them to
    /// the `tags` JSON column and indexes them into `album_tags`.
    pub tags: DbTags,
    /// `participants` (Go `Participants`) — marshalled to the
    /// `participants` column and the `album_artists` junction rows.
    pub participants: Participants,
    /// FTS participant-name input (Go `Participants.AllNames()`), carried
    /// beside the JSON source so the FTS columns are derived from the same
    /// `model.Participants` the JSON is. S-D3 hydration fills both from one
    /// read; there is no write path that accepts a free-standing name list.
    pub participant_names: Vec<NamedParticipant>,
    /// `missing` (Go `Missing`).
    pub missing: bool,
    /// `imported_at` (Go `ImportedAt`) — required: NOT NULL in the DDL, and
    /// Go stamps it to Now at `Put` entry (`:210`), which [`put_sql`] does.
    pub imported_at: DateTime<Utc>,
    /// `created_at` (Go `CreatedAt`); `None` binds NULL.
    pub created_at: Option<DateTime<Utc>>,
    /// `updated_at` (Go `UpdatedAt`); `None` binds NULL. Passed through
    /// unchanged by [`put_sql`] (Go `Put` never touches it,
    /// `album_repository.go:208-219`).
    pub updated_at: Option<DateTime<Utc>>,
}

/// One artist name input for the FTS participant columns — Go's
/// `Participants.AllNames()` (`model/participants.go:174-185`) reads
/// `artist.Name` plus `artist.SortArtistName` when non-empty, and NEVER
/// `SubRole`. The persisted `participants` JSON stays the `{id, name, subRole}`
/// shape (`sql_participations.go:13-17`) — `sort_name` is input-only FTS
/// metadata that `sql_participations::Participant` cannot carry (it is
/// hydration-owned) without changing that file, so the records carry it
/// here.
///
/// **S-D2b consistency:** the record holds the `participants` JSON source
/// and its `NamedParticipant` view side by side — hydration/S-D3 or the
/// caller fills both from the same `model.Participants`, and there is no
/// write path that accepts a free-standing name list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedParticipant {
    /// The artist id (`model.Artist.ID`) — matches the JSON entry.
    pub id: String,
    /// The artist name (`model.Artist.Name`) — always contributes.
    pub name: String,
    /// The artist sort name (`model.Artist.SortArtistName`); contributes
    /// only when non-empty (Go `if artist.SortArtistName != ""`).
    pub sort_name: String,
    /// `SubRole` — deliberately NOT a name source (Go `AllNames` ignores
    /// it); kept on the type so a wrong construction cannot smuggle a
    /// sub-role into the FTS input.
    pub sub_role: String,
}

/// `Participants.AllNames()` (`model/participants.go:174-185`): every
/// participant's name plus its non-empty sort name, deduplicated (Go's
/// `slice.Unique` — order-preserving dedup). The FTS consumers
/// (`format_full_text`/`search_participants`) are insensitive to the order
/// within the list, so input order stands in for Go's unspecified
/// map-iteration order. `sub_role` is never a name source.
#[must_use]
pub fn participant_names(names: &[NamedParticipant]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in names {
        if !out.contains(&p.name) {
            out.push(p.name.clone());
        }
        if !p.sort_name.is_empty() && !out.contains(&p.sort_name) {
            out.push(p.sort_name.clone());
        }
    }
    out
}
/// The album-version tag name (`model.TagAlbumVersion`, `model/tag.go:197`).
const TAG_ALBUM_VERSION: &str = "albumversion";
/// The catalog-number tag name (`model.TagCatalogNumber`, `model/tag.go:206`).
const TAG_CATALOG_NUMBER: &str = "catalognumber";

/// `structs.Map(dbAlbum)` + `dbAlbum.PostMapArgs` (`album_repository.go:71-97`)
/// as one sorted [`crate::marshal::ColumnMap`]: exactly the 45 non-id
/// persisted album columns (`id` is appended last by [`put_sql`]), with the
/// PostMapArgs overwrites routed through the S-D2a hook so the
/// transformation lives in one place.
///
/// **S-D2b consistency invariant:** the FTS inputs are derived *atomically*
/// from the record's own `participants`/`discs`/`tags` (see
/// [`album_derived`]) — the derived values and the JSON/junction values
/// share one source, so they can never contradict each other.
#[must_use]
pub fn album_row(a: &AlbumRecord) -> crate::marshal::ColumnMap {
    let mut map = BTreeMap::new();
    map.insert(
        "album_artist".to_string(),
        Value::Text(a.album_artist.clone()),
    );
    map.insert(
        "album_artist_id".to_string(),
        Value::Text(a.album_artist_id.clone()),
    );
    map.insert(
        "catalog_num".to_string(),
        Value::Text(a.catalog_num.clone()),
    );
    map.insert("comment".to_string(), Value::Text(a.comment.clone()));
    map.insert("compilation".to_string(), Value::Bool(a.compilation));
    map.insert(
        "created_at".to_string(),
        crate::time::optional_value(a.created_at.as_ref()),
    );
    map.insert("date".to_string(), Value::Text(a.date.clone()));
    map.insert(
        "description".to_string(),
        Value::Text(a.description.clone()),
    );
    // `discs`/`participants`/`tags`/`folder_ids` land as typed values and
    // are overwritten with their JSON by PostMapArgs below.
    map.insert("discs".to_string(), Value::Null);
    map.insert("duration".to_string(), Value::Real(a.duration));
    map.insert(
        "embed_art_path".to_string(),
        Value::Text(a.embed_art_path.clone()),
    );
    map.insert(
        "explicit_status".to_string(),
        Value::Text(a.explicit_status.clone()),
    );
    map.insert(
        "external_info_updated_at".to_string(),
        crate::time::optional_value(a.external_info_updated_at.as_ref()),
    );
    map.insert(
        "external_url".to_string(),
        Value::Text(a.external_url.clone()),
    );
    map.insert("folder_ids".to_string(), Value::Null);
    map.insert("genre".to_string(), Value::Text(a.genre.clone()));
    map.insert(
        "imported_at".to_string(),
        crate::time::value(&a.imported_at),
    );
    map.insert(
        "large_image_url".to_string(),
        Value::Text(a.large_image_url.clone()),
    );
    map.insert("library_id".to_string(), Value::Integer(a.library_id));
    map.insert(
        "medium_image_url".to_string(),
        Value::Text(a.medium_image_url.clone()),
    );
    map.insert(
        "min_original_year".to_string(),
        Value::Integer(i64::from(a.min_original_year)),
    );
    map.insert(
        "min_year".to_string(),
        Value::Integer(i64::from(a.min_year)),
    );
    map.insert(
        "max_original_year".to_string(),
        Value::Integer(i64::from(a.max_original_year)),
    );
    map.insert(
        "max_year".to_string(),
        Value::Integer(i64::from(a.max_year)),
    );
    map.insert(
        "mbz_album_id".to_string(),
        Value::Text(a.mbz_album_id.clone()),
    );
    map.insert(
        "mbz_album_artist_id".to_string(),
        Value::Text(a.mbz_album_artist_id.clone()),
    );
    map.insert(
        "mbz_album_type".to_string(),
        Value::Text(a.mbz_album_type.clone()),
    );
    map.insert(
        "mbz_album_comment".to_string(),
        Value::Text(a.mbz_album_comment.clone()),
    );
    map.insert(
        "mbz_release_group_id".to_string(),
        Value::Text(a.mbz_release_group_id.clone()),
    );
    map.insert("missing".to_string(), Value::Bool(a.missing));
    map.insert("name".to_string(), Value::Text(a.name.clone()));
    map.insert(
        "order_album_name".to_string(),
        Value::Text(a.order_album_name.clone()),
    );
    map.insert(
        "order_album_artist_name".to_string(),
        Value::Text(a.order_album_artist_name.clone()),
    );
    map.insert(
        "original_date".to_string(),
        Value::Text(a.original_date.clone()),
    );
    map.insert("participants".to_string(), Value::Null);
    map.insert(
        "release_date".to_string(),
        Value::Text(a.release_date.clone()),
    );
    map.insert(
        "rg_album_gain".to_string(),
        a.rg_album_gain.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert(
        "rg_album_peak".to_string(),
        a.rg_album_peak.map(Value::Real).unwrap_or(Value::Null),
    );
    map.insert("size".to_string(), Value::Integer(a.size));
    map.insert(
        "small_image_url".to_string(),
        Value::Text(a.small_image_url.clone()),
    );
    map.insert(
        "song_count".to_string(),
        Value::Integer(i64::from(a.song_count)),
    );
    map.insert(
        "sort_album_name".to_string(),
        Value::Text(a.sort_album_name.clone()),
    );
    map.insert(
        "sort_album_artist_name".to_string(),
        Value::Text(a.sort_album_artist_name.clone()),
    );
    map.insert("tags".to_string(), Value::Null);
    map.insert(
        "updated_at".to_string(),
        crate::time::optional_value(a.updated_at.as_ref()),
    );
    crate::marshal::album_post_map_args(crate::marshal::ColumnMap::new(map), album_derived(a)).0
}

/// `AlbumDerived` computed atomically from the record — the S-D2b
/// consistency invariant made type-level: there is no constructor that takes
/// a free-standing `participant_names`/`album_version`/`catalog_number`, so
/// the FTS inputs cannot diverge from the stored JSON/junction data.
/// `disc_values` uses the BTreeMap's sorted key order; Go's
/// `maps.Values` order is non-deterministic and irrelevant (`format_full_text`
/// sorts its words).
#[must_use]
fn album_derived(a: &AlbumRecord) -> crate::marshal::AlbumDerived {
    crate::marshal::AlbumDerived {
        name: a.name.clone(),
        sort_album_name: a.sort_album_name.clone(),
        album_artist: a.album_artist.clone(),
        participant_names: participant_names(&a.participant_names),
        disc_values: a
            .discs
            .as_ref()
            .map(|d| d.values().cloned().collect())
            .unwrap_or_default(),
        album_version: a.tags.get(TAG_ALBUM_VERSION).cloned().unwrap_or_default(),
        catalog_number: a.tags.get(TAG_CATALOG_NUMBER).cloned().unwrap_or_default(),
        tags: a.tags.clone(),
        participants: a.participants.clone(),
        folder_ids: a.folder_ids.clone(),
        discs: a.discs.clone(),
    }
}

/// `Put` (`album_repository.go:164-172`), record-driven (S-D2b): the record
/// is the only write input — no caller-supplied column arrays.
///
/// `now` stamps the row exactly like Go's `Put`
/// (`album_repository.go:208-219`): `imported_at` to `now`
/// (`al.ImportedAt = time.Now()`, `:210`); `created_at` and `updated_at`
/// pass through the record unchanged — Go stamps neither in `Put`.
/// `created_at` is still dropped from the UPDATE set by the seam.
///
/// **Consistency invariant:** the PostMapArgs hook is fed the FTS inputs
/// derived from the record's own `participants`/`discs`/`tags"
/// ([`album_derived`]) — the same values the JSON columns and the
/// `album_artists`/`album_tags` junction maintenance are built from — so the
/// FTS columns can never contradict the stored JSON.
///
/// Both legs render through the shared [`crate::marshal::MarshalledRow`]:
/// UPDATE = the `selectUpdateColumns`-filtered set (no `created_at`; `id"
/// binds last via `WHERE id = ?`), INSERT = the full sorted columns with
/// `id` last in both lists (`sql_base_repository.go:637-641`). Go's `put"
/// runs the UPDATE first and only INSERTs on a 0-row match; the caller
/// picks the leg.
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
/// (impossible from [`album_row`]) or the UPDATE set is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct PutAlbumSql {
    /// The `r.put` update leg (filtered columns, `id` last in args).
    pub update: Sql,
    /// The `r.put` insert leg (sorted columns, `id` last in both).
    pub insert: Sql,
    /// `Delete(album_artists) WHERE album_id = ?`.
    pub participants_delete: Sql,
    /// The `updateParticipants` json_each INSERT
    /// (`sql_participations.go:102-124`), built by
    /// [`crate::sql_participations::update_participants_sql`] from the
    /// record's participants. `None` when the album has no participants
    /// (Go returns before building the insert, `:87-89`) — the delete still
    /// runs, so a put with empty participants clears the table.
    pub participants_insert: Option<Sql>,
    /// `Delete(album_tags) WHERE album_id = ?`.
    pub tags_delete: Sql,
    /// The `updateTags` scoped INSERT, absent when no indexed tag ids.
    pub tags_insert: Option<Sql>,
}

/// See [`PutAlbumSql`].
pub fn put_sql(
    id: &str,
    a: &AlbumRecord,
    now: DateTime<Utc>,
    cols_to_update: &[&str],
) -> Result<PutAlbumSql, crate::marshal::MarshalError> {
    // Go's Put (album_repository.go:208-219): only ImportedAt is stamped;
    // CreatedAt/UpdatedAt pass through the record unchanged.
    let stamped = AlbumRecord {
        imported_at: now,
        ..a.clone()
    };
    let (map, maintenance) =
        crate::marshal::album_post_map_args(album_row(&stamped), album_derived(&stamped));
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
    maintenance: &crate::marshal::AlbumMaintenance,
    cols_to_update: &[&str],
) -> Result<PutAlbumSql, crate::marshal::MarshalError> {
    let row = crate::marshal::MarshalledRow::new(ALBUM_TABLE, id, map, cols_to_update)?;
    let up = crate::sql_participations::update_participants_sql(
        ALBUM_TABLE.to_string(),
        id.to_string(),
        &maintenance.participants,
    );
    Ok(PutAlbumSql {
        update: (row.update_sql, row.update_args),
        insert: (row.insert_sql, row.insert_args),
        participants_delete: up.delete,
        participants_insert: up.insert,
        tags_delete: delete_tags_sql(ALBUM_TABLE.to_string(), id.to_string()),
        tags_insert: insert_tags_sql(
            ALBUM_TABLE.to_string(),
            id.to_string(),
            maintenance.indexed_tag_ids.clone(),
        ),
    })
}
/// Thin alias for [`put_from_map`] retained so the S-D2a `put_marshalled_sql`
/// call sites in `tests/runtime_marshal.rs` keep compiling; there is exactly
/// one put-result type per repo (no `PutAlbumMarshalled`).
///
/// # Errors
///
/// As [`put_from_map`].
pub fn put_marshalled_sql(
    id: &str,
    map: &crate::marshal::ColumnMap,
    maintenance: &crate::marshal::AlbumMaintenance,
    cols_to_update: &[&str],
) -> Result<PutAlbumSql, crate::marshal::MarshalError> {
    put_from_map(id, map, maintenance, cols_to_update)
}

/// `UpdateExternalInfo` (`album_repository.go:223`) — the six external-
/// metadata columns, the only columns the restricted update may touch.
pub const EXTERNAL_INFO_COLS: [&str; 6] = [
    "description",
    "small_image_url",
    "medium_image_url",
    "large_image_url",
    "external_url",
    "external_info_updated_at",
];

/// `UpdateExternalInfo` (`album_repository.go:223-225`): `r.put` restricted
/// to [`EXTERNAL_INFO_COLS`] — record-driven; the UPDATE leg sets only the
/// six external columns, the INSERT leg is the full row (Go's `put` is
/// full-column on the insert leg regardless of `colsToUpdate`,
/// `sql_base_repository.go:637-641`).
///
/// # Errors
///
/// As [`put_sql`].
pub fn update_external_info_sql(
    id: &str,
    a: &AlbumRecord,
    now: DateTime<Utc>,
) -> Result<PutAlbumSql, crate::marshal::MarshalError> {
    put_sql(id, a, now, &EXTERNAL_INFO_COLS)
}

/// `Touch` (`album_repository.go:258-270`): one `imported_at = ?` UPDATE per
/// 200-id chunk (`slices.Chunk(ids, 200)`, `:264`).
#[must_use]
pub fn touch_sql(ids: &[String], imported_at: i64) -> Vec<Sql> {
    const CHUNK: usize = 200;
    ids.chunks(CHUNK)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![Value::Integer(imported_at)];
            args.extend(chunk.iter().map(|id| Value::from(id.as_str())));
            (
                format!("UPDATE {ALBUM_TABLE} SET imported_at = ? WHERE id IN ({placeholders})"),
                args,
            )
        })
        .collect()
}

/// `TouchByMissingFolder` (`album_repository.go:273-286`): the one UPDATE
/// whose EXISTS subquery walks `json_each(folder_ids)` into `folder` for
/// missing folders. `imported_at` binds first.
#[must_use]
pub fn touch_by_missing_folder_sql(imported_at: i64) -> Sql {
    (
        format!(
            "UPDATE {ALBUM_TABLE} SET imported_at = ? WHERE folder_ids IS NOT NULL AND \
             EXISTS (SELECT 1 FROM json_each(folder_ids) AS je \
             JOIN main.folder AS f ON je.value = f.id WHERE f.missing = ?)"
        ),
        vec![Value::Integer(imported_at), Value::Bool(true)],
    )
}

/// `GetYears` (`album_repository.go:229-241`): the distinct non-missing
/// `max_year` values, optionally scoped to libraries, ordered by
/// `max_year`. Go folds `ErrNotFound` into empty.
#[must_use]
pub fn get_years_sql(library_ids: &[i64], library_filter: Option<Sql>) -> Sql {
    let mut sq = Repository::new(ALBUM_TABLE)
        .select(["distinct max_year"])
        .where_raw("max_year > ?", vec![Value::Integer(0)])
        .where_eq("missing", false)
        .order_by(["max_year"]);
    if !library_ids.is_empty() {
        let placeholders = vec!["?"; library_ids.len()].join(", ");
        sq = sq.where_raw(
            format!("library_id IN ({placeholders})"),
            library_ids.iter().map(|id| Value::Integer(*id)).collect(),
        );
    }
    if let Some((sql, args)) = library_filter {
        sq = sq.where_raw(sql, args);
    }
    sq.to_sql().unwrap_or_default()
}

/// `purgeEmpty` (`album_repository.go:347-362`): one DELETE, optionally
/// scoped to libraries.
#[must_use]
pub fn purge_empty_sql(library_ids: &[i64]) -> Sql {
    let mut sql = format!(
        "DELETE FROM {ALBUM_TABLE} WHERE id not in \
         (select distinct(album_id) from media_file)"
    );
    let mut args = Vec::new();
    if !library_ids.is_empty() {
        let placeholders = vec!["?"; library_ids.len()].join(", ");
        sql.push_str(&format!(" AND library_id IN ({placeholders})"));
        args.extend(library_ids.iter().map(|id| Value::Integer(*id)));
    }
    (sql, args)
}

/// `CopyAttributes` (`album_repository.go:243-256`): the read of the source
/// row cast to text (`go-sqlite3` re-formats datetimes otherwise), then the
/// SET-map UPDATE onto the target id. The `created_at` poison guard
/// (`:250-254`) is the caller's fold over the read row; `columns` are
/// caller-supplied column names (identifier safety rule).
#[must_use]
pub fn copy_attributes_read_sql(from_id: &str, columns: &[String]) -> Sql {
    let cols = columns
        .iter()
        .map(|c| format!("cast({c} as text) as {c}"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!("SELECT {cols} FROM {ALBUM_TABLE} WHERE id = ?"),
        vec![Value::from(from_id)],
    )
}

/// See [`copy_attributes_read_sql`]; `values` are the read row's text values
/// for exactly `columns`, in order.
#[must_use]
pub fn copy_attributes_update_sql(to_id: &str, columns: &[String], values: Vec<Value>) -> Sql {
    let set_pairs = columns
        .iter()
        .map(|c| format!("{c} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut args = values;
    args.push(Value::from(to_id));
    (
        format!("UPDATE {ALBUM_TABLE} SET {set_pairs} WHERE id = ?"),
        args,
    )
}

/// `RefreshPlayCounts` (`album_repository.go:326-344`): Go's literal CTE
/// INSERT … ON CONFLICT upsert; the table names are compile-time constants,
/// no user text reaches it.
#[must_use]
pub fn refresh_play_counts_sql() -> Sql {
    (
        "\nwith play_counts as (\n    select user_id, album_id, \
         sum(play_count) as total_play_count, max(play_date) as last_play_date\n    \
         from media_file\n             join annotation on item_id = media_file.id\n    \
         group by user_id, album_id\n)\ninsert into annotation \
         (user_id, item_id, item_type, play_count, play_date)\nselect \
         user_id, album_id, 'album', total_play_count, last_play_date\nfrom \
         play_counts\nwhere total_play_count > 0\non conflict (user_id, item_id, \
         item_type) do update\n    set play_count = excluded.play_count,\n        \
         play_date  = excluded.play_date;\n"
            .to_string(),
        Vec::new(),
    )
}

/// `soleAlbumArtistFilter` (`album_repository.go:198-200`): the
/// `json_array_length(participants, '$.albumartist') = 1` fragment Go shares
/// between `SoleAlbumArtistFilter` and `GetSoleAlbumArtistIDsInSubtrees`.
pub const SOLE_ALBUM_ARTIST_FILTER: &str = "json_array_length(participants, '$.albumartist') = 1";

/// `SoleAlbumArtistFilter` (`album_repository.go:202-206`): the album-artist
/// semi-join AND the sole-artist fragment.
#[must_use]
pub fn sole_album_artist_filter_sql(artist_id: &str) -> Sql {
    let (sql, args) = participant_id_filter(ALBUM_TABLE, artist_id, &["albumartist"]);
    (format!("({sql} AND {SOLE_ALBUM_ARTIST_FILTER})"), args)
}

/// `GetSoleAlbumArtistIDsInSubtrees` (`album_repository.go:213-229`): the
/// chunked `json_each(album.folder_ids)` EXISTS semi-join onto `folder`
/// through `folderSubtreeFilter` (`folder_repository.go:210-227` →
/// `crate::folder_repo::folder_subtree_filter`), selecting the one
/// `albumartist[0].id`. Repeated ids across chunks are fine (the queue
/// upserts by PK, `:218`); `paths` chunks at 100
/// (`subtreePathChunkSize`, `folder_repository.go:204`).
#[must_use]
pub fn get_sole_album_artist_ids_in_subtrees_sql(lib_id: i64, paths: &[String]) -> Vec<Sql> {
    const SUBTREE_PATH_CHUNK_SIZE: usize = 100;
    let mut result = Vec::new();
    for chunk in paths.chunks(SUBTREE_PATH_CHUNK_SIZE) {
        let (subtree_sql, subtree_args) = crate::folder_repo::folder_subtree_filter(lib_id, chunk);
        let mut args = subtree_args;
        args.push(Value::Bool(false));
        result.push((
            format!(
                "SELECT distinct json_extract(participants, '$.albumartist[0].id') \
                 FROM {ALBUM_TABLE} WHERE {SOLE_ALBUM_ARTIST_FILTER} AND \
                 EXISTS (SELECT 1 FROM json_each(album.folder_ids) je \
                 join folder on folder.id = je.value WHERE {subtree_sql}) \
                 AND album.missing = ?"
            ),
            args,
        ));
    }
    result
}

/// `GetTouchedAlbums` (`album_repository.go:296-304`): the album select with
/// the touched-since-last-scan restriction. Go wraps it in
/// `queryWithStableResults` (the streaming slice); the pair pins here.
#[must_use]
pub fn get_touched_albums_sql(
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
    lib_id: i64,
) -> Sql {
    select_album_sql(
        user_id,
        normalized_album_play_count,
        library_filter,
        &[
            ("library.id = ?".to_string(), vec![Value::Integer(lib_id)]),
            (
                "album.imported_at > library.last_scan_at".to_string(),
                Vec::new(),
            ),
        ],
        &[],
        0,
        0,
    )
}

/// `getAllIDs` (`album_repository.go:196-206`): the id-only projection
/// (`album.id`) with the library filter and the annotation LEFT JOIN when
/// the filters need one (`filtersNeedAnnotation`, `sql_annotations.go:55-61`,
/// is the caller's regex scan — `need_annotation` carries its result).
#[must_use]
pub fn get_all_ids_sql(
    library_filter: Option<Sql>,
    need_annotation: bool,
    user_id: Option<&str>,
    normalized_album_play_count: bool,
) -> Sql {
    let mut sql = format!("SELECT album.id FROM {ALBUM_TABLE}");
    let mut args: Vec<Value> = Vec::new();
    if need_annotation {
        if let Some(user) = user_id {
            sql.push_str(&format!(
                " LEFT JOIN {ANNOTATION_TABLE} ON ({ANNOTATION_TABLE}.item_id = album.id \
                 AND {ANNOTATION_TABLE}.item_type = ? AND {ANNOTATION_TABLE}.user_id = ?)"
            ));
            args.push(Value::from(ALBUM_TABLE));
            args.push(Value::from(user));
            let _ = normalized_album_play_count;
        }
    }
    if let Some((filter_sql, filter_args)) = library_filter {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args);
    }
    (sql, args)
}

/// `GetCursor`'s per-chunk re-query (`chunkOptions` + `streamByIDs`,
/// `persistence/artwork_hydration.go:41-78`): Go re-queries `GetAll` with an
/// `album.id IN (…)` filter per id chunk; that re-query is this module's
/// [`select_album_sql`] with the id IN-list as its WHERE clause. The cursor
/// wiring itself is the streaming slice's.
#[must_use]
pub fn cursor_chunk_sql(
    ids: &[String],
    user_id: Option<&str>,
    normalized_album_play_count: bool,
    library_filter: Option<Sql>,
    order_by: &[String],
    max: i64,
    offset: i64,
) -> Sql {
    let placeholders = vec!["?"; ids.len()].join(", ");
    let id_args: Vec<Value> = ids.iter().map(|id| Value::from(id.as_str())).collect();
    select_album_sql(
        user_id,
        normalized_album_play_count,
        library_filter,
        &[(format!("album.id IN ({placeholders})"), id_args)],
        order_by,
        max,
        offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_participations::{Participant, Role};

    fn lib_filter() -> Option<Sql> {
        Some((
            "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("user1")],
        ))
    }

    #[test]
    fn sort_mappings_pin_the_registered_table() {
        // Arrange: setSortMappings (:134-146), plain max_year form,
        // created_at recently_added (RecentlyAddedByModTime off).
        // Act:
        let m = sort_mappings(NATURAL_SORT_ALBUM_NAME, false);
        // Assert:
        assert_eq!(
            m.get("name").map(String::as_str),
            Some("order_album_name, order_album_artist_name")
        );
        assert_eq!(
            m.get("max_year").map(String::as_str),
            Some(
                "coalesce(nullif(original_date,''), cast(max_year as text)), \
                 release_date, album.name"
            )
        );
        assert_eq!(
            m.get("recently_added").map(String::as_str),
            Some("album.created_at, album.id")
        );
        let mod_time = sort_mappings(NATURAL_SORT_ALBUM_NAME, true);
        assert_eq!(
            mod_time.get("recently_added").map(String::as_str),
            Some("album.updated_at, album.id")
        );
    }

    #[test]
    fn select_album_joins_library_left_and_annotation() {
        // Arrange: selectAlbum (:166-172): library LEFT join then
        // withAnnotation (sql_annotations.go:63-77); join args bind first.
        // Act:
        let (sql, args) = select_album_sql(Some("u1"), false, None, &[], &[], 0, 0);
        // Assert:
        assert_eq!(
            sql,
            "SELECT album.*, library.path as library_path, library.name as library_name, \
             coalesce(starred, 0) as starred, coalesce(rating, 0) as rating, starred_at, \
             play_date, rated_at, coalesce(play_count, 0) as play_count, album.average_rating \
             FROM album LEFT JOIN library ON library.id = album.library_id \
             LEFT JOIN annotation ON (annotation.item_id = album.id \
             AND annotation.item_type = ? AND annotation.user_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
            ]
        );
        // No user: only the bare average_rating column (:63-65).
        let (bare_sql, bare_args) = select_album_sql(None, false, None, &[], &[], 0, 0);
        assert_eq!(
            bare_sql,
            "SELECT album.*, library.path as library_path, library.name as library_name, \
             album.average_rating FROM album \
             LEFT JOIN library ON library.id = album.library_id"
        );
        assert!(bare_args.is_empty());
    }

    #[test]
    fn get_sql_filters_by_bound_album_id() {
        // Arrange: Get → GetAll with Eq{"album.id"} (:174-179).
        // Act:
        let (sql, args) = get_sql("al-1", Some("u1"), false, lib_filter());
        // Assert: the join args bind before the WHERE args (join position).
        assert!(sql.contains("LEFT JOIN annotation ON"));
        assert!(sql.ends_with(
            "WHERE album.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?) AND album.id = ?"
        ));
        assert_eq!(
            args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("user1".to_string()),
                Value::Text("al-1".to_string()),
            ]
        );
    }

    #[test]
    fn count_all_pins_distinct_id_count() {
        // Arrange: CountAll → count() (sql_base_repository.go:565-576).
        // Act:
        let (sql, args) = count_all_sql(None, false, lib_filter(), false);
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct album.id) as count FROM album WHERE \
             album.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?)"
        );
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
        // The annotation need adds the LEFT JOIN (Go: withAnnotation).
        let (joined, joined_args) = count_all_sql(Some("u1"), false, None, true);
        assert!(joined.contains("LEFT JOIN annotation ON (annotation.item_id = album.id"));
        assert_eq!(
            joined_args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
            ]
        );
    }

    #[test]
    fn exists_counts_by_id_with_library_filter() {
        // Arrange: Exists (:158-162) — count, not exists(), so the library
        // filter applies.
        // Act:
        let (sql, args) = exists_sql("al-1", lib_filter());
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct album.id) as count FROM album \
             WHERE album.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?) AND album.id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Text("al-1".to_string()),
            ]
        );
    }

    #[test]
    fn search_empty_query_pins_the_rowid_page() {
        // Arrange: Search → doSearch (:441-447) with the empty-query branch
        // (sql_search.go:47-50): natural-order rowid page. `sq` arrives via
        // sql_search::new_select (applyOptions' pagination, :36).
        let options = crate::sql_restful::QueryOptions {
            max: 20,
            ..crate::sql_restful::QueryOptions::default()
        };
        let sq = crate::sql_search::new_select(ALBUM_TABLE, Some(&options));
        // Act:
        let outcome = search_sql(sq, "", Some(&options), lib_filter(), None).expect("renders");
        // Assert:
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("empty query must produce a query");
        };
        assert!(sql.starts_with(
            "SELECT * FROM album INNER JOIN (SELECT rowid as _rid, \
             row_number() OVER () AS _rn FROM (SELECT album.rowid FROM album \
             WHERE album.missing = ?"
        ));
        assert_eq!(args[0], Value::Bool(false));
        assert_eq!(args[1], Value::Text("user1".to_string()));
        // Assert: the page bound is a literal in the rowid subquery, so it is
        // not part of args (`select.go:139-147`). The only remaining third arg
        // is Phase 2's own `missing = ?` restriction.
        assert!(sql.contains("ORDER BY album.rowid LIMIT 20)"));
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("user1".to_string()),
                Value::Bool(false)
            ]
        );
    }

    #[test]
    fn search_mbid_query_binds_per_field() {
        // Arrange: doSearch's UUID branch (sql_search.go:54-57) over
        // albumSearchConfig's MBID fields.
        let sq = crate::sql_search::new_select(ALBUM_TABLE, None);
        // Act:
        let outcome = search_sql(sq, "F76D3C25-3B6B-4C61-83C7-8E0A0B6D0A47", None, None, None)
            .expect("renders");
        // Assert:
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("UUID query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT * FROM album WHERE album.missing = ? AND \
             (album.mbz_album_id = ? OR album.mbz_release_group_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47".to_string()),
                Value::Text("f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47".to_string()),
            ]
        );
    }

    #[test]
    fn get_search_strategy_follows_config_order() {
        // Arrange: getSearchStrategy (sql_search.go:36-49), config-ordered.
        // Act/Assert: legacy backend / full-string first.
        assert!(get_search_strategy("beatles", true, false).is_some());
        assert!(get_search_strategy("beatles", false, true).is_some());
        // CJK → core-column LIKE; FTS otherwise; punctuation-only → None.
        assert!(get_search_strategy("音乐", false, false).is_some());
        assert!(get_search_strategy("beatles", false, false).is_some());
        assert!(get_search_strategy("", false, false).is_none());
    }

    #[test]
    fn album_filter_pins_the_fixed_key_shapes() {
        // Arrange: albumFilters (:123-151) fixed keys.
        // Act/Assert: id (idFilter).
        let Filter::Eq { column, value } = album_filter("id", "al-1").expect("id") else {
            panic!("id filter is Eq");
        };
        assert_eq!(column, "album.id");
        assert_eq!(value, Value::Text("al-1".to_string()));
        // year: Or{And{Gt,LtOrEq,GtOrEq}, Eq} (:142-148).
        let Filter::Raw { sql, raw_args } = album_filter("year", "1969").expect("year") else {
            panic!("year filter is Raw");
        };
        assert_eq!(
            sql,
            "((min_year > ? AND min_year <= ? AND max_year >= ?) OR max_year = ?)"
        );
        assert_eq!(
            raw_args,
            vec![
                Value::Integer(0),
                Value::Integer(1969),
                Value::Integer(1969),
                Value::Integer(1969),
            ]
        );
        // recentlyPlayedFilter (:139-141).
        let Filter::Raw { sql, raw_args } =
            album_filter("recently_played", "true").expect("recently_played")
        else {
            panic!("recently_played filter is Raw");
        };
        assert_eq!(sql, "play_count > ?");
        assert_eq!(raw_args, vec![Value::Integer(0)]);
        // starred → annotationBoolFilter (sql_annotations.go:80-88).
        let Filter::Raw { sql, .. } = album_filter("starred", "true").expect("starred") else {
            panic!("starred filter is Raw");
        };
        assert_eq!(sql, "COALESCE(starred, 0) > 0");
        // library_id: Eq (sql_base_repository.go:256-258).
        let Filter::Eq { column, .. } = album_filter("library_id", "3").expect("library_id") else {
            panic!("library_id filter is Eq");
        };
        assert_eq!(column, "library_id");
        // artist_id: ParticipantIDFilter roles albumartist+artist (:137-138).
        let Filter::Raw { sql, raw_args } = album_filter("artist_id", "ar-1").expect("artist_id")
        else {
            panic!("artist_id filter is Raw");
        };
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_artists \
             WHERE artist_id = ? AND role IN (?, ?))"
        );
        assert_eq!(
            raw_args,
            vec![
                Value::Text("ar-1".to_string()),
                Value::Text("albumartist".to_string()),
                Value::Text("artist".to_string()),
            ]
        );
        // genre_id → genreFilter(AlbumGenres) semi-join (sql_tags.go:92-99).
        let Filter::Raw { sql, raw_args } = album_filter("genre_id", "tg-1").expect("genre_id")
        else {
            panic!("genre_id filter is Raw");
        };
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_tags WHERE tag_id IN (?))"
        );
        assert_eq!(raw_args, vec![Value::Text("tg-1".to_string())]);
        // compilation → booleanFilter (sql_restful.go:102-105).
        assert_eq!(
            album_filter("compilation", "TRUE").expect("compilation"),
            Filter::Eq {
                column: "compilation".to_string(),
                value: Value::Bool(true),
            }
        );
        // Unregistered keys drop (Go nil filter).
        assert!(album_filter("bogus", "x").is_none());
        assert!(album_filter("name", "x").is_none());
        assert!(album_filter("role_lead_id", "x").is_none());
    }

    fn test_record() -> AlbumRecord {
        let mut participants = Participants::new();
        participants.insert(
            Role::from("albumartist"),
            vec![Participant {
                id: "ar-1".to_string(),
                name: "Beatles".to_string(),
                sub_role: String::new(),
            }],
        );
        AlbumRecord {
            library_id: 7,
            album_artist: "The Beatles".to_string(),
            album_artist_id: String::new(),
            catalog_num: "CAT-1".to_string(),
            comment: "c".to_string(),
            compilation: false,
            created_at: None,
            date: "".to_string(),
            description: "".to_string(),
            discs: None,
            duration: 0.0,
            embed_art_path: String::new(),
            explicit_status: "".to_string(),
            external_info_updated_at: None,
            external_url: String::new(),
            folder_ids: None,
            genre: String::new(),
            imported_at: chrono::Utc::now(),
            large_image_url: String::new(),
            medium_image_url: String::new(),
            min_original_year: 0,
            min_year: 0,
            max_original_year: 0,
            max_year: 0,
            mbz_album_id: String::new(),
            mbz_album_artist_id: String::new(),
            mbz_album_type: String::new(),
            mbz_album_comment: String::new(),
            mbz_release_group_id: String::new(),
            missing: false,
            name: "Abbey Road".to_string(),
            order_album_name: String::new(),
            order_album_artist_name: String::new(),
            original_date: String::new(),
            release_date: String::new(),
            rg_album_gain: None,
            rg_album_peak: None,
            size: 0,
            small_image_url: String::new(),
            song_count: 0,
            sort_album_name: String::new(),
            sort_album_artist_name: String::new(),
            tags: crate::sql_tags::DbTags::new(),
            participants,
            participant_names: vec![NamedParticipant {
                id: "ar-1".to_string(),
                name: "Beatles".to_string(),
                sort_name: String::new(),
                sub_role: String::new(),
            }],
            updated_at: None,
        }
    }
    #[test]
    fn put_sql_pins_put_then_participants_then_tags() {
        // Arrange: Put (:164-172), record-driven: the legs, updateParticipants
        // (sql_participations.go:78-119), updateTags (sql_tags.go:57-79).
        let rec = test_record();
        let now = chrono::Utc::now();
        // Act:
        let put = put_sql("al-1", &rec, now, &[]).expect("put renders");
        // Assert: 48 map columns (45 base row + 3 FTS from PostMapArgs; the
        // 4 JSON columns are overwritten in place) + id = 49 insert args;
        // update drops created_at (selectUpdateColumns) = 48 args; id last.
        assert!(put.update.0.starts_with("UPDATE album SET "));
        assert!(put.update.0.ends_with("WHERE id = ?"));
        assert_eq!(put.update.1.len(), 48);
        assert_eq!(put.update.1.last(), Some(&Value::Text("al-1".to_string())));
        assert!(put.insert.0.starts_with("INSERT INTO album ("));
        let tuple = put
            .insert
            .0
            .rsplit("VALUES (")
            .next()
            .unwrap()
            .strip_suffix(")")
            .unwrap();
        assert_eq!(
            tuple.matches('?').count(),
            49, // 48 map columns + id
            "the VALUES tuple must pair one ? per column"
        );
        assert_eq!(put.insert.1.len(), 49);
        assert_eq!(put.insert.1.last(), Some(&Value::Text("al-1".to_string())));
        // The full write set carries the PostMapArgs FTS columns and drops
        // the DDL orphan `releases` (no Go field, never written).
        assert!(put.insert.0.contains("full_text"));
        assert!(put.insert.0.contains("search_participants"));
        assert!(put.insert.0.contains("search_normalized"));
        assert!(!put.insert.0.contains("releases"));
        // imported_at is stamped to `now` at Put entry (:210) and is in the
        // UPDATE set (created_at is dropped by selectUpdateColumns).
        assert!(!put.update.0.contains("created_at"));
        assert!(put.update.0.contains("imported_at"));
        // updateParticipants: delete-all then the json_each INSERT.
        assert_eq!(
            put.participants_delete,
            (
                "DELETE FROM album_artists WHERE album_id = ?".to_string(),
                vec![Value::Text("al-1".to_string())],
            )
        );
        let p_ins = put
            .participants_insert
            .as_ref()
            .expect("participants insert");
        assert!(p_ins.0.contains("INSERT INTO album_artists"));
        assert!(p_ins
            .0
            .contains("JOIN artist ON artist.id = json_extract(value, '$.artist_id')"));
        // updateTags: delete-then-insert; empty indexed tags -> no insert.
        assert_eq!(
            put.tags_delete,
            (
                "DELETE FROM album_tags WHERE album_id = ?".to_string(),
                vec![Value::Text("al-1".to_string())],
            )
        );
        assert!(put.tags_insert.is_none());
    }

    #[test]
    fn put_sql_restricted_external_info_touches_only_the_six() {
        // Arrange: UpdateExternalInfo (:223) — record-driven restriction.
        let rec = test_record();
        let now = chrono::Utc::now();
        // Act:
        let put = update_external_info_sql("al-1", &rec, now).expect("restricted put");
        // Assert: the UPDATE SET is exactly the six external columns, none
        // other; created_at/updated_at/name untouched.
        let set = put
            .update
            .0
            .strip_prefix("UPDATE album SET ")
            .and_then(|s| s.strip_suffix(" WHERE id = ?"))
            .expect("set clause");
        let cols: Vec<&str> = set
            .split(", ")
            .map(|pair| pair.split(" = ?").next().unwrap())
            .collect();
        assert_eq!(
            cols,
            vec![
                "description",
                "external_info_updated_at",
                "external_url",
                "large_image_url",
                "medium_image_url",
                "small_image_url",
            ]
        );
        assert_eq!(put.update.1.len(), 7); // six + the WHERE id
                                           // The INSERT leg stays full-column (Go's put inserts the whole row).
        assert_eq!(put.insert.1.len(), 49);
    }

    #[test]
    fn put_sql_empty_update_set_errors() {
        // cols_to_update naming only created_at drops everything ->
        // EmptyUpdateSet, not degenerate SQL.
        let rec = test_record();
        let err = put_sql("al-1", &rec, chrono::Utc::now(), &["created_at"]).unwrap_err();
        assert!(matches!(err, crate::marshal::MarshalError::EmptyUpdateSet));
    }

    #[test]
    fn participant_names_pins_name_plus_sort_name_and_never_sub_role() {
        // AllNames (model/participants.go:174-185): name + non-empty
        // SortArtistName, unique, NEVER SubRole.
        let names = [
            NamedParticipant {
                id: "ar-1".to_string(),
                name: "Nils Frahm".to_string(),
                sort_name: "Frahm, Nils".to_string(),
                sub_role: "Vocals".to_string(),
            },
            // Duplicate name (dedup) with an empty sort name.
            NamedParticipant {
                id: "ar-2".to_string(),
                name: "Nils Frahm".to_string(),
                sort_name: String::new(),
                sub_role: "Piano".to_string(),
            },
            // Empty sort name contributes nothing new.
            NamedParticipant {
                id: "ar-3".to_string(),
                name: "Jana Firla".to_string(),
                sort_name: String::new(),
                sub_role: String::new(),
            },
        ];
        let out = participant_names(&names);
        assert_eq!(
            out,
            vec![
                "Nils Frahm".to_string(),
                "Frahm, Nils".to_string(),
                "Jana Firla".to_string(),
            ],
        );
        // SubRole text must never leak into the FTS input.
        assert!(!out
            .iter()
            .any(|n| n.contains("Vocals") || n.contains("Piano")));
    }

    #[test]
    fn touch_and_missing_folder_pin_update_shapes() {
        // Arrange: Touch (:258-270) chunks at 200; TouchByMissingFolder
        // (:273-286).
        let ids = (0..201).map(|i| format!("al-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = touch_sql(&ids, 1_700_000_000);
        let (mf_sql, mf_args) = touch_by_missing_folder_sql(1_700_000_000);
        // Assert:
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].1[0], Value::Integer(1_700_000_000));
        assert_eq!(stmts[0].1.len(), 201);
        assert_eq!(stmts[1].1.len(), 2);
        assert_eq!(
            mf_sql,
            "UPDATE album SET imported_at = ? WHERE folder_ids IS NOT NULL AND \
             EXISTS (SELECT 1 FROM json_each(folder_ids) AS je \
             JOIN main.folder AS f ON je.value = f.id WHERE f.missing = ?)"
        );
        assert_eq!(
            mf_args,
            vec![Value::Integer(1_700_000_000), Value::Bool(true)]
        );
    }

    #[test]
    fn get_years_and_purge_pin_their_shapes() {
        // Arrange: GetYears (:229-241), purgeEmpty (:347-362).
        // Act:
        let (years_sql, years_args) = get_years_sql(&[1, 2], None);
        let (purge_sql, purge_args) = purge_empty_sql(&[1]);
        // Assert:
        assert_eq!(
            years_sql,
            "SELECT distinct max_year FROM album WHERE max_year > ? AND missing = ? \
             AND library_id IN (?, ?) ORDER BY max_year"
        );
        assert_eq!(
            years_args,
            vec![
                Value::Integer(0),
                Value::Bool(false),
                Value::Integer(1),
                Value::Integer(2),
            ]
        );
        assert_eq!(
            purge_sql,
            "DELETE FROM album WHERE id not in \
             (select distinct(album_id) from media_file) AND library_id IN (?)"
        );
        assert_eq!(purge_args, vec![Value::Integer(1)]);
    }

    #[test]
    fn copy_attributes_pins_cast_read_and_set_map_update() {
        // Arrange: CopyAttributes (:243-256): the text-cast read then the
        // SET-map update; the created_at poison guard is the caller's.
        let columns = vec!["description".to_string(), "created_at".to_string()];
        // Act:
        let (read_sql, read_args) = copy_attributes_read_sql("al-from", &columns);
        let (upd_sql, upd_args) = copy_attributes_update_sql(
            "al-to",
            &columns,
            vec![Value::from("x"), Value::from("2024-01-01")],
        );
        // Assert:
        assert_eq!(
            read_sql,
            "SELECT cast(description as text) as description, \
             cast(created_at as text) as created_at FROM album WHERE id = ?"
        );
        assert_eq!(read_args, vec![Value::Text("al-from".to_string())]);
        assert_eq!(
            upd_sql,
            "UPDATE album SET description = ?, created_at = ? WHERE id = ?"
        );
        assert_eq!(
            upd_args,
            vec![
                Value::Text("x".to_string()),
                Value::Text("2024-01-01".to_string()),
                Value::Text("al-to".to_string()),
            ]
        );
    }

    #[test]
    fn refresh_play_counts_pins_go_literal_cte() {
        // Arrange: RefreshPlayCounts (:326-344) is a fixed literal.
        // Act:
        let (sql, args) = refresh_play_counts_sql();
        // Assert:
        assert!(sql.contains("with play_counts as ("));
        assert!(sql.contains("on conflict (user_id, item_id, item_type) do update"));
        assert!(args.is_empty());
    }

    #[test]
    fn sole_album_artist_pins_semi_join_and_json_length() {
        // Arrange: SoleAlbumArtistFilter (:202-206) and
        // GetSoleAlbumArtistIDsInSubtrees (:213-229).
        // Act:
        let (solo_sql, solo_args) = sole_album_artist_filter_sql("ar-1");
        let subtree = get_sole_album_artist_ids_in_subtrees_sql(7, &["music".to_string()]);
        // Assert:
        assert_eq!(
            solo_sql,
            "(album.id IN (SELECT album_id FROM album_artists \
             WHERE artist_id = ? AND role IN (?)) \
             AND json_array_length(participants, '$.albumartist') = 1)"
        );
        assert_eq!(
            solo_args,
            vec![
                Value::Text("ar-1".to_string()),
                Value::Text("albumartist".to_string()),
            ]
        );
        assert_eq!(subtree.len(), 1);
        assert!(subtree[0]
            .0
            .contains("EXISTS (SELECT 1 FROM json_each(album.folder_ids)"));
        assert!(subtree[0]
            .0
            .contains("json_extract(participants, '$.albumartist[0].id')"));
        assert_eq!(subtree[0].1[0], Value::Integer(7));
    }

    #[test]
    fn touched_albums_pins_library_and_last_scan_conditions() {
        // Arrange: GetTouchedAlbums (:296-304): Eq{library.id} and the
        // imported_at > last_scan_at comparison.
        // Act:
        let (sql, args) = get_touched_albums_sql(None, false, None, 7);
        // Assert:
        assert!(sql.ends_with("WHERE library.id = ? AND album.imported_at > library.last_scan_at"));
        assert_eq!(args, vec![Value::Integer(7)]);
    }

    #[test]
    fn get_all_ids_pins_id_projection_with_annotation_need() {
        // Arrange: getAllIDs (:196-206) with the annotation join.
        // Act:
        let (sql, args) = get_all_ids_sql(None, true, Some("u1"), false);
        // Assert:
        assert!(sql.contains("SELECT album.id FROM album"));
        assert!(sql.contains("LEFT JOIN annotation ON (annotation.item_id = album.id"));
        assert_eq!(
            args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
            ]
        );
        // Without the need: the plain id select.
        let (bare, bare_args) = get_all_ids_sql(None, false, Some("u1"), false);
        assert_eq!(bare, "SELECT album.id FROM album");
        assert!(bare_args.is_empty());
    }

    #[test]
    fn cursor_chunk_pins_id_in_list_over_the_album_select() {
        // Arrange: GetCursor's per-chunk re-query (artwork_hydration.go:41-78):
        // GetAll with an album.id IN filter.
        // Act:
        let (sql, args) = cursor_chunk_sql(
            &["al-1".to_string(), "al-2".to_string()],
            Some("u1"),
            false,
            None,
            &["name asc".to_string()],
            2,
            0,
        );
        // Assert: join args first, then the IN ids. The `LIMIT 2` bound is a
        // literal (`select.go:139-142`), so it is not appended to args.
        assert!(sql.contains("WHERE album.id IN (?, ?) ORDER BY name asc LIMIT 2"));
        assert_eq!(
            args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("al-1".to_string()),
                Value::Text("al-2".to_string()),
            ]
        );
    }
}
