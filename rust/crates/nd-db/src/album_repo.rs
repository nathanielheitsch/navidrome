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
//! Deliberately absent (other lanes/slices own them): the streaming/cursor
//! machinery `queryWithStableResults`/`wrapAlbumCursor`/`streamByIDs`/`chunkOptions`
//! (`:296-324`, `persistence/artwork_hydration.go:19-54` — their WHERE shapes pin
//! here, see [`get_touched_albums_sql`]/[`cursor_chunk_sql`]), the
//! `getSortMappings`/`naturalSort`/`mapSortOrder` config-gated transforms
//! (`sql_base_repository.go:125-137` — the mapping table pins here as data,
//! [`sort_mappings`]) and the REST shims `Count`/`Read`/`ReadAll`
//! (`:446-459`, `sql_restful.rs` owns the options parsing).
//!
//! Landed in S-D3c: the executed [`AlbumRepo`] reads (`Get` `:234-243`,
//! `GetAll` `:245-256`), wired to the shared seam (`crate::hydrate::row_to_album` →
//! `AlbumRow::into_record`) and then to `hydrateArtwork` (`:257-260`) through
//! [`crate::artwork_hydration::hydrate_album_artwork`]. Only Get/GetAll are
//! artwork-hydrated here: Go's `Read`/`ReadAll` (`:471-477`) hydrate only because
//! they delegate to `Get`/`GetAll`, `GetCursor` always hydrates (`:306-315`) but
//! belongs to the streaming lane, and `GetTouchedAlbums` (`:392-403`) does NOT
//! hydrate at all — so its statement builder stays statement-only. Go's `Search`
//! (`:452-465`) hydrates artwork too, but the Rust search lane renders its
//! projection from `sql_search::new_select` (`sql_search.rs:129-131` —
//! `SELECT * FROM album`), which carries no `library_path`/`library_name`/annotation
//! columns and so cannot be hydrated through this seam: Rust `Search` results are
//! NOT artwork-hydrated (recorded as a residual).
//! Landed in DB-09: the executed Put (`album_repository.go:208-219`) as
//! [`put_album_in_tx`] — base upsert plus participants/tags junction maintenance
//! on the CALLER's transaction, executing exactly the bundle [`put_sql`] returns.
//! It issues NO FTS statement: the schema's own `album_fts_ai`/`_au`/`_ad` triggers
//! maintain the index, so an explicit FTS write would double-index it.
//!
//! `GetAll` takes no `optimizePagination` (`sql_base_repository.go:477-486`) on
//! either side: Go passes no options to `queryAll` (`album_repository.go:248`), so
//! Go's offset rewrite never fires for albums. That rewrite is unimplemented
//! port-wide; see `mediafile_repo`'s module docs for the one Go read that would
//! take it.
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column/order-by names) are
//! the registered, caller-supplied constants; every value binds to `?`.
//! Order-by identifiers arrive caller-supplied and direction-carrying per
//! `buildSortOrder` output — no user text reaches an ORDER BY fragment.
//!
//! Scope of the executed repo reads: [`AlbumRepo::get_all`] is the
//! order-by/pagination SUBSET of Go's `GetAll(ctx, options...)`. Go additionally
//! applies `options[0].Filters` through `newSelect` → `applyFilters`
//! (`sql_base_repository.go:139-146`, `:247-252`), which this signature cannot
//! express, so a filtered list read goes through the [`select_album_sql`]
//! `where_clauses` seam with the caller's rendered clause until the REST/options
//! lane wires `QueryOptions.Filters`.

use std::collections::BTreeMap;

use crate::runtime_error::DbOperationError;
use crate::sql_annotations::ANNOTATION_TABLE;
use crate::sql_participations::{participant_id_filter, Participants};
use crate::sql_restful::{boolean_filter, Filter};
use crate::sql_search::{do_search, SearchConfig, SearchOutcome, SearchStrategy, Sql};
use crate::sql_search_fts::{new_fts_search, SearchStrategy as FtsFallback};
use crate::sql_search_like::{new_legacy_search, new_like_search};
use crate::sql_tags::{album_genres, delete_tags_sql, genre_id_filter, insert_tags_sql, DbTags};
use crate::sqlrepo::{Repository, SqlError, Value};
use chrono::{DateTime, Utc};

/// `r.tableName` (`NewAlbumRepository`, `album_repository.go:105-122`).
pub const ALBUM_TABLE: &str = "album";

/// `setSortMappings` (`album_repository.go:110-120`): the album sort field →
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

/// `albumSearchConfig` (`album_repository.go:446-450`).
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

/// `Search` (`album_repository.go:452-465`): dispatch through the shared
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

/// The `selectAlbum` shape (`album_repository.go:227-232`): `album.*`, the
/// library display columns (Go LEFT-joins `library` — the path/name are
/// display-only), then the logged-in user's annotation LEFT JOIN
/// (`withAnnotation`, `sql_annotations.go:63-91`). The sqlrepo builder models
/// only INNER JOIN, so — like the sibling `playlist` lane — the whole select
/// renders as one literal with the joins at their FROM position; the join's
/// `?` args bind BEFORE the WHERE args (squirrel binds join args at the join
/// position).
///
/// `user_id: None` keeps only `album.average_rating`
/// (`sql_annotations.go:65-66` — Go's invalid-user branch adds the bare
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

/// `Get` (`album_repository.go:234-243`): `GetAll` with
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

/// `GetAll` (`album_repository.go:245-256`): [`select_album_sql`] with the
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

/// `CountAll` (`album_repository.go:193-200`): `newSelect` + library filter +
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
    // squirrel renders joins before WHERE (`select.go:92-118`), so the join args
    // bind BEFORE the filter args even though Go applied the filter first
    // (`album_repository.go:193-200`: applyLibraryFilter, then withAnnotation).
    // Rendering WHERE first emits `FROM album WHERE … LEFT JOIN annotation …`,
    // which SQLite rejects with `near "LEFT": syntax error`; this matches the
    // mediafile twin (`mediafile_repo.rs:count_all_sql`).
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

/// `Exists` (`album_repository.go:202-206`): the base count by id (Go counts
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

/// `albumFilters` (`album_repository.go:124-151`): the registered filter
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

/// `model.Album`'s row as the seam reads and writes it (`model/album.go:15-73`):
/// the 45 non-id persisted album columns (the contract's 46 minus `id`, which
/// the seam forbids in the map; the id is appended last by [`put_sql`]), plus the
/// five **hydration-only** fields the read path owns — `id`, `library_path`,
/// `library_name`, `annotation`, `genres` (`persistence/album_repository.go:41-69`,
/// `selectAlbum` `:226-232`).
///
/// Deliberately absent from the WRITE map (entity write contract): the DDL orphan
/// column `releases` (no Go field, no Go writer — a put never emits it, so it stays
/// at its DDL default 0), `average_rating` (annotation-managed, never written by
/// entity Put), and every non-persisted field (`LibraryPath`/`LibraryName`/
/// `Genres` are `structs:"-"` join-only; the `Annotations`/`ItemImage` embeds carry
/// no columns; `Album.ID` is appended by the write legs, not the map).
/// [`album_row`] emits none of the five hydration-only fields, so the write SQL is
/// byte-identical to a record whose derived fields are empty — asserted by
/// `tests/runtime_hydrate_album_mediafile.rs`.
///
/// The Go `RgAlbumGain`/`RgAlbumPeak` dbx shims (`album_repository.go:37-38`) exist
/// only because dbx's default field-map cannot render `RGAlbumGain` to
/// `rg_album_gain`; Rust binds the real column names directly, so the two
/// unconditional copies at `album_repository.go:66-67` are no-ops here — see
/// [`crate::hydrate::album_post_scan`]. `discs`/`participants`/`tags`/`folder_ids`
/// hold the *typed* values the Go shims shadow: PostMapArgs overwrites their column
/// keys with JSON on the write side ([`album_row`] routes that through
/// [`crate::marshal::album_post_map_args`]), and on the read side the raw TEXT
/// columns sit on [`AlbumRow`] until [`crate::hydrate::album_post_scan`] decodes
/// them.
///
/// Timestamps are `DateTime<Utc>` bound through `crate::time`. `created_at`/
/// `updated_at`/`external_info_updated_at` are nullable (Go zero
/// `time.Time` / `*time.Time` nil → NULL); `imported_at` is NOT NULL — Go
/// stamps `ImportedAt = Now` at `Put` entry (`album_repository.go:210`),
/// mirrored in [`put_sql`].
///
/// `participant_names` is the FTS input the record carries beside the participant
/// JSON (see [`NamedParticipant`]). A hydrated record gets it from the decoded
/// `participants` column with an EMPTY `sort_name`: the column stores only
/// `{id, name, subRole}` (`sql_participations.go:13-17`) and Go fills sort names
/// only in `getParticipants` (`:127-146`), which re-reads the `artist` table and
/// belongs to a later lane. So re-writing a hydrated album reproduces every
/// participant name but no sort names — documented divergence, and the reason
/// [`crate::hydrate::album_post_scan`] fills this field rather than leaving it.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRecord {
    /// The row's `id` (Go `Album.ID`, `structs:"id"` at `model/album.go:19`).
    /// Read-path identity: [`crate::hydrate::row_to_album`] fills it from the row so
    /// a bulk read keeps every row's own id. [`album_row`] does NOT emit it — the id
    /// is appended by the repo's write legs ([`put_sql`] takes it as its own
    /// argument, matching Go's `put(ctx, id, …)`, `sql_base_repository.go:618`).
    /// Same precedent as `FolderRecord::id` (`folder_repo.rs:347`) and
    /// `PlaylistRecord::id` (`playlist_repo.rs:486`).
    pub id: String,
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
    /// `duration` (Go `Duration`, `float32` at `model/album.go:38`). The DDL
    /// column is `real` either way; `f64` matches the column affinity (same
    /// precedent as `PlaylistRecord::duration`). Go's `float32` destination
    /// rejects a cell outside its range (`1e+40` → `to a float32: value out of
    /// range`, executed); the `f64` field accepts it — documented residual in
    /// `hydrate-contract.md`.
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
    /// `genre` (Go `Genre` — "the most common genre", `model/album.go:64`). For an
    /// album that label is *indirectly* true: `ToAlbum` builds tags through
    /// `Album.SetTags` → `TagList.GroupByFrequency` (`model/album.go:115-122`,
    /// `model/tag.go:25-57`), which orders each name's values by frequency, so the
    /// stored `genre` tag's first entry is the most frequent one. The column itself
    /// is NOT computed from that ranking — read hydration derives it from the stored
    /// tags shim via `Tags.ToGenres()` (`hydrate.rs:2619-2621`), i.e. `genres[0].Name`.
    /// `hash:"ignore"` (`model/album.go:64`) keeps it out of `Album.Equals`, so a
    /// genre-only change never refreshes an album row, unlike a media row.
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
    /// `library_path` (Go `Album.LibraryPath`, `structs:"-"` at `model/album.go:21`
    /// — join-only). Hydrated from `library.path as library_path` (`selectAlbum`,
    /// `album_repository.go:227`); [`album_row`] never emits it. The join is a LEFT
    /// join, so a row whose library is gone holds SQL NULL, which Go's non-pointer
    /// `string` rejects: [`crate::hydrate::row_to_album`] errors when the alias is
    /// present but NULL, and leaves Go's zero (empty string) when a projection
    /// omits the alias (`dbx@v1.12.0/rows.go:80-86` discards unmatched columns).
    pub library_path: String,
    /// `library_name` (Go `Album.LibraryName`, `structs:"-"`, `model/album.go:22`
    /// — join-only), from `library.name as library_name`. Same read rules as
    /// [`AlbumRecord::library_path`].
    pub library_name: String,
    /// The per-user annotation the select's `annotation` LEFT JOIN contributes
    /// (`sql_annotations.go:63-91`), **derived, not persisted**: Go's
    /// `Album.Annotations` embed is `structs:"-"` (`model/album.go:16`), so
    /// [`album_row`] emits none of it and the write SQL is unchanged.
    /// [`crate::hydrate::row_to_album`] fills it from the six joined columns plus the
    /// base `album.average_rating`, with zero values where a light query omits them.
    /// Its timestamp strings carry Go's RFC3339Nano wire form
    /// ([`crate::hydrate::to_go_time_text`]), NOT the SQLite storage spelling.
    pub annotation: nd_model::annotation::Annotation,
    /// `genres` (Go `Album.Genres`, `structs:"-"` at `model/album.go:65` —
    /// join-only, derived). Set ONLY by [`crate::hydrate::album_post_scan`] from the
    /// decoded `tags` shim (`Tags.ToGenres`, `model/tag.go:138-148`), which also
    /// overwrites [`AlbumRecord::genre`] (`album_repository.go:57`). Left empty when
    /// the shim is empty, because Go's guard skips the assignment and the column
    /// value of `genre` survives. See [`crate::sql_tags::Genre`] for why this DTO is
    /// nd-db-local; never emitted by [`album_row`].
    pub genres: Vec<crate::sql_tags::Genre>,
    /// Go's embedded `ItemImage` (`model/album.go:17`; every field `structs:"-"` at
    /// `model/artwork.go:29-40`): the album's own artwork state, **read-path only**.
    /// Filled by [`crate::artwork_hydration::hydrate_album_artwork`] — the SECOND
    /// hydration layer (a batched `item_artwork` lookup run after PostScan,
    /// `persistence/album_repository.go:257-260`) — never by
    /// [`crate::hydrate::row_to_album`], whose projection has no artwork column.
    /// The zero value means UNRESOLVED (no state row found), which is NOT the same
    /// state as known-absent (`image_absent == true`); see
    /// [`crate::artwork_hydration::ItemImage`]. Never emitted by [`album_row`], so
    /// the write map stays at 48 keys.
    pub image: crate::artwork_hydration::ItemImage,
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
/// The [`NamedParticipant`] view of a decoded `participants` column —
/// `dbAlbum.PostScan`'s `unmarshalParticipants` result read the way Go's
/// `Participants.AllNames()` (`model/participants.go:174-185`) sees it.
///
/// `sort_name` is EMPTY for every entry: the column stores only
/// `{id, name, subRole}` (`persistence/sql_participations.go:13-17`) and Go fills
/// sort names later in `getParticipants` (`:127-146`) by re-reading the `artist`
/// table — a read this hydrator does not perform (that belongs to the
/// artist-hydration lane). Keeping `participant_names` populated is what makes a
/// hydrated record's re-write reproduce every participant name in `full_text`/
/// `search_participants`; the absent sort names are the documented divergence in
/// [`AlbumRecord`]'s docs. `sub_role` is carried through and is never a name source.
#[must_use]
pub fn participant_names_from_participants(participants: &Participants) -> Vec<NamedParticipant> {
    participants
        .values()
        .flatten()
        .map(|p| NamedParticipant {
            id: p.id.clone(),
            name: p.name.clone(),
            sort_name: String::new(),
            sub_role: p.sub_role.clone(),
        })
        .collect()
}

/// The album-version tag name (`model.TagAlbumVersion`, `model/tag.go:197`).
const TAG_ALBUM_VERSION: &str = "albumversion";
/// The catalog-number tag name (`model.TagCatalogNumber`, `model/tag.go:206`).
const TAG_CATALOG_NUMBER: &str = "catalognumber";

/// `structs.Map(dbAlbum)` + `dbAlbum.PostMapArgs` (`album_repository.go:71-97`) as
/// one sorted [`crate::marshal::ColumnMap`]: exactly the 45 non-id persisted album
/// columns (`id` is appended last by [`put_sql`]), with the PostMapArgs overwrites
/// routed through the S-D2a hook so the transformation lives in one place.
///
/// **Write-map invariance (S-D3b, extended by S-D3c):** the five hydration-only
/// record fields (`id`, `library_path`, `library_name`, `annotation`, `genres`)
/// and the read-path-only artwork field (`image`) are NOT keys here — Go's
/// `structs:"-"` tags keep them out of `structs.Map` (`model/album.go:16-17`,
/// `model/artwork.go:29-40`), and `id` is the write legs' own argument — so a
/// hydrated record renders the identical 48-key INSERT/UPDATE column set an
/// unwritten one does.
///
/// **S-D2b consistency invariant:** the FTS inputs are derived *atomically* from
/// the record's own `participants`/`discs`/`tags` (see [`album_derived`]) — the
/// derived values and the JSON/junction values share one source, so they can never
/// contradict each other.
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
/// `dbAlbum` raw row (phase 1 of the two-phase hydration seam,
/// `persistence/album_repository.go:29-39`): the columns `selectAlbum` scans
/// (`album_repository.go:227-232` — `album.*`, the two library display columns,
/// the six annotation columns and the base `average_rating`) with the four JSON
/// shim columns **still unparsed**.
///
/// The shims are Go's non-pointer `string` fields (`:31-34`), hence plain `String`
/// here and [`crate::hydrate::RowGet::get_text`] on the read: SQL NULL in any of
/// them aborts the scan for the same reason Go does with `converting NULL to
/// string is unsupported` (Rust's payload names the column instead of
/// reproducing that wording — see the `hydrate.rs` module-level note). Every
/// column the DDL declares for these four carries a NOT NULL default
/// (`schema_ddl.rs:58`: `discs`/`tags`/`participants` `'{}'`, `folder_ids` `'[]'`), so an
/// empty string means "the shim column was not scanned", never "NULL" — and Go's
/// `PostScan` guards (`:43`, `:52`, `:59`) treat it as the skip branch. `participants`
/// is the one shim with NO guard (`:48`), so even the empty string reaches
/// `unmarshalParticipants` and errors.
///
/// The join-only display columns, the annotation columns and `id` are read straight
/// onto the record — Go scans `Album.LibraryPath`/`LibraryName` and the promoted
/// `Annotations` embed by FIELD name despite `structs:"-"`
/// (`dbx@v1.12.0/struct.go:163-215` descends into anonymous embeds and maps through
/// `DefaultFieldMapFunc`) — so they are not duplicated on this raw struct. Phase 2 is
/// [`crate::hydrate::album_post_scan`].
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRow {
    /// The album columns scanned from the row, the four shim fields still empty.
    pub record: AlbumRecord,
    /// The raw `discs` TEXT column, unparsed (Go `dbAlbum.Discs`, `:31`).
    pub discs: String,
    /// The raw `participants` TEXT column, unparsed (Go `dbAlbum.Participants`,
    /// `:32`). Decoded UNGUARDED, like Go's unguarded `:48`.
    pub participants: String,
    /// The raw `tags` TEXT column, unparsed (Go `dbAlbum.Tags`, `:33`). Its decode
    /// is what derives `genre`/`genres`.
    pub tags: String,
    /// The raw `folder_ids` TEXT column, unparsed (Go `dbAlbum.FolderIDs`, `:34`).
    pub folder_ids: String,
}

impl AlbumRow {
    /// `dbAlbum.PostScan` (`album_repository.go:41-69`): thin delegation onto
    /// [`crate::hydrate::album_post_scan`], surfaced the way Go's `ScanStruct` error
    /// leaves the read (`dbx@v1.12.0/rows.go:92-102` returns the hook's error
    /// untouched, and the read funnel surfaces it as a column-conversion failure).
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] carrying one of Go's EXACT album
    /// prefixes ([`crate::hydrate::ALBUM_DISCS_PREFIX`],
    /// [`crate::hydrate::ALBUM_PREFIX`],
    /// [`crate::hydrate::ALBUM_FOLDER_IDS_PREFIX`]).
    pub fn into_record(self) -> Result<AlbumRecord, crate::hydrate::HydrateError> {
        crate::hydrate::album_post_scan(self)
    }
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

/// `Put` (`album_repository.go:208-219`), record-driven (S-D2b): the record
/// is the only write input — no caller-supplied column arrays.
///
/// `now` stamps the row exactly like Go's `Put`
/// (`album_repository.go:208-219`): `imported_at` to `now`
/// (`al.ImportedAt = time.Now()`, `:210`); `created_at` and `updated_at`
/// pass through the record unchanged — Go stamps neither in `Put`.
/// `created_at` is still dropped from the UPDATE set by the seam.
///
/// **Consistency invariant:** the PostMapArgs hook is fed the FTS inputs
/// derived from the record's own `participants`/`discs`/`tags`
/// (`album_derived`) — the same values the JSON columns and the
/// `album_artists`/`album_tags` junction maintenance are built from — so the
/// FTS columns can never contradict the stored JSON.
///
/// Both legs render through the shared [`crate::marshal::MarshalledRow`]:
/// UPDATE = the `selectUpdateColumns`-filtered set (no `created_at`; `id`
/// binds last via `WHERE id = ?`), INSERT = the full sorted columns with
/// `id` last in both lists (`sql_base_repository.go:637-641`). Go's `put`
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
/// Returns [`crate::marshal::MarshalError`] when the map carries `id`
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
/// What a successful [`put_album_in_tx`] reports.
///
/// Both fields are ADVISORY until the CALLER commits: a rolled-back Put publishes
/// nothing, and the album repository keeps no cache (Go keeps none either), so there
/// is nothing to invalidate on rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumWriteResult {
    /// The written row's id: [`AlbumRecord::id`], which is always non-empty here. Go
    /// assigns `al.ID = id` from `r.put`'s return value (`album_repository.go:211-215`)
    /// and then keys BOTH junction legs with it; the port reports the same id, which is
    /// the record's own because this boundary never generates one.
    ///
    /// Go's `id` can differ from the input ONLY on its random-id arm
    /// (`sql_base_repository.go:635-638`), which this port refuses instead
    /// ([`AlbumWriteError::EmptyId`]), so the returned id always equals the input's.
    pub id: String,
    /// `true` when the INSERT leg ran rather than the UPDATE leg, i.e. when the UPDATE
    /// matched zero rows (`sql_base_repository.go:630-641`). A predefined id that misses
    /// is therefore `inserted: true`, not a miss and not a `NotFound`.
    pub inserted: bool,
}
/// Callback error of the executed album [`put_album_in_tx`].
///
/// The builder arm is REAL and must stay off the SQLite arm: [`put_sql`] returns
/// [`crate::marshal::MarshalError`], so a failure to materialize the two base legs is a
/// domain failure, not a driver one. `Infallible` would force such a failure onto
/// [`DbOperationError::Sqlite`], which `D-ERROR` forbids — the concrete cause would be
/// lost and `store::is_busy` would be asked to classify a non-driver failure.
///
/// For the FULL-record seam this wrapper uses, neither [`crate::marshal::MarshalError`
/// variant is reachable today: [`album_row`] never emits `id` (so not `IdInMap`) and its
/// 45-column map always leaves a settable UPDATE set (so not `EmptyUpdateSet`, which
/// needs a `cols_to_update` restriction this signature does not take). The arm exists
/// because the renderer it executes IS fallible, not because a caller can provoke it here.
#[derive(Debug, thiserror::Error)]
pub enum AlbumWriteError {
    /// The base legs could not be materialized by [`put_sql`]. Go's equivalent failure
    /// is `fmt.Errorf("error preparing values to write to DB: %w", err)`
    /// (`sql_base_repository.go:620-623`); the port keeps the typed `MarshalError` on the
    /// `Domain` arm rather than stringifying it.
    #[error(transparent)]
    Builder(#[from] crate::marshal::MarshalError),

    /// [`AlbumRecord::id`] is empty, so there is nothing to write. **DOCUMENTED
    /// DEVIATION, not exact Go parity:** Go's `put` fills an empty id with
    /// `id2.NewRandom()` (`sql_base_repository.go:635-638`), SKIPS the UPDATE leg, and
    /// returns the generated id with no error. This port refuses instead, because album
    /// identity comes from SC (`nd_id::new_hash`/`new_hash1`/`new_random`,
    /// `nd-id/src/lib.rs:115-135`) and a DB-side id generator would be an invented policy
    /// nobody asked for; the caller's retry loop is also where id stability belongs, so
    /// generating inside the DB would make a retry publish a different id. Accepted under
    /// the supplied-SC-ID contract (parent decision, same as
    /// [`crate::folder_repo::FolderWriteError::EmptyId`] and
    /// [`crate::artist_repo::ArtistWriteError::EmptyId`]); Go has no equivalent error here,
    /// so do NOT describe this arm as Go-exact.
    #[error("album record has an empty id: the id comes from SC and is never generated here")]
    EmptyId,
}
/// `Put` (`album_repository.go:208-219`) EXECUTED on the CALLER's transaction, in Go's
/// exact statement order: the `r.put` upsert (`sql_base_repository.go:618-642`) — UPDATE
/// by `id` FIRST, INSERT only on a zero-row match — then `updateParticipants`
/// (`sql_participations.go:78-125`), then `updateTags` (`sql_tags.go:57-79`). Any leg
/// failure aborts the whole Put, which is what Go's caller relies on to roll back the
/// scanner's folder transaction (`scanner/phase_1_folders.go:410` `persistAlbum`).
///
/// It executes exactly what [`put_sql`] renders — the six-statement [`PutAlbumSql`]
/// bundle — and re-renders nothing here.
///
/// Identity: `record.id` is the UPDATE's `WHERE id = ?`, the INSERT's id column, and the
/// `album_id` of BOTH junction legs, which is Go's `al.ID = id` (`:215`) feeding
/// `updateParticipants`/`updateTags`. An empty id is refused as a typed contract
/// violation BEFORE any statement runs rather than repaired with a generated one; see
/// [`AlbumWriteError::EmptyId`]. No id is generated anywhere in this function, so a
/// retried call keeps the same identity and a rollback publishes no id.
///
/// Timestamps: `now` is the CALLER's clock and reaches [`put_sql`] as its `now`, which
/// stamps `imported_at` ONLY (`album_repository.go:210`; Go stamps neither `CreatedAt`
/// nor `UpdatedAt` in `Put`). `created_at` is dropped from the UPDATE set by the shared
/// seam (`marshal::UPDATE_DROP`), so it stays immutable across the update leg, and the
/// record is borrowed and never mutated — stamps travel as bound values.
///
/// Junction maintenance: each junction's DELETE runs unconditionally and BEFORE its
/// optional INSERT, which is Go's shape (`sql_participations.go:79-89`,
/// `sql_tags.go:58-74`). A record with NO participants (or no indexed tags) therefore
/// CLEARS the existing rows rather than leaving them stale; the `Option` insert leg is
/// skipped only after its delete ran. Both junction INSERTs are `INSERT … SELECT` over
/// `json_each` INNER-joined to `artist`/`tag`, so ids the dictionary does not have are
/// dropped SILENTLY by the join — Go's behavior (`sql_participations.go:117`,
/// `sql_tags.go:69`), not an error, and the stored `participants`/`tags` JSON columns
/// still carry those entries. That asymmetry is Go's own: `Put` never re-derives or
/// filters the JSON from the junction result, so this wrapper does not either. Counts
/// (`library_tag`, `album.average_rating`) and the library link are NOT touched here —
/// DB-17 and the annotation/library lanes own those, and Go's `Put` does no aggregation.
///
/// FTS: NO statement here mentions `album_fts`. The schema's `album_fts_ai`/`_au`/`_ad`
/// triggers (`nd-dbschema/src/schema_ddl.rs:85-87`) index the base row's own
/// `name`/`sort_album_name`/`album_artist`/`search_participants`/`discs`/`catalog_num`/
/// `album_version`/`search_normalized`, and [`put_sql`] derives those columns from the
/// record's own `participants`/`discs`/`tags` (the S-D2b consistency invariant), so the
/// index is correct from the base write alone. Junction writes touch no FTS trigger (the
/// junction tables have none). A malformed `tags` cell makes the trigger's
/// `json_each(NEW.tags, '$.albumversion')` fail — EXECUTED-RUST against the real trigger
/// DDL with SQLite 3.54.0, that failure is `SQLITE_ERROR` (`malformed JSON`, raw code 1,
/// NOT `SQLITE_CONSTRAINT_TRIGGER`), which aborts the base statement and rolls back the
/// caller's transaction.
///
/// Scope: a caller-trusted primitive, exactly like Go. `WHERE id = ?` and nothing else:
/// no library predicate, no auth check, no ownership guard. Library scoping is the
/// READ side's job ([`AlbumRepo::get`]'s `library_filter`), not this write's.
///
/// The transaction is the caller's: this never begins, commits, retries, checks out a
/// connection, or runs in autocommit, and there is no connection-bound variant. Raw
/// SQLite errors keep their concrete cause and extended code on the
/// [`DbOperationError::Sqlite`] arm — Go returns `Put`'s errors raw and unwrapped
/// (`album_repository.go:211-219` adds no `fmt.Errorf`).
///
/// `cols_to_update` is deliberately NOT a parameter: Go's `Put` passes no `colsToUpdate`
/// (`:212`), so this boundary is the unfiltered full-record save and needs no typed
/// column enum. The restricted `UpdateExternalInfo` write (`:223`) stays the rendered
/// [`update_external_info_sql`] seam until the lane that executes it (DB-15/16) exists.
///
/// # Errors
/// [`DbOperationError::Domain`]·[`AlbumWriteError::EmptyId`] for an empty
/// [`AlbumRecord::id`] (before any statement runs), and
/// [`DbOperationError::Domain`]·[`AlbumWriteError::Builder`] when [`put_sql`] fails to
/// materialize the base legs; [`DbOperationError::Sqlite`] carrying the concrete
/// `rusqlite::Error` (extended code intact) from the UPDATE, the INSERT, either junction
/// DELETE, or either junction INSERT — the `album.library_id` FK included, which Go
/// returns raw. A repeated id is an UPDATE hit rather than a constraint failure; a
/// zero-row UPDATE is NOT an error; unknown participant/tag ids are dropped by the JOIN
/// and are NOT an error.
pub fn put_album_in_tx(
    tx: &rusqlite::Transaction<'_>,
    record: &AlbumRecord,
    now: &DateTime<Utc>,
) -> Result<AlbumWriteResult, DbOperationError<AlbumWriteError>> {
    if record.id.is_empty() {
        // Go's `id == ""` arm generates a random id (`sql_base_repository.go:635-638`).
        // The port refuses instead: album identity belongs to SC, and a DB-side id would
        // also make a retried Put publish a different id.
        return Err(DbOperationError::Domain(AlbumWriteError::EmptyId));
    }
    let bundle = put_sql(&record.id, record, *now, &[])
        .map_err(|err| DbOperationError::Domain(AlbumWriteError::Builder(err)))?;

    // Base leg (:624-633): UPDATE first; a row count > 0 ends the upsert, so the INSERT
    // never runs. A zero-row match is the predefined-id insert fallback (:634-641).
    let inserted =
        crate::execution::execute(tx, &bundle.update).map_err(DbOperationError::sqlite)? == 0;
    if inserted {
        crate::execution::execute(tx, &bundle.insert).map_err(DbOperationError::sqlite)?;
    }

    // `updateParticipants` (:78-125): the delete ALWAYS runs — it is what clears stale
    // role rows when a role changes — and Go's early return on an empty participant list
    // (:87-89) only skips the INSERT.
    crate::execution::execute(tx, &bundle.participants_delete).map_err(DbOperationError::sqlite)?;
    if let Some(insert) = bundle.participants_insert.as_ref() {
        crate::execution::execute(tx, insert).map_err(DbOperationError::sqlite)?;
    }

    // `updateTags` (:57-79): same shape — unconditional delete, optional insert.
    crate::execution::execute(tx, &bundle.tags_delete).map_err(DbOperationError::sqlite)?;
    if let Some(insert) = bundle.tags_insert.as_ref() {
        crate::execution::execute(tx, insert).map_err(DbOperationError::sqlite)?;
    }

    Ok(AlbumWriteResult {
        id: record.id.clone(),
        inserted,
    })
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

/// `UpdateExternalInfo` (`album_repository.go:222-225`): `r.put` restricted
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

/// `Touch` (`album_repository.go:360-374`): one `imported_at = ?` UPDATE per
/// 200-id chunk (`slices.Chunk(ids, 200)`, `:264`).
#[must_use]
pub fn touch_sql(ids: &[String], imported_at: &DateTime<Utc>) -> Vec<Sql> {
    const CHUNK: usize = 200;
    ids.chunks(CHUNK)
        .map(|chunk| {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut args = vec![crate::time::value(imported_at)];
            args.extend(chunk.iter().map(|id| Value::from(id.as_str())));
            (
                format!("UPDATE {ALBUM_TABLE} SET imported_at = ? WHERE id IN ({placeholders})"),
                args,
            )
        })
        .collect()
}

/// `TouchByMissingFolder` (`album_repository.go:376-390`): the one UPDATE
/// whose EXISTS subquery walks `json_each(folder_ids)` into `folder` for
/// missing folders. `imported_at` binds first.
#[must_use]
pub fn touch_by_missing_folder_sql(imported_at: &DateTime<Utc>) -> Sql {
    (
        format!(
            "UPDATE {ALBUM_TABLE} SET imported_at = ? WHERE folder_ids IS NOT NULL AND \
             EXISTS (SELECT 1 FROM json_each(folder_ids) AS je \
             JOIN main.folder AS f ON je.value = f.id WHERE f.missing = ?)"
        ),
        vec![crate::time::value(imported_at), Value::Bool(true)],
    )
}

/// Callback error of the executed album touch primitives, which have NO domain failure
/// to carry: Go's `Touch`/`TouchByMissingFolder` return a bare `error` (or `(int64,
/// error)`) that can only be a driver failure (`album_repository.go:360-387`) — no
/// validation, no `NotFound`, no permission check. `Infallible` makes the `Domain` arm
/// structurally uninhabited rather than reserved, as in `library_repo::LibraryScanError`
/// and `tag_repo::TagWriteError`, and keeps every SQLite failure on the
/// [`DbOperationError::Sqlite`] arm so `store::is_busy` still classifies `SQLITE_BUSY`
/// for the executor's retry loop.
///
/// Go's `fmt.Errorf("error touching albums: %w", err)` (`:368`) and
/// `"error touching albums by missing folder: %w"` (`:384`) are deliberately NOT
/// reproduced: the port keeps the raw `rusqlite::Error` on the `Sqlite` arm so the
/// executor's retry classifier can still see `SQLITE_BUSY` (`decisions.md` E1, same call
/// `library_repo::assign_admins_in_tx` and `tag_repo::add_in_tx` record). The message
/// text differs from Go's; the cause does not.
pub type AlbumTouchError = DbOperationError<std::convert::Infallible>;

/// `Touch` (`album_repository.go:360-373`) EXECUTED on the CALLER's transaction: one
/// `imported_at = ?` UPDATE per 200-id chunk (`slices.Chunk(ids, 200)`, `:364`) in Go's
/// loop order over the CALLER's id order, executing exactly what [`touch_sql`] renders
/// rather than re-rendering it here. Go's `len(ids) == 0` guard (`:361-363`) and the
/// chunker's empty-input arm agree: an empty id list runs NO statement and returns
/// `Ok(0)`.
///
/// **Scope: exactly the supplied ids.** Go's statement is
/// `UPDATE album SET imported_at = ? WHERE id IN (chunk)` (`:365`) with no library
/// predicate, and its caller already hands it ids inside the job's library —
/// `scanner/phase_1_folders.go:452-456` derives them from `entry.missingTracks`'
/// `AlbumID`s. This primitive adds NO library filter and no auth guard: it is
/// caller-trusted, so a caller that supplies another library's album id HAS it touched
/// and the returned count includes it. Ownership is future SC typed authority
/// (`docs/rust-port/subplans/db-runtime.md:264`). Unknown ids are a silent no-op — Go checks the
/// count only for its `log.Debug("updated", c)` (`:370`) and never turns 0 into an error.
///
/// The affected-row total is returned for the caller's observability (Go logs it at
/// `:370` and discards it; nd-db has no logger). No policy rides on the count.
///
/// `imported_at` is the caller's stamp where Go binds `time.Now()` (`:365`); the caller
/// owns the clock and samples it outside any retry loop.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` from any chunk;
/// [`DbOperationError::Domain`] is structurally uninhabited ([`AlbumTouchError`]). A
/// failed chunk fails the call and the caller's transaction rolls back earlier chunks.
pub fn touch_albums_in_tx(
    tx: &rusqlite::Transaction<'_>,
    ids: &[String],
    imported_at: &DateTime<Utc>,
) -> Result<usize, AlbumTouchError> {
    let mut total = 0usize;
    for statement in touch_sql(ids, imported_at) {
        total += crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
    }
    Ok(total)
}

/// `TouchByMissingFolder` (`album_repository.go:376-387`) EXECUTED on the CALLER's
/// transaction: the ONE statement [`touch_by_missing_folder_sql`] renders.
///
/// **This one is GLOBAL — the name says so, and the scope is not an accident to be
/// narrowed.** Go's predicate is `folder_ids IS NOT NULL` plus
/// `EXISTS (SELECT 1 FROM json_each(folder_ids) AS je JOIN main.folder AS f ON
/// je.value = f.id WHERE f.missing = true)` (`:377-381`): no id list, no library
/// predicate, no `logged_user`/library filter anywhere in the method. It therefore
/// touches albums of libraries OTHER than the one being scanned whenever an album's
/// `folder_ids` references a folder that belongs to a different library and is flagged
/// missing — the cross-library reach is real (the schema allows it: `album.folder_ids`
/// is declared `jsonb default '[]' not null` with no FK (`nd-dbschema/src/schema_ddl.rs:58`),
/// and a fixture pins the reach here. Go bounds the consequences on the READ side:
/// `GetTouchedAlbums(ctx, libID)` (`:392-403`) scopes the follow-up to one library by
/// `album.imported_at > library.last_scan_at`.
///
/// Narrowing this to the scan job's libraries would be a recorded product deviation and
/// is NOT done here (parent decision, `nav-db07-db08-decisions.md` §A3/C1): the exact Go
/// predicate is retained and the scope is stated in the name and this comment.
///
/// Returns the affected-row count. Go's own caller DISCARDS it
/// (`scanner/phase_1_folders.go:541`, `if _, err := …`), so the number is for the
/// caller's observability only. NULL `folder_ids` is skipped by `IS NOT NULL` (Go's
/// `NotEq{"folder_ids": nil}`, `:379`); `'[]'` matches nothing through `json_each`;
/// a malformed JSON cell is a HARD SQLite `malformed JSON` failure of the whole
/// statement (executed: it touches 0 rows and errors), which the caller's transaction
/// turns into a rollback of everything before it. Go fails the same way — there is no
/// `json_valid` preflight and none is added here.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact — the malformed-JSON failure carries NO `SQLITE_CONSTRAINT_*` extended code,
/// so callers must match on the concrete `rusqlite::Error`, not an extended-code class).
/// [`DbOperationError::Domain`] is structurally uninhabited ([`AlbumTouchError`]).
pub fn touch_albums_with_missing_folders_globally_in_tx(
    tx: &rusqlite::Transaction<'_>,
    imported_at: &DateTime<Utc>,
) -> Result<usize, AlbumTouchError> {
    let rows = crate::execution::execute(tx, &touch_by_missing_folder_sql(imported_at))
        .map_err(DbOperationError::sqlite)?;
    Ok(rows)
}

/// `GetYears` (`album_repository.go:317-330`): the distinct non-missing
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

/// `purgeEmpty` (`album_repository.go:430-450`): one DELETE, optionally
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

/// `CopyAttributes` (`album_repository.go:331-356`): the read of the source
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

/// The only column the folder transaction copies (`scanner/phase_1_folders.go:496`,
/// `repo.CopyAttributes(ctx, prevID, a.ID, "created_at")`). Named as a constant because the
/// poison guard below is spelled for THIS column exactly as Go's is
/// (`album_repository.go:343-350`: `if col == "created_at" && …`), and
/// [`copy_album_created_at_in_tx`] executes exactly this one-column pair.
pub const COPY_CREATED_AT_COLUMN: &str = "created_at";

/// What [`copy_album_created_at_in_tx`] did. Go returns a bare `error` and its caller only
/// distinguishes `model.ErrNotFound` (`scanner/phase_1_folders.go:497-500`), so the two
/// successful SKIP arms are port-side observability rather than a Go distinction — kept
/// distinct because they are the cases a fixture has to pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyAlbumCreatedAtOutcome {
    /// The source's `created_at` was read and written onto the target id: one UPDATE that
    /// ran (`album_repository.go:355-356`).
    Copied,
    /// The source row exists but its `created_at` is poison — NULL, empty, or `0001-`
    /// prefixed (`:343-350`) — so Go's `if len(to) == 0 { return nil }` (`:351-353`) runs NO
    /// UPDATE. This is the guard that stops an aggregated album's birth time being
    /// overwritten with a zero value on every metadata-driven album-ID change (Go's comment
    /// at `:344-346`).
    SkippedPoisonedSource,
    /// No source row at `from_id`: Go's `queryOne` maps `sql.ErrNoRows` to `model.ErrNotFound`
    /// (`sql_base_repository.go:382-385`) and the folder transaction SILENTLY ignores that
    /// error (`scanner/phase_1_folders.go:497-500`). Reported as its own arm precisely so the
    /// caller reproduces that ignore instead of guessing it from a zero-row UPDATE.
    SourceNotFound,
}

/// `CopyAttributes(ctx, fromID, toID, "created_at")` (`album_repository.go:331-357`)
/// EXECUTED on the CALLER's transaction: read the source row cast to text through
/// [`copy_attributes_read_sql`], then run the SET-map UPDATE through
/// [`copy_attributes_update_sql`]. No new statement text — this is the executed half of the
/// two renderers above, and it is the SAME helper DB-16's planned
/// `copy_album_created_at_in_tx` needs, so it is written once (DB-14, per `D-BATCH`).
///
/// Go casts to text on the read (`:333-334`) precisely so a datetime cell is not decoded and
/// re-formatted by the driver; rusqlite hands back the stored TEXT for a `cast(… as text)`
/// column, so the port binds that text unchanged and the stored spelling round-trips byte for
/// byte.
///
/// The poison rule is Go's, verbatim (`:343-350`): skip when the value is invalid (NULL),
/// empty, or starts with `0001-`. A poisoned source means NO UPDATE at all (`:351-353`),
/// which is success, not an error.
///
/// Scope: caller-trusted, like every write primitive in this crate — `WHERE id = ?` and
/// nothing else (`:356`). No library predicate, no auth guard.
///
/// Retryable: rerunning it copies the same source value onto the same target, and this
/// method never writes the source's own `created_at`.
///
/// # Errors
/// [`DbOperationError::Sqlite`] from either statement, retaining the concrete
/// `rusqlite::Error`. A missing source row is NOT an error — it is
/// [`CopyAlbumCreatedAtOutcome::SourceNotFound`], the arm the caller ignores.
/// [`DbOperationError::Domain`] is structurally uninhabited (there is no validation to fail:
/// both ids are the caller's and both statements are fixed).
pub fn copy_album_created_at_in_tx(
    tx: &rusqlite::Transaction<'_>,
    from_id: &str,
    to_id: &str,
) -> Result<CopyAlbumCreatedAtOutcome, DbOperationError<std::convert::Infallible>> {
    // Go's read arm (`:336-341`): `queryOne` over the cast projection. `query_optional`'s
    // `None` is that repository's `model.ErrNotFound`, which the folder tx ignores.
    let source = crate::execution::query_optional(
        tx,
        &copy_attributes_read_sql(from_id, &[COPY_CREATED_AT_COLUMN.to_string()]),
        |row| row.get::<_, Option<String>>(0),
    )
    .map_err(DbOperationError::sqlite)?;
    let Some(stored) = source else {
        return Ok(CopyAlbumCreatedAtOutcome::SourceNotFound);
    };

    // Go's poison guard (`:343-350`): `!v.Valid || v.String == "" ||
    // strings.HasPrefix(v.String, "0001-")`. `stored.is_none()` is the `!v.Valid` arm (a NULL
    // cell); the prefix arm catches a Go zero `time.Time` stored as TEXT, which is what
    // actually reaches this column in production.
    if stored
        .as_deref()
        .is_none_or(|text| text.is_empty() || text.starts_with("0001-"))
    {
        // Go: `continue` skips the column, `len(to) == 0`, `return nil` (`:351-353`). No
        // UPDATE runs, so the target keeps its own `created_at`.
        return Ok(CopyAlbumCreatedAtOutcome::SkippedPoisonedSource);
    }

    // Go's write arm (`:355-356`): `Update(table).SetMap(to).Where(Eq{"id": toID})`, whose
    // affected-row count `executeSQL` discards for this caller. A zero-row match (target
    // absent) is therefore success, exactly as in Go.
    crate::execution::execute(
        tx,
        &copy_attributes_update_sql(
            to_id,
            &[COPY_CREATED_AT_COLUMN.to_string()],
            vec![crate::sqlrepo::Value::Text(stored.expect("checked above"))],
        ),
    )
    .map_err(DbOperationError::sqlite)?;
    Ok(CopyAlbumCreatedAtOutcome::Copied)
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

/// `RefreshPlayCounts` (`album_repository.go:411-428`) EXECUTED on the CALLER's
/// transaction: the one zero-argument CTE upsert [`refresh_play_counts_sql`] renders,
/// run verbatim through [`crate::execution::execute`] — this is the executed half of that
/// renderer, not a second statement.
///
/// Go truth, SOURCE-VERIFIED at those lines: the CTE aggregates `sum(play_count)` /
/// `max(play_date)` over `media_file JOIN annotation ON item_id = media_file.id` grouped
/// by `(user_id, album_id)`, then inserts `item_type = 'album'` rows and
/// `on conflict (user_id, item_id, item_type) do update set play_count = excluded.play_count,
/// play_date = excluded.play_date`. Two consequences of that text are pinned by
/// `tests/runtime_count_refresh.rs`:
///
/// * **`rating`/`starred`/`starred_at`/`rated_at` survive.** The conflict arm names only
///   `play_count` and `play_date`, so an existing annotation's rating, star and their
///   stamps are untouched — a whole-row replacement would clobber them. `album.average_rating`
///   is likewise not written here (that is [`crate::sql_annotations::update_avg_rating_in_tx`]).
/// * **`where total_play_count > 0` is the only guard, and `play_date` has none.** Go's SET
///   carries no `max(ifnull(play_date,''), ?)` comparison of the kind
///   [`crate::sql_annotations::inc_play_count_sql`] uses (`sql_annotations.go:163-165`), so
///   this refresh DOES overwrite `play_date` with the aggregate `max(play_date)` even when
///   that is older than the stored value, and it never NULLs a `play_date` the aggregate
///   did not produce — a group whose `max(play_date)` is NULL is excluded by the `> 0`
///   filter only through its count, so a `play_count > 0` group with NULL dates writes a
///   NULL `play_date`. That is Go's behavior; it is NOT an idempotence guard.
/// * **An empty aggregate is 0, not NULL**: `where total_play_count > 0` cannot match NULL
///   (`NULL > 0` is NULL), so a population whose `sum(play_count)` is NULL inserts nothing,
///   and `max(play_date)` of no rows is NULL rather than an error. There is deliberately no
///   `coalesce` in Go's statement, and none is added.
///
/// Returns the statement-level affected-row count, i.e. Go's `executeSQL` `RowsAffected`
/// (`sql_base_repository.go:336-353`, which `RefreshPlayCounts` returns unchanged at
/// `:427`). It is NOT an entity count: SQLite counts an INSERT and an existing-row UPDATE
/// identically, and a group whose aggregate equals the stored row still counts as 1 change
/// (the upsert reports the change, not a no-op).
///
/// Zero bound arguments, so there is no variable-number limit to plan around: this statement
/// never needs chunking (contrast DB-18's four-fold 1000-id bind lists). Retryable — a rerun
/// recomputes the same aggregates and lands on the same values.
///
/// No production caller is wired here: Go's trigger is `phase_3_refresh_albums.go:138`
/// (inside `WithTxRetry` at `:136`), gated on `changesDetected` at `:129`, and the artist
/// twin is `:150`. Like DB-14/15/16's primitives this ships as a primitive only; SC's
/// phase-3 lane is the consumer.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact, so `store::is_busy` still classifies `SQLITE_BUSY`), including the hard
/// `malformed JSON` failure a damaged `participants`/`tags` cell can raise.
/// [`DbOperationError::Domain`] is structurally uninhabited ([`AlbumTouchError`]).
///
/// Divergence, deliberate: Go's caller wraps failures as
/// `fmt.Errorf("refreshing album annotations: %w", err)` (`phase_3_refresh_albums.go:142`),
/// so Go's message gains a prefix; the port keeps the raw `rusqlite::Error` on the `Sqlite`
/// arm (`decisions.md` E1). Message text differs, cause does not.
pub fn refresh_play_counts_in_tx(tx: &rusqlite::Transaction<'_>) -> Result<usize, AlbumTouchError> {
    crate::execution::execute(tx, &refresh_play_counts_sql()).map_err(DbOperationError::sqlite)
}

/// `RefreshPlayCounts` (`album_repository.go:411-428`): Go's literal CTE
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

/// `soleAlbumArtistFilter` (`album_repository.go:273-275`): the
/// `json_array_length(participants, '$.albumartist') = 1` fragment Go shares
/// between `SoleAlbumArtistFilter` and `GetSoleAlbumArtistIDsInSubtrees`.
pub const SOLE_ALBUM_ARTIST_FILTER: &str = "json_array_length(participants, '$.albumartist') = 1";

/// `SoleAlbumArtistFilter` (`album_repository.go:279-281`): the album-artist
/// semi-join AND the sole-artist fragment.
#[must_use]
pub fn sole_album_artist_filter_sql(artist_id: &str) -> Sql {
    let (sql, args) = participant_id_filter(ALBUM_TABLE, artist_id, &["albumartist"]);
    (format!("({sql} AND {SOLE_ALBUM_ARTIST_FILTER})"), args)
}

/// `GetSoleAlbumArtistIDsInSubtrees` (`album_repository.go:285-304`): the
/// chunked `json_each(album.folder_ids)` EXISTS semi-join onto `folder`
/// through `folderSubtreeFilter` (`folder_repository.go:210-227` →
/// `crate::folder_repo::folder_subtree_filter`), selecting the one
/// `albumartist[0].id`. Repeated ids across chunks are fine (the queue
/// upserts by PK, `:218`); `paths` chunks at 100
/// (`subtreePathChunkSize`, `folder_repository.go:204`).
///
/// DB-11 corrected the statement to carry NO album-`missing` predicate: Go's
/// `Where(And{soleAlbumArtistFilter, inSubtree})` (`album_repository.go:294-296`)
/// has none, so a missing album in a matching subtree IS reported — which is what
/// the image-change queue needs, since it must also see albums whose folders
/// vanished. The previous `AND album.missing = ?` over-restricted the read. The
/// only production caller of this builder is
/// [`crate::scanner_queries::sole_album_artist_ids_in_subtrees`], so the fix is
/// in place rather than in a parallel variant.
#[must_use]
pub fn get_sole_album_artist_ids_in_subtrees_sql(lib_id: i64, paths: &[String]) -> Vec<Sql> {
    const SUBTREE_PATH_CHUNK_SIZE: usize = 100;
    let mut result = Vec::new();
    for chunk in paths.chunks(SUBTREE_PATH_CHUNK_SIZE) {
        let (subtree_sql, subtree_args) = crate::folder_repo::folder_subtree_filter(lib_id, chunk);
        result.push((
            format!(
                "SELECT distinct json_extract(participants, '$.albumartist[0].id') \
                 FROM {ALBUM_TABLE} WHERE {SOLE_ALBUM_ARTIST_FILTER} AND \
                 EXISTS (SELECT 1 FROM json_each(album.folder_ids) je \
                 join folder on folder.id = je.value WHERE {subtree_sql})"
            ),
            subtree_args,
        ));
    }
    result
}

/// `GetTouchedAlbums` (`album_repository.go:392-403`): the album select with
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

/// `getAllIDs` (`album_repository.go:263-270`): the id-only projection
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

/// Failures of the executed album reads ([`AlbumRepo`]).
///
/// The variant set follows the sibling `UserRepo::UserError` (`user_repo.rs:609-622`)
/// and `PlayerRepo::PlayerError` (`player_repo.rs:243-260`): a statement/hydration
/// failure and the not-found case, nothing else.
#[derive(Debug, thiserror::Error)]
pub enum AlbumError {
    /// The SQLite statement failed, or a row's hydration failed. Go's read funnel
    /// surfaces a `PostScan` failure as a column-conversion error
    /// (`dbx@v1.12.0/rows.go:92-102`), which is why a decode failure arrives here
    /// too — via [`crate::hydrate::HydrateError::into_sqlite`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`). For
    /// [`AlbumRepo::get`] that is `GetAll` returning zero rows
    /// (`album_repository.go:239-241`), NOT `queryOne`'s `sql.ErrNoRows` mapping:
    /// Go's `Get` is `GetAll` + an `Eq` filter and decides from `len(res) == 0`.
    #[error("record not found")]
    NotFound,
}

/// `albumRepository` (`album_repository.go:105-122`) bound to one pooled
/// connection — the executed READ side of the album repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate
/// has no ambient context (the `share_repo.rs` DI freeze), so the caller threads a
/// lease from [`crate::store::SQLStore::with_connection_sync`] (a transaction
/// derefs to a `&Connection`) and every statement runs through [`crate::execution`]
/// — the port of `executeSQL`/`queryOne`/`queryAll`
/// (`sql_base_repository.go:336-355,375-388,441-457`). Rows are hydrated through
/// the shared seam exactly as `tests/runtime_hydrate_album_mediafile.rs:108-120`
/// does — [`crate::hydrate::row_to_album`] → [`AlbumRow::into_record`], with
/// [`crate::hydrate::HydrateError::into_sqlite`] as the exit shape — and artwork
/// hydration is the second layer,
/// [`crate::artwork_hydration::hydrate_album_artwork`] (`hydrateArtwork`,
/// `album_repository.go:257-260`).
///
/// **Why `get` goes through `query_all` and not `query_optional`:** Go's `Get`
/// (`:234-243`) is literally `GetAll` with `Eq{"album.id": id}` followed by
/// `if len(res) == 0 { return nil, model.ErrNotFound }`. It never reaches
/// `queryOne`, so `sql.ErrNoRows` and its mapping at
/// `sql_base_repository.go:382-385` are not this code path — and on this driver
/// `dbx`'s `All` cannot produce `sql.ErrNoRows` at all: it loops `Next` then
/// `Close` and makes an empty NON-nil slice for a zero-row result
/// (`dbx@v1.12.0/query.go:274-277`, `rows.go:109-125`), while only `Rows.one`/
/// `Rows.row` return it (`rows.go:237-244`, `:281-294`). That makes `queryAll`'s
/// ErrNoRows→ErrNotFound arm (`sql_base_repository.go:451-454`) dead code here.
/// So do NOT "fix" [`AlbumRepo::get_all`] to report [`AlbumError::NotFound`] for
/// zero rows: the empty `Vec` IS Go's behaviour.
#[derive(Debug)]
pub struct AlbumRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> AlbumRepo<'a> {
    /// `NewAlbumRepository` (`album_repository.go:105-122`): bind to `conn`
    /// (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`album_repository.go:234-243`): [`get_sql`] (== `GetAll` +
    /// `Eq{"album.id": id}`), hydrated through both layers, with a zero-row read
    /// mapped to [`AlbumError::NotFound`] the way Go decides it from `len(res)`.
    ///
    /// # Errors
    /// [`AlbumError::NotFound`] when the id matches nothing (also when the library
    /// filter excludes it); [`AlbumError::Sqlite`] on a statement or hydration
    /// failure. An artwork-lookup failure is NOT an error here — Go's `Get`
    /// swallows it too, because it delegates to `GetAll`, whose `hydrateItems` logs
    /// and returns an empty map (`artwork_hydration.go:62-66`): the album comes back
    /// with a zero (unresolved) [`crate::artwork_hydration::ItemImage`]. Use
    /// [`AlbumRepo::get_all`] when the caller needs that failure reported. Go's `Get`
    /// returns `nil` beside its error on BOTH paths (`:236-238` `return nil, err`,
    /// `:239-241` `return nil, model.ErrNotFound`), so the port's `Result` carries
    /// the same information and is not a divergence.
    pub fn get(
        &self,
        id: &str,
        user_id: Option<&str>,
        normalized_album_play_count: bool,
        library_filter: Option<Sql>,
    ) -> Result<AlbumRecord, AlbumError> {
        let statement = get_sql(id, user_id, normalized_album_play_count, library_filter);
        let records = self.read_hydrated(&statement)?;
        // Go hydrates the whole (0-or-1 element) slice, then takes res[0].
        let mut outcome = self.hydrate_artwork(records);
        if outcome.items.is_empty() {
            return Err(AlbumError::NotFound);
        }
        // Go's swallow-and-log path: the page still renders (artwork_hydration.go:62-66).
        let _ = outcome.artwork_error;
        Ok(outcome.items.remove(0))
    }

    /// `GetAll` (`album_repository.go:245-256`): [`get_all_sql`] under the
    /// caller's order-by and pagination, then artwork hydration. An empty result
    /// is an empty `Vec`, never [`AlbumError::NotFound`] — see [`AlbumRepo`]'s
    /// dbx proof.
    ///
    /// `artwork_error` carries Go's swallowed `log.Error`
    /// (`artwork_hydration.go:62-66`): the albums come back either way, each with
    /// a zero (unresolved) [`crate::artwork_hydration::ItemImage`], exactly as
    /// Go's empty map leaves them. That is why this returns `Ok` for an artwork
    /// failure and `Err` only for a statement or row-hydration failure
    /// (`album_repository.go:248-251` returns `queryAll`'s error unchanged).
    ///
    /// # Errors
    /// [`AlbumError::Sqlite`] when the statement fails or a row will not hydrate.
    pub fn get_all(
        &self,
        user_id: Option<&str>,
        normalized_album_play_count: bool,
        library_filter: Option<Sql>,
        order_by: Vec<String>,
        max: i64,
        offset: i64,
    ) -> Result<crate::artwork_hydration::ReadOutcome<AlbumRecord>, AlbumError> {
        let statement = get_all_sql(
            user_id,
            normalized_album_play_count,
            library_filter,
            order_by,
            max,
            offset,
        );
        let records = self.read_hydrated(&statement)?;
        Ok(self.hydrate_artwork(records))
    }

    /// Phase 1 + phase 2 (Go's `queryAll` + `toModels()`): the raw rows through
    /// the shared seam, shim columns decoded. A `PostScan` failure aborts the
    /// whole read, as it does in Go.
    fn read_hydrated(&self, statement: &Sql) -> Result<Vec<AlbumRecord>, AlbumError> {
        let rows = crate::execution::query_all(self.conn, statement, crate::hydrate::row_to_album)?;
        // Go's read funnel leaves a PostScan failure as a column-conversion error,
        // so `into_sqlite` is the exit shape and `AlbumError::Sqlite` wraps it.
        Ok(rows
            .into_iter()
            .map(AlbumRow::into_record)
            .collect::<Result<Vec<AlbumRecord>, crate::hydrate::HydrateError>>()
            .map_err(crate::hydrate::HydrateError::into_sqlite)?)
    }

    /// Phase 3 (`hydrateArtwork`, `album_repository.go:257-260`): one batched
    /// `al`-kind lookup over the hydrated records.
    fn hydrate_artwork(
        &self,
        records: Vec<AlbumRecord>,
    ) -> crate::artwork_hydration::ReadOutcome<AlbumRecord> {
        let mut items = records;
        let mut artwork_error = None;
        crate::artwork_hydration::hydrate_album_artwork(self.conn, &mut items, &mut artwork_error);
        crate::artwork_hydration::ReadOutcome {
            items,
            artwork_error,
        }
    }
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
        // Arrange: selectAlbum (:227-232): library LEFT join then
        // withAnnotation (sql_annotations.go:63-91); join args bind first.
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
        // Assert: with BOTH a library filter and the join, squirrel's render order
        // is FROM -> join -> WHERE and the join's args precede the filter's
        // (`select.go:92-118`); Go applies the filter first
        // (`album_repository.go:193-200`). Executed against the real schema by
        // `tests/runtime_hydrate_album_mediafile.rs::album_light_queries_bind_join_first`.
        let (both, both_args) = count_all_sql(Some("u1"), false, lib_filter(), true);
        assert_eq!(
            both,
            "SELECT count(distinct album.id) as count FROM album \
             LEFT JOIN annotation ON (annotation.item_id = album.id \
             AND annotation.item_type = ? AND annotation.user_id = ?) \
             WHERE album.library_id IN (SELECT ul.library_id FROM user_library ul \
             WHERE ul.user_id = ?)"
        );
        assert_eq!(
            both_args,
            vec![
                Value::Text("album".to_string()),
                Value::Text("u1".to_string()),
                Value::Text("user1".to_string()),
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
            // Read-path identity; the write legs take the id as their own argument.
            id: "al-1".to_string(),
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
            // Hydration-only fields: an unwritten test record keeps them empty, and
            // the record-driven write tests assert they never reach the map.
            library_path: String::new(),
            library_name: String::new(),
            annotation: nd_model::annotation::Annotation::default(),
            genres: Vec::new(),
            // S-D3c read-path-only artwork state (Go's embedded ItemImage,
            // structs:"-"); never a write-map key.
            image: crate::artwork_hydration::ItemImage::default(),
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
        let stmts = touch_sql(&ids, &crate::time::zero());
        let (mf_sql, mf_args) = touch_by_missing_folder_sql(&crate::time::zero());
        // Assert:
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].1[0], crate::time::value(&crate::time::zero()));
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
            vec![crate::time::value(&crate::time::zero()), Value::Bool(true)]
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
        // Arrange: RefreshPlayCounts (:411-428) is a fixed literal.
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
        // Arrange: GetTouchedAlbums (:392-397): Eq{library.id} and the
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
