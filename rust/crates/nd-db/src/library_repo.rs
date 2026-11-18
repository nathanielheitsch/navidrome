//! Port of `persistence/library_repository.go` (329L) — the `library`
//! table plus its `user_library` admin-assignment join.
//!
//! Every Go method builds a squirrel statement (or hands one to dbx's
//! `Model(l).Insert()`) and materializes it through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`. The WRITE legs stop at
//! those [`Sql`] pairs: execution, `WithTx` and logging stay with the caller,
//! following the sibling-lane split (see `album_repo.rs`). The READ legs are
//! EXECUTED: [`LibraryRepo`] is `libraryRepository` bound to one leased
//! connection and runs through [`crate::execution`], the port of
//! `queryOne`/`queryAll`/`count` (`sql_base_repository.go:336-355,375-388,
//! 441-457,565-576`), in the shape `radio_repo::RadioRepo` establishes.
//!
//! Ported here: `Get`'s id select (`:35-40`), `GetPath` (`:42-67`),
//! `GetAll`/`CountAll` (`:283-292`), `ScanInProgress` (`:190-194`),
//! `GetUsersWithLibraryAccess` (`:297-307`), `Put`'s update-then-insert legs plus
//! the admin user-library CROSS JOIN upsert (`:72-127`), `StoreMusicFolder`'s
//! config-change guard (`:138-152`), `AddArtist`'s library_artist insert
//! (`:152-162`), the scan lifecycle updates `ScanBegin`/`ScanEnd`/`SetScannedPID`
//! (`:162-189`), `RefreshStats`'s eight aggregate probes and its totals update
//! (`:196-251`) and `Delete`'s row delete (`:253-270`).
//!
//! Not ported, with the reason: Go's `libCache`/`libLock` PROCESS cache
//! (`:25-29`, written by `GetPath` `:56-59` and `Put` `:126`) is runtime state with
//! no SQL counterpart, so [`LibraryRepo::get_path`] re-runs its lookup on every
//! call; its doc records the staleness divergence that produces. `Put`'s
//! `ErrValidation` guard on library 1's path and `Delete`'s admin/ID-1 checks
//! (`:73-80`, `:254-259`) are auth-layer policy the caller applies before issuing
//! the statements here; `Delete`'s artist-orphan reconcile and plugin-reference
//! cleanup (`:262-269`) belong to the artist/plugin lanes; the REST shims
//! `Count`/`Read`/`ReadAll` (`:311-326`) are option parsing on top of
//! [`count_all_sql`]/[`get_all_query_sql`], pinned by [`rest_repo`] and
//! `sql_restful.rs`; `Read`'s `strconv.Atoi` of a string id (`:316-321`) is the
//! REST lane's concern. [`LibraryRepo`] exposes the executed read side of
//! `Get`/`GetPath`/`GetAll`/`CountAll`/`ScanInProgress`/
//! `GetUsersWithLibraryAccess`.
//!
//! Timestamps: Go binds `time.Time`, which go-sqlite3 renders as TEXT using the
//! first of its `SQLiteTimestampFormats` (`sqlite3.go:331-338`, `:2265-2272`); the
//! executed trace of Go's `ScanBegin`/`ScanEnd` binds
//! `2026-11-17 23:40:58.976772 -0500 EST` and `0001-01-01 00:00:00 +00:00 UTC`.
//! This repository's WRITE legs still bind epoch-second integers — a documented
//! divergence from [`crate::time`]'s TEXT convention, listed as remaining work
//! there — with [`ZERO_TIME`] as the zero `time.Time` constant [`scan_end_sql`]
//! writes (`:171-179`). Converting it is a separate lane, and both sides must move
//! together: `library` timestamps feed `artist`'s `RefreshStats` comparison
//! (`artist_repository.go:479`). The READ side decodes exactly as Go does — a
//! declared-`datetime` column whose INTEGER cell goes through
//! [`crate::hydrate::driver_integer_dt`] — so it is parity for either storage
//! spelling (executed Go: integer `1704067200` reads as 2024-01-01 and integer `0`
//! as 1970-01-01, NOT Go's zero time). The one place the integer-write convention
//! changes a READ result is [`scan_in_progress_sql`]'s comparison bound; see that
//! function and the executed test
//! `scan_in_progress_agrees_with_the_stamps_this_port_writes`.
//!
//! Safety rule (`sqlrepo.rs`): identifiers (table/column names) are this
//! repository's registered constants; every value binds to a `?`.
//!
//! Sort: `GetAll` HAS a real ORDER BY leg — Go's `newSelect` calls `applyOptions`
//! (`sql_base_repository.go:150-162`), which appends `buildSortOrder`'s
//! direction-carrying expression (`:199-228`). [`get_all_sql`] keeps its
//! caller-supplied expressions for that leg; [`get_all_query_sql`] derives the leg
//! from parsed options through [`build_sort_order`], the local port of that
//! splitter over [`SORT_MAPPINGS`] — which is EMPTY, because `libraryRepository`
//! never calls `setSortMappings` (`grep -n setSortMappings
//! persistence/library_repository.go` is empty), so every sort field reaches
//! ORDER BY as its snake-cased column name.
//!
//! `count` APPENDS `ORDER BY library.id` rather than replacing the caller's sort:
//! squirrel's `OrderBy` is `builder.Append` (`squirrel@v1.5.4/select.go:362-373`),
//! and `count` only calls `RemoveOffset`/`RemoveLimit` — never `RemoveOrderBy` —
//! before appending (`sql_base_repository.go:565-570`, despite its comment "To
//! remove any ORDER BY clause"). Executed Go traces (oracle `/tmp/sd3g1_oracle4`,
//! `/tmp/sd3g1_oracle5`): `CountAll(Sort:"name desc", Order:"asc")` →
//! `… ORDER BY name desc, library.id`; `CountAll(Sort:"totalSongs", Order:"desc")`
//! → `… ORDER BY total_songs desc, library.id`; `CountAll()` →
//! `… ORDER BY library.id`. So [`count_all_sql`] renders the reset leg alone and
//! [`count_all_query_sql`] renders the caller's leg FOLLOWED by it. An earlier
//! bullet here claimed `count` replaces the caller sort and traced only
//! `ORDER BY library.id` for a sorted call; the executed trace contradicts that,
//! and the claim is corrected here.
//!
//! `optimizePagination` (`sql_base_repository.go:475-486`) is NOT ported. It is
//! gated on `conf.Server.DevOffsetOptimize`, whose Go default is 50000: the field is
//! declared at `conf/configuration.go:145` and given
//! `viper.SetDefault("devoffsetoptimize", 50000)` at `:1129`, and the Rust port carries
//! `dev_offset_optimize: 50000` (`nd-conf/src/config.rs:746`). The rewrite therefore
//! only fires for `Offset > 50000` (`:478`), so an ordinary offset reaches plain
//! `SELECT * FROM library OFFSET 5` / `OFFSET 100` — which is what an executed
//! repository run at the default traced, never the rowid rewrite. Omitting it is
//! parity for every offset a test can reach; offsets above 50000 would take Go's
//! rewrite and are NOT ported (reported, not implemented — this slice is documentation
//! only, see [`get_all_query_sql`]).
//!
//! Clause order for a filtered, sorted page is `WHERE … ORDER BY … LIMIT … OFFSET …`
//! in both languages: `newSelect` runs `applyOptions` (which appends ORDER BY and
//! the bounds) BEFORE `applyFilters` (`sql_base_repository.go:141-145`), but squirrel
//! renders clauses by KIND, not by call order, so the executed trace is
//! `SELECT * FROM library WHERE library.name = ? ORDER BY name desc LIMIT 2 OFFSET 1`
//! — matching [`Repository::to_sql`]'s fixed order. The REST form traces
//! `WHERE (name LIKE ? AND path LIKE ?) ORDER BY name desc LIMIT 3`, i.e. one
//! parenthesized AND-group of the parsed filters ([`rest_repo`]).

use crate::sql_restful::RestRepository;
use crate::sqlrepo::{Repository, Value};
use crate::user_repo::UserRecord;
use chrono::{DateTime, Utc};

/// The `library` table (`NewLibraryRepository`, `library_repository.go:26`).
pub const TABLE: &str = "library";

/// The `user_library` join table (`Put`'s admin assignment, `:94-103`).
pub const USER_LIBRARY_TABLE: &str = "user_library";

/// The `library_artist` join table (`AddArtist`, `:135-140`).
pub const LIBRARY_ARTIST_TABLE: &str = "library_artist";

/// `model.DefaultLibraryID` (`model/library.go:68`): the row `Put` guards
/// and `StoreMusicFolder` rewrites.
pub const DEFAULT_LIBRARY_ID: i64 = 1;

/// Go's zero `time.Time` (`time.Time{}`) as the crate's epoch-seconds unit:
/// the stamp `ScanEnd` writes and `ScanInProgress` compares against.
pub const ZERO_TIME: i64 = 0;

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `model.Library` (`model/library.go:15-36`): every column the table declares,
/// in struct order, with Go's non-pointer field types. There is no shim column and
/// no `structs:"-"` read-path embed on this model (`grep -n 'structs:"-"'
/// model/library.go` is empty — every field carries a tag), so the record is exactly
/// the SELECT's column set and [`crate::hydrate::row_to_library`] fills every field
/// from the row.
///
/// Go's `int` fields are 64-bit, so the counters stay `i64` rather than narrowing to
/// `i32`: `RowGet::get_i32` would report out-of-range on a cell Go scans happily.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryRecord {
    /// `library.id` — Go's `int`, the autoincrement primary key.
    pub id: i64,
    /// `library.name` (unique).
    pub name: String,
    /// `library.path` (unique).
    pub path: String,
    /// `library.remote_path`.
    pub remote_path: String,
    /// `library.last_scan_at` — `ScanEnd`'s stamp.
    pub last_scan_at: DateTime<Utc>,
    /// `library.last_scan_started_at` — `ScanBegin`'s stamp, reset by `ScanEnd`.
    pub last_scan_started_at: DateTime<Utc>,
    /// `library.full_scan_in_progress`.
    pub full_scan_in_progress: bool,
    /// `library.updated_at`.
    pub updated_at: DateTime<Utc>,
    /// `library.created_at`.
    pub created_at: DateTime<Utc>,
    /// `library.total_songs`.
    pub total_songs: i64,
    /// `library.total_albums`.
    pub total_albums: i64,
    /// `library.total_artists`.
    pub total_artists: i64,
    /// `library.total_folders`.
    pub total_folders: i64,
    /// `library.total_files`.
    pub total_files: i64,
    /// `library.total_missing_files`.
    pub total_missing_files: i64,
    /// `library.total_size` (Go `int64`).
    pub total_size: i64,
    /// `library.total_duration` (Go `float64`).
    pub total_duration: f64,
    /// `library.default_new_users`.
    pub default_new_users: bool,
    /// `library.pid_album`.
    pub pid_album: String,
    /// `library.pid_track`.
    pub pid_track: String,
    /// `library.scanned_pid_album` (Go `json:"-"`, still scanned by field name).
    pub scanned_pid_album: String,
    /// `library.scanned_pid_track`.
    pub scanned_pid_track: String,
}

/// `model.Libraries` (`model/library.go:70`).
pub type Libraries = Vec<LibraryRecord>;

/// `Get` (`:35-40`): `newSelect(ctx).Columns("*").Where(Eq{"id": id})`.
#[must_use]
pub fn get_sql(id: i64) -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE id = ?"),
        vec![Value::Integer(id)],
    )
}

/// `GetAll` (`:283-287`): `newSelect(ctx, ops...).Columns("*")` with the
/// caller's order-by expressions (already direction-carrying, per
/// `buildSortOrder` output) and `applyOptions`' Max/Offset. Filters cannot be
/// expressed on this builder-free form; use [`get_all_query_sql`] for the full leg
/// set Go's `newSelect` applies.
#[must_use]
pub fn get_all_sql(order_by: &[&str], max: i64, offset: i64) -> Sql {
    let mut sql = format!("SELECT * FROM {TABLE}");
    // squirrel binds nothing for LIMIT/OFFSET (`select.go:139-147`), and this
    // select has no other clauses, so the arg list stays empty.
    let args: Vec<Value> = Vec::new();
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

/// `CountAll` (`:290-292`) via the base `count()`
/// (`sql_base_repository.go:565-576`): `count(distinct library.id) as count` with
/// pagination dropped and the reset `ORDER BY library.id` leg `count` APPENDS.
///
/// Executed Go (oracle `/tmp/sd3g1_oracle4`) traces that leg on the bare call:
/// `SELECT count(distinct library.id) as count FROM library ORDER BY library.id`.
/// An earlier version of this fn rendered no ORDER BY at all; the executed trace
/// contradicted it and the leg is now emitted, matching
/// [`crate::radio_repo::count_all_sql`]'s precedent. A caller sort is NOT
/// expressible on this builder-free form — see [`count_all_query_sql`], which
/// renders Go's executed `… ORDER BY <caller leg>, library.id`.
#[must_use]
pub fn count_all_sql() -> Sql {
    (
        format!("SELECT count(distinct {TABLE}.id) as count FROM {TABLE} ORDER BY {TABLE}.id"),
        Vec::new(),
    )
}

/// `registerModel(&model.Library{}, nil)`'s rest-facing half
/// (`library_repository.go:30` → `sql_base_repository.go:101-106` plus
/// `registerModelWhiteList`/`registerFieldWhiteList` at `sql_restful.go:142-163`):
/// the `library` table with NO filter mappings — Go's second `registerModel`
/// argument is `nil`, so there is no `name`/`library_id` custom filter here and every
/// filter falls through to the whitelist arm — and the whitelist of `model.Library`'s
/// struct field names plus the annotation fields (`sql_restful.go:159-162`).
///
/// The whitelist is what makes Go's executed warning
/// `Ignoring filter not whitelisted filter=bogus table=library` drop that filter
/// outright (`sql_restful.go:33-36`), and a whitelisted field ending in `id` takes the
/// exact-match arm while everything else takes "starts with" (`:38-44`). Executed
/// (oracle `/tmp/sd3g1_opt`): `Filters:{"name":"et"}` renders `WHERE (name LIKE ?)`
/// bound to `et%` and matched NOTHING (no library name starts with "et"), while
/// `Filters:{"id":"2"}` renders `WHERE (id = ?)` bound to the TEXT `"2"` — Go's value
/// stays a `string` there, so the bind is text and SQLite's column affinity does the
/// comparison. Both row sets are pinned by the executed tests.
#[must_use]
pub fn rest_repo() -> RestRepository {
    RestRepository::new(TABLE).with_field_white_list([
        "ID",
        "Name",
        "Path",
        "RemotePath",
        "LastScanAt",
        "LastScanStartedAt",
        "FullScanInProgress",
        "UpdatedAt",
        "CreatedAt",
        "TotalSongs",
        "TotalAlbums",
        "TotalArtists",
        "TotalFolders",
        "TotalFiles",
        "TotalMissingFiles",
        "TotalSize",
        "TotalDuration",
        "DefaultNewUsers",
        "PIDAlbum",
        "PIDTrack",
        "ScannedPIDAlbum",
        "ScannedPIDTrack",
    ])
}

/// `setSortMappings` for this repository: NONE. `libraryRepository` never calls it
/// (`grep -n setSortMappings persistence/library_repository.go` is empty), so
/// `sortMapping` resolves every field through its snake_case fallback
/// (`sql_base_repository.go:165-185`). Kept as an explicit constant so the empty map
/// is a pinned fact rather than an accident.
pub const SORT_MAPPINGS: [(&str, &str); 0] = [];

/// `lookupSortMapping` over this repository's (empty) map
/// (`sql_base_repository.go:187-197`).
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort)
        .map(|(_, value)| *value)
}

/// `sortMapping` (`sql_base_repository.go:165-185`): resolve each comma-separated
/// part through [`sort_mapping`], snake-casing the parts it does not know, so a mix of
/// mapped keys and plain columns keeps the mappings it recognizes.
#[must_use]
pub fn mapped_sort(sort: &str) -> String {
    crate::sql_restful::split_sort_fields(sort, ',')
        .iter()
        .map(|part| {
            let part = part.trim();
            match sort_mapping(part) {
                Some(mapping) => mapping.to_string(),
                None => crate::sql_restful::to_snake_case(part),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `buildSortOrder` (`sql_base_repository.go:199-228`): resolve the sort through
/// [`mapped_sort`], fold the caller's direction, and flip a part that spells its own
/// direction. Local rather than reused from [`crate::player_repo::build_sort_order`],
/// which resolves THE PLAYER's mappings (`player_repo.rs:557`) and would mis-map a
/// library sort — Go's splitter is per-repository for exactly that reason.
#[must_use]
pub fn build_sort_order(sort: &str, order: &str) -> String {
    let order = order.trim().to_lowercase();
    let (order, reverse) = if order == "desc" {
        ("desc".to_string(), "asc".to_string())
    } else {
        ("asc".to_string(), "desc".to_string())
    };
    crate::sql_restful::split_sort_fields(&mapped_sort(sort), ',')
        .iter()
        .map(|part| {
            let fields = crate::sql_restful::split_sort_fields(part, ' ');
            let field = fields.first().map(String::as_str).unwrap_or("");
            let direction = match fields.get(1).map(String::as_str) {
                None | Some("asc") => order.clone(),
                Some(_) => reverse.clone(),
            };
            format!("{field} {direction}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `sanitizeSort` for the library REST path (`sql_restful.go:47-60` via
/// `RestRepository::sanitize_sort`, plus the per-repository `lookupSortMapping` arm
/// Go validates against at `:52-54`). The mapping arm is vacuous here because
/// [`SORT_MAPPINGS`] is empty, so a sort field survives only when it is a whitelisted
/// field name. Executed (oracle `/tmp/sd3g1_opt`): `Sort:"totalSongs"` survives and
/// traces `ORDER BY total_songs desc`; `Sort:"bogus"` is dropped and traces no ORDER
/// BY at all beside an `Ignoring sort not whitelisted` warning.
#[must_use]
pub fn sanitize_sort(sort: &str, order: &str) -> (String, String) {
    let (whitelisted, order) = rest_repo().sanitize_sort(sort, order);
    if sort.is_empty() {
        return (String::new(), order);
    }
    let snake = crate::sql_restful::to_snake_case(sort);
    let kept = if sort_mapping(&snake).is_some() || !whitelisted.is_empty() {
        snake
    } else {
        String::new()
    };
    (kept, order)
}

/// `parseRestOptions` for this repository (`sql_restful.go:31-45`): the parsed
/// `QueryOptions` that [`get_all_query_sql`] and [`count_all_query_sql`] consume — the
/// port of `ReadAll`/`Count` (`:311-326`) minus `Read`'s `strconv.Atoi`.
#[must_use]
pub fn parse_rest_options(
    options: Option<&crate::sql_restful::RestQueryOptions>,
) -> crate::sql_restful::QueryOptions {
    let Some(options) = options else {
        return crate::sql_restful::QueryOptions::default();
    };
    let (sort, order) = sanitize_sort(&options.sort, &options.order);
    crate::sql_restful::QueryOptions {
        sort,
        order,
        max: options.max,
        offset: options.offset,
        seed: options.filters.get("seed").cloned(),
        filters: rest_repo().parse_rest_filters(options),
    }
}

/// `GetAll` (`:283-287`) driven by a parsed [`crate::sql_restful::QueryOptions`]: the
/// builder path, so all three legs Go's `newSelect` applies are expressible —
/// `applyOptions`' Max/Offset, the `buildSortOrder` leg, and `applyFilters`
/// (`sql_base_repository.go:141-145`, `:149-162`, `:247-252`).
///
/// Clause order is squirrel's by-kind order, which is Go's executed order: filters
/// render in WHERE ahead of the ORDER BY leg even though Go called `applyOptions`
/// first. Executed traces: `SELECT * FROM library WHERE library.name = ? ORDER BY
/// name desc LIMIT 2 OFFSET 1`, and the REST form's parenthesized
/// `WHERE (name LIKE ? AND path LIKE ?) ORDER BY name desc LIMIT 3`.
///
/// `optimizePagination` stays unported (module header): at Go's default
/// `DevOffsetOptimize = 50000` (`conf/configuration.go:1129`, mirrored by
/// `nd-conf/src/config.rs:746`) the rowid rewrite only fires above that offset, so
/// every page this renderer is tested with takes the plain `OFFSET` path.
///
/// Deviation kept, not fixed: an `Offset` with no `Max` renders `LIMIT -1 OFFSET n`
/// ([`Repository::to_sql`], `sqlrepo.rs:313-319`), whereas Go's bare `OFFSET n` is a
/// SQLite parse error — executed Go returns `near "2": syntax error` (oracle
/// `/tmp/sd3g1_oracle2`). The port EXECUTES that page and skips rows.
pub fn get_all_query_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = Repository::new(TABLE).select(["*"]);
    if let Some(options) = options {
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
        sq = crate::sql_search::apply_pagination(sq, options);
        sq = crate::sql_search::apply_filters(sq, options);
    }
    sq.to_sql()
}

/// `CountAll` (`:290-292`) driven by a parsed [`crate::sql_restful::QueryOptions`].
///
/// `count` drops the columns and bounds and APPENDS `ORDER BY library.id` after any
/// caller leg (squirrel `OrderBy` is `builder.Append`, `select.go:362-373`); filters
/// survive through `applyFilters` (`:571`) and `Max`/`Offset` are removed by
/// `RemoveOffset`/`RemoveLimit` (`:566-567`), so a page changes nothing. Executed
/// (oracle `/tmp/sd3g1_oracle2`): `CountAll(filter name=Beta, Max:1, Offset:3,
/// Sort:"name desc")` traces `… WHERE library.name = ? ORDER BY name desc, library.id`,
/// and `CountAll(Sort:"totalSongs", Order:"desc")` traces `… ORDER BY total_songs desc,
/// library.id`.
pub fn count_all_query_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let caller_leg = options
        .filter(|o| !o.sort.is_empty())
        .map(|o| build_sort_order(&o.sort, &o.order));
    let mut sq = Repository::new(TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    // `count` resets columns and bounds (`sql_base_repository.go:565-567`), which
    // `Repository::count` reproduces, then appends its own leg AFTER the caller's.
    sq = sq.count(TABLE);
    let mut legs = Vec::new();
    if let Some(leg) = caller_leg {
        legs.push(leg);
    }
    legs.push(format!("{TABLE}.id"));
    sq.order_by(legs).to_sql()
}

/// The `Put` update-map payload (`library_repository.go:76-82`): the six
/// columns `selectUpdateColumns` filters plus the `updated_at` Go always
/// appends (`:83`). `Put` also stamps `CreatedAt`/`UpdatedAt` and the
/// ID-1 path validation (`:59-71`) — those are the caller's clock/policy.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryUpdate<'a> {
    /// `library.name`.
    pub name: &'a str,
    /// `library.path`.
    pub path: &'a str,
    /// `library.remote_path`.
    pub remote_path: &'a str,
    /// `library.default_new_users`.
    pub default_new_users: bool,
    /// `library.pid_album`.
    pub pid_album: &'a str,
    /// `library.pid_track`.
    pub pid_track: &'a str,
    /// `library.updated_at` (`l.UpdatedAt = time.Now()`, `:74`).
    pub updated_at: i64,
}

impl LibraryUpdate<'_> {
    /// The `selectUpdateColumns` map (`:76-82`) plus `updated_at` (`:83`),
    /// restricted to `cols_to_update` (snake_cased by the caller) — all
    /// columns when none are requested. `updated_at` always survives.
    fn set_pairs(&self, cols_to_update: &[&str]) -> Vec<(&'static str, Value)> {
        let all: Vec<(&'static str, Value)> = vec![
            ("name", Value::from(self.name)),
            ("path", Value::from(self.path)),
            ("remote_path", Value::from(self.remote_path)),
            ("default_new_users", Value::Bool(self.default_new_users)),
            ("pid_album", Value::from(self.pid_album)),
            ("pid_track", Value::from(self.pid_track)),
            ("updated_at", Value::Integer(self.updated_at)),
        ];
        if cols_to_update.is_empty() {
            return all;
        }
        all.into_iter()
            .filter(|(col, _)| *col == "updated_at" || cols_to_update.contains(col))
            .collect()
    }
}

/// `Put`'s UPDATE leg (`:85-87`): `Update(library).SetMap(cols).Where(Eq{"id":
/// l.ID})` — squirrel's `SetMap` renders the SET pairs in alphabetical key
/// order (`squirrel@v1.5.4/update.go:237`). Only the requested columns set;
/// zero rows affected is the caller's insert fallback (`:96-101`).
#[must_use]
pub fn put_update_sql(id: i64, update: &LibraryUpdate, cols_to_update: &[&str]) -> Sql {
    let mut pairs = update.set_pairs(cols_to_update);
    pairs.sort_by_key(|(col, _)| *col);
    let set = pairs
        .iter()
        .map(|(col, _)| format!("{col} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let args = pairs
        .iter()
        .map(|(_, value)| value.clone())
        .chain([Value::Integer(id)])
        .collect();
    (format!("UPDATE {TABLE} SET {set} WHERE id = ?"), args)
}

/// `Put`'s INSERT legs (`:78-79`, `:96-101` — `r.db.Model(l).Insert()`):
/// the row columns in `model.Library`'s declaration order (`model/library.go:15-36`,
/// the order dbx's reflection renders), with `id` bound first when the row
/// carries one (the ID-999 insert case, `library_repository_test.go:113-137`)
/// and omitted for the autoincrement path (`:77-79`).
pub const INSERT_COLUMNS: [&str; 21] = [
    "name",
    "path",
    "remote_path",
    "last_scan_at",
    "last_scan_started_at",
    "full_scan_in_progress",
    "updated_at",
    "created_at",
    "total_songs",
    "total_albums",
    "total_artists",
    "total_folders",
    "total_files",
    "total_missing_files",
    "total_size",
    "total_duration",
    "default_new_users",
    "pid_album",
    "pid_track",
    "scanned_pid_album",
    "scanned_pid_track",
];

/// The INSERT with `id` bound first when given; `values` pair one-to-one with
/// [`INSERT_COLUMNS`].
#[must_use]
pub fn insert_sql(id: Option<i64>, values: Vec<Value>) -> Sql {
    let mut columns: Vec<&str> = Vec::with_capacity(INSERT_COLUMNS.len() + 1);
    let mut args = Vec::with_capacity(values.len() + 1);
    if let Some(id) = id {
        columns.push("id");
        args.push(Value::Integer(id));
    }
    columns.extend(INSERT_COLUMNS);
    args.extend(values);
    let placeholders = vec!["?"; args.len()].join(", ");
    let cols = columns.join(", ");
    (
        format!("INSERT INTO {TABLE} ({cols}) VALUES ({placeholders})"),
        args,
    )
}

/// `Put`'s admin assignment (`:105-111`): every admin user gains every
/// library, idempotently. A `Sql` with no bound args — all identifiers;
/// `ON CONFLICT … DO NOTHING` keeps re-`Put`s from duplicating rows.
#[must_use]
pub fn assign_admins_sql() -> Sql {
    (
        "INSERT INTO user_library (user_id, library_id)\nSELECT u.id, l.id\nFROM user u\nCROSS JOIN library l\nWHERE u.is_admin = true\nON CONFLICT (user_id, library_id) DO NOTHING;".to_string(),
        Vec::new(),
    )
}

/// `StoreMusicFolder` (`:117-133`): re-point library 1 at the configured
/// music folder — `Set("path", …).Set("updated_at", …)` in call order, gated
/// by `NotEq{"path": …}` so an unchanged config writes nothing (the
/// `rowsAffected > 0` cache update is the caller's).
#[must_use]
pub fn store_music_folder_sql(path: &str, updated_at: i64) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET path = ?, updated_at = ? \
             WHERE id = ? AND path <> ?"
        ),
        vec![
            Value::from(path),
            Value::Integer(updated_at),
            Value::Integer(DEFAULT_LIBRARY_ID),
            Value::from(path),
        ],
    )
}

/// `AddArtist` (`:135-145`): the library↔artist link, idempotent on its
/// unique key.
#[must_use]
pub fn add_artist_sql(id: i64, artist_id: &str) -> Sql {
    (
        format!(
            "INSERT INTO {LIBRARY_ARTIST_TABLE} (library_id, artist_id) VALUES (?, ?) \
             on conflict(library_id, artist_id) do nothing"
        ),
        vec![Value::Integer(id), Value::from(artist_id)],
    )
}

/// `ScanBegin` (`:147-153`): stamp the scan start and its mode.
#[must_use]
pub fn scan_begin_sql(id: i64, now: i64, full_scan: bool) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET last_scan_started_at = ?, full_scan_in_progress = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(now),
            Value::Bool(full_scan),
            Value::Integer(id),
        ],
    )
}

/// `ScanEnd` (`:155-168`): stamp the finish, clear the flag and reset the
/// start stamp to the zero time.
#[must_use]
pub fn scan_end_sql(id: i64, now: i64) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET last_scan_at = ?, full_scan_in_progress = ?, \
             last_scan_started_at = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(now),
            Value::Bool(false),
            Value::Integer(ZERO_TIME),
            Value::Integer(id),
        ],
    )
}

/// `SetScannedPID` (`:170-178`): record the PID specs the finished scan used.
#[must_use]
pub fn set_scanned_pid_sql(id: i64, pid_album: &str, pid_track: &str) -> Sql {
    (
        format!("UPDATE {TABLE} SET scanned_pid_album = ?, scanned_pid_track = ? WHERE id = ?"),
        vec![
            Value::from(pid_album),
            Value::from(pid_track),
            Value::Integer(id),
        ],
    )
}

/// `ScanInProgress` (`:190-194`): `NotEq` on `last_scan_started_at` against the zero
/// `time.Time` — the zero-time constant must match the one [`scan_end_sql`] writes,
/// or a finished scan would read as in-progress.
///
/// This is the statement Go builds BEFORE `count()` replaces its columns, bounds and
/// order (`sql_base_repository.go:565-570`), so it is the intermediate select, not
/// what reaches SQLite. The executed form is [`scan_in_progress_count_sql`], which is
/// what [`LibraryRepo::scan_in_progress`] runs.
///
/// **Write-side hazard, recorded not fixed:** Go binds Go's zero `time.Time`, which
/// go-sqlite3 renders as TEXT (`0001-01-01 00:00:00 +0000 UTC`, executed trace in
/// oracle `/tmp/sd3g1_oracle4`). This port binds the INTEGER `ZERO_TIME` instead —
/// the same convention `scan_end_sql` writes, so the two agree WITHIN the port. A
/// row stamped by the Go binary with the TEXT zero time is still `<> 0` for SQLite,
/// which orders TEXT above INTEGER by storage class, so the port would report it as
/// in-progress. Executed on the Go side: with every row set to INTEGER 0, Go's
/// `ScanInProgress` returned `true`, because its bound is the TEXT zero time and
/// `0 <> '0001-01-01 00:00:00'` is true — the mirror image of the same fault. Moving
/// both sides is the timestamp lane's job (see the module header).
#[must_use]
pub fn scan_in_progress_sql() -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE last_scan_started_at <> ?"),
        vec![Value::Integer(ZERO_TIME)],
    )
}
/// `ScanInProgress` as EXECUTED (`:190-194` through `count`,
/// `sql_base_repository.go:565-576`): `count(distinct library.id) as count` over the
/// not-equal predicate, plus the reset `ORDER BY library.id` leg `count` appends (this
/// path passes no caller sort, so nothing precedes it). Executed Go text (oracle
/// `/tmp/sd3g1_oracle4`): `SELECT count(distinct library.id) as count FROM library
/// WHERE last_scan_started_at <> ? ORDER BY library.id`.
#[must_use]
pub fn scan_in_progress_count_sql() -> Sql {
    (
        format!(
            "SELECT count(distinct {TABLE}.id) as count FROM {TABLE} \
             WHERE last_scan_started_at <> ? ORDER BY {TABLE}.id"
        ),
        vec![Value::Integer(ZERO_TIME)],
    )
}

/// `RefreshStats` (`:183-240`): the eight aggregate probes, in
/// `run.Parallel`'s declaration order (`:199-236`). The `Eq` maps render
/// their two keys alphabetically (`library_id` < `missing`), and the folder
/// count's `And` nests the `Gt` clause last (`:221-225`). Each probe's row is
/// a scalar `count`/`sum` the caller sums into [`refresh_stats_update_sql`].
#[must_use]
pub fn refresh_stats_probes_sql(id: i64) -> [Sql; 8] {
    let lib = Value::Integer(id);
    [
        (
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM library_artist la \
             INNER JOIN artist a ON la.artist_id = a.id \
             WHERE la.library_id = ? AND a.missing = ?"
                .to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM folder WHERE library_id = ? AND missing = ? AND num_audio_files > ?".to_string(),
            vec![lib.clone(), Value::Bool(false), Value::Integer(0)],
        ),
        (
            "SELECT ifnull(sum(num_audio_files + num_playlists + json_array_length(image_files)),0) as count \
             FROM folder WHERE library_id = ? AND missing = ?"
                .to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(true)],
        ),
        (
            "SELECT ifnull(sum(size),0) as sum FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib.clone(), Value::Bool(false)],
        ),
        (
            "SELECT ifnull(sum(duration),0) as sum FROM album WHERE library_id = ? AND missing = ?".to_string(),
            vec![lib, Value::Bool(false)],
        ),
    ]
}

/// `RefreshStats`'s totals update (`:229-238`): the eight probe results in
/// probe order, then `updated_at` — Go's `.Set` call order.
#[must_use]
/// The eight probe results plus the id, in [`refresh_stats_probes_sql`] order.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStatsTotals {
    /// `library.id`.
    pub id: i64,
    /// `total_songs` (probe 0).
    pub songs: i64,
    /// `total_albums` (probe 1).
    pub albums: i64,
    /// `total_artists` (probe 2).
    pub artists: i64,
    /// `total_folders` (probe 3).
    pub folders: i64,
    /// `total_files` (probe 4).
    pub files: i64,
    /// `total_missing_files` (probe 5).
    pub missing: i64,
    /// `total_size` (probe 6).
    pub size: i64,
    /// `total_duration` (probe 7).
    pub duration: f64,
    /// `updated_at` (Go's `time.Now()`).
    pub updated_at: i64,
}

/// See [`RefreshStatsTotals`].
pub fn refresh_stats_update_sql(totals: &RefreshStatsTotals) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET total_songs = ?, total_albums = ?, total_artists = ?, \
             total_folders = ?, total_files = ?, total_missing_files = ?, total_size = ?, \
             total_duration = ?, updated_at = ? WHERE id = ?"
        ),
        vec![
            Value::Integer(totals.songs),
            Value::Integer(totals.albums),
            Value::Integer(totals.artists),
            Value::Integer(totals.folders),
            Value::Integer(totals.files),
            Value::Integer(totals.missing),
            Value::Integer(totals.size),
            Value::Real(totals.duration),
            Value::Integer(totals.updated_at),
            Value::Integer(totals.id),
        ],
    )
}

/// `Delete` (`:242-270`): the row delete — the admin/ID-1 validation
/// (`:244-249`) and the artist-orphan/plugin cleanups (`:252-266`) are the
/// caller's (auth, artist/plugin lanes).
#[must_use]
pub fn delete_sql(id: i64) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE id = ?"),
        vec![Value::Integer(id)],
    )
}

/// `GetUsersWithLibraryAccess` (`:198-208`): the user rows joined through
/// `user_library`, ordered by name. Squirrel's plain `.Join` renders bare
/// `JOIN` (`squirrel@v1.5.4/select.go:298`).
#[must_use]
pub fn get_users_with_library_access_sql(library_id: i64) -> Sql {
    (
        "SELECT u.* FROM user u \
         JOIN user_library ul ON u.id = ul.user_id \
         WHERE ul.library_id = ? \
         ORDER BY u.name"
            .to_string(),
        vec![Value::Integer(library_id)],
    )
}

/// Failures of the executed library reads ([`LibraryRepo`]).
///
/// The variant set follows the sibling `RadioRepo::RadioError`
/// (`radio_repo.rs:425-460`) — a statement/scan failure, the not-found case, and the
/// builder arm this repository's statement builders return a `Result` for.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The SQLite statement failed, or a row could not be scanned. There is no
    /// `PostScan` hook to fail here — `grep -n "dbLibrary\|PostScan"
    /// persistence/library_repository.go` is empty — so this variant is a pure
    /// statement/scan failure. The reachable scan failures are Go's non-pointer
    /// fields rejecting NULL, both executed against the real repository (oracle
    /// `/tmp/sd3g1_oracle2`): `total_duration = NULL` gives
    /// `sql: Scan error on column index 16, name "total_duration": converting NULL to
    /// float64 is unsupported` and `remote_path = NULL` gives
    /// `sql: Scan error on column index 3, name "remote_path": converting NULL to
    /// string is unsupported`. Go returns its partially-filled `res` beside the error
    /// (`library_repository.go:286` returns `res` with `err`), so rows scanned before
    /// the failing one survive there; `Result` has no value-beside-error, so the port
    /// reports the error and drops them — the same decision already documented for
    /// [`crate::radio_repo::RadioRepo::get_all`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The statement could not be materialized — Go's `toSQL` failure
    /// (`sql_base_repository.go:358-372`). For this table the builder's only failure
    /// mode is an empty order-by list, which the fixed table makes unreachable; the
    /// arm exists so the port does not invent a `rusqlite::Error` for it.
    #[error(transparent)]
    Builder(#[from] crate::sqlrepo::SqlError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`), which
    /// `queryOne` maps from `sql.ErrNoRows` (`sql_base_repository.go:375-388`,
    /// `dbx@v1.12.0/rows.go:237-244`).
    ///
    /// Executed Go (`/tmp/sd3g1_oracle`): `Get(99)` returned
    /// `err="data not found"` beside a NON-nil pointer to a zero `Library`; the port's
    /// `Err(NotFound)` carries the same information minus that dead pointer, which no
    /// Go caller can legitimately read.
    ///
    /// [`LibraryRepo::get_all`] and [`LibraryRepo::get_all_plain`] NEVER report this
    /// variant: `queryAll`'s dbx `All` makes an empty non-nil slice for a zero-row
    /// result (`dbx@v1.12.0/query.go:278-291`), so its ErrNoRows arm is dead code on
    /// that path. [`LibraryRepo::get_path`] DOES report it — but only for an id the
    /// full library list does not contain (`library_repository.go:65-66`), which is a
    /// cache lookup miss rather than a `queryOne`.
    #[error("data not found")]
    NotFound,
}

/// `libraryRepository` (`library_repository.go:21-23`) bound to one pooled
/// connection — the executed READ side of the library repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate has
/// no ambient context (the `sqlrepo.rs` DI freeze), so the caller threads a lease from
/// [`crate::store::SQLStore::with_connection_sync`] (a transaction derefs to a
/// `&Connection`) and every statement runs through [`crate::execution`] — the port of
/// `executeSQL`/`queryOne`/`queryAll`/`count`
/// (`sql_base_repository.go:336-355,375-388,441-457,565-576`). Rows hydrate through
/// [`crate::hydrate::row_to_library`] — a SINGLE phase, because Go has no `dbLibrary`
/// wrapper and no `PostScan` hook.
///
/// No read takes a user: `libraryRepository` defines no `userFilter` and no
/// `applyLibraryFilter` call (`grep -n "applyLibraryFilter\|userFilter"
/// persistence/library_repository.go` is empty), so `newSelect` adds no ownership
/// clause (`sql_base_repository.go:139-147`). Neither [`get_sql`], [`get_all_sql`] nor
/// [`get_all_query_sql`] carries one, and none of the executed tests needs a logged
/// user.
#[derive(Debug)]
pub struct LibraryRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> LibraryRepo<'a> {
    /// `NewLibraryRepository` (`library_repository.go:26-32`): bind to `conn` (the
    /// pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`:35-40`): `SELECT * … WHERE id = ?`, hydrated through
    /// [`crate::hydrate::row_to_library`].
    ///
    /// Go's `id` is an `int` and `library.id` is INTEGER, so the bind is
    /// `Value::Integer` — the executed trace is
    /// `SELECT * FROM library WHERE id = ?` with an integer arg.
    ///
    /// NotFound is `queryOne`'s no-rows condition (see [`LibraryError::NotFound`]),
    /// decided before any field is read: executed `Get(99)` issued exactly one statement
    /// and returned `data not found`.
    ///
    /// A NULL into one of Go's non-pointer columns is a SCAN error, not NotFound
    /// (see [`LibraryError::Sqlite`]).
    ///
    /// # Errors
    /// [`LibraryError::NotFound`] when the id matches nothing;
    /// [`LibraryError::Sqlite`] on a statement or scan failure.
    pub fn get(&self, id: i64) -> Result<LibraryRecord, LibraryError> {
        let statement = get_sql(id);
        // Go's queryOne (`:38`) → dbx `Rows.one` (`dbx@v1.12.0/rows.go:237-244`);
        // `query_optional`'s `None` is exactly that no-rows condition, which Go maps to
        // model.ErrNotFound.
        crate::execution::query_optional(self.conn, &statement, crate::hydrate::row_to_library)?
            .ok_or(LibraryError::NotFound)
    }

    /// `GetAll` (`:283-287`) with no options: Go's `GetAll(ctx)`, i.e.
    /// `newSelect(ctx)` with an empty option list, which applies no filter, no page and
    /// no ORDER BY at all. Executed Go trace: `SELECT * FROM library`, args empty.
    ///
    /// Row order is therefore SQLite's, not the caller's — exactly as in Go, since
    /// `applyOptions` only runs when `len(options) > 0`
    /// (`sql_base_repository.go:142-145`).
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails or a row will not scan (see
    /// [`LibraryError::Sqlite`] for Go's partial-slice behavior). Never
    /// [`LibraryError::NotFound`]: an empty table is an empty `Vec`.
    pub fn get_all_plain(&self) -> Result<Libraries, LibraryError> {
        let statement = get_all_sql(&[], 0, 0);
        Ok(crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_library,
        )?)
    }

    /// `GetAll` (`:283-287`) under the caller's parsed options: the filters, the
    /// `buildSortOrder` leg and Max/Offset all reach the statement, as they do in Go's
    /// `newSelect` (`sql_base_repository.go:141-145`).
    ///
    /// Executed row sets are pinned by the runtime tests; the ORDER BY leg is NOT
    /// silently dropped here (unlike the artist and radio lanes' hand-rendered tails,
    /// documented in `hydrate-contract.md` §S-D3d/§S-D3f), because this path goes
    /// through [`Repository`] with an explicit `order_by` leg.
    ///
    /// The offset-without-limit page EXECUTES (`LIMIT -1 OFFSET n`) where Go's bare
    /// `OFFSET` is a SQLite syntax error — the builder's own cited deviation
    /// (`sqlrepo.rs:313-319`), pinned rather than "fixed".
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails or a row will not scan;
    /// [`LibraryError::Builder`] if the builder fails to materialize. Never
    /// [`LibraryError::NotFound`].
    pub fn get_all(
        &self,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<Libraries, LibraryError> {
        let statement = get_all_query_sql(options)?;
        Ok(crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_library,
        )?)
    }

    /// `CountAll` (`:290-292`) with no options: the DISTINCT aggregate with the reset
    /// `ORDER BY library.id` leg. Executed Go: `SELECT count(distinct library.id) as
    /// count FROM library ORDER BY library.id`.
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails. Go's `count` returns
    /// `res.Count` beside a `queryOne` error (`sql_base_repository.go:572-574`), so a
    /// failing count surfaces both; the port reports only the error and the count
    /// defaults to 0 in that case.
    pub fn count_all(&self) -> Result<i64, LibraryError> {
        let statement = count_all_sql();
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `CountAll` (`:290-292`) under the caller's parsed options: filters survive,
    /// `Max`/`Offset` are dropped by `count`, and a caller sort leg is APPENDED before
    /// the reset `library.id` leg (see [`count_all_query_sql`] for the executed traces).
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails; [`LibraryError::Builder`] if
    /// the builder fails to materialize.
    pub fn count_all_query(
        &self,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<i64, LibraryError> {
        let statement = count_all_query_sql(options)?;
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `ScanInProgress` (`:190-194`): `count > 0` over
    /// `last_scan_started_at <> ZERO_TIME`.
    ///
    /// Go's `count` returns `(count, err)` and Go's caller inspects the bool even when
    /// the error is set (`:193-194` returns both); the port folds that to
    /// `Ok(count > 0)` on success and `Err` otherwise, which is the only `Result`
    /// shape available.
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails.
    pub fn scan_in_progress(&self) -> Result<bool, LibraryError> {
        let statement = scan_in_progress_count_sql();
        let count =
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default();
        Ok(count > 0)
    }

    /// `GetUsersWithLibraryAccess` (`:297-307`): the `SELECT u.*` join over
    /// `user_library`, ordered by `u.name`, read with
    /// [`crate::hydrate::row_to_user_without_libraries`].
    ///
    /// Two Go behaviors this read reproduces, both executed (oracle
    /// `/tmp/sd3g1_oracle`, `/tmp/sd3g1_oracle2`):
    /// * the rows come back in `ORDER BY u.name` order — Go's statement has that leg
    ///   unconditionally (`:302`), and executed `GetUsersWithLibraryAccess(3)` returned
    ///   `[Alice, Bob]` for names Alice/Bob;
    /// * `Libraries` stays EMPTY on every row, because `u.*` emits no `libraries_json`
    ///   alias, so `dbUser.PostScan` takes its skip arm (see that reader's doc). A
    ///   caller that needs the association list must read the user through
    ///   [`crate::user_repo::UserRepo::get`].
    ///
    /// An id with no associations is an empty `Vec` with no error — executed:
    /// `GetUsersWithLibraryAccess(99)` gave `len=0, err=<nil>`. `queryAll` never maps
    /// `ErrNoRows` to NotFound (`sql_base_repository.go:450-453`), so this read never
    /// reports [`LibraryError::NotFound`].
    ///
    /// # Errors
    /// [`LibraryError::Sqlite`] when the statement fails or a user row will not scan.
    pub fn get_users_with_library_access(
        &self,
        library_id: i64,
    ) -> Result<Vec<UserRecord>, LibraryError> {
        let statement = get_users_with_library_access_sql(library_id);
        Ok(crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_user_without_libraries,
        )?)
    }

    /// `GetPath` (`:42-67`): the library's `path` for an id, or NotFound.
    ///
    /// Go's implementation is a PROCESS cache (`libCache`/`libLock`, `:25-29`) over
    /// `GetAll`: on a miss it reloads EVERY library into the map and then reads the id
    /// out of it (`:53-66`). That map is runtime state with no SQL counterpart, so the
    /// port keeps no cache and re-runs the same lookup per call. The consequences are
    /// recorded, not emulated — and they run in BOTH directions:
    /// * Go is STALE where the port is fresh. Executed (oracle `/tmp/sd3g1_oracle2`):
    ///   after `update library set path='/changed-A' where id=1`, Go's
    ///   `GetPath(1)` still returned the pre-update `"/music-A"`; after
    ///   `delete from library where id=2`, Go's `GetPath(2)` still returned `"/m2"`
    ///   with `err=<nil>`. The port re-reads and returns the current path / NotFound.
    /// * Go is FRESH where the port costs a query: its cached hit issues no SQL at all
    ///   (`:45-50`), while the port runs `SELECT * FROM library` on every call.
    ///   `Put` also writes the cache (`:126`), which is another reason Go's value
    ///   can precede the row's.
    ///
    /// `path` may legitimately be empty — the fresh schema's library 1 has
    /// `path = ''` — and Go returns `("", nil)` for it (executed: `GetPath(1)` gave
    /// `""`, `err=<nil>`). The lookup is therefore by PRESENCE in the list, not by
    /// non-emptiness; Go's own `if l != ""` cache probe (`:51`) only short-circuits a
    /// hit, and the post-reload arm at `:63-66` returns an empty path too.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`] when no library carries the id;
    /// [`LibraryError::Sqlite`] when the underlying `GetAll` fails — Go logs that
    /// (`:55`, "Error loading libraries from DB") and returns the error, which this
    /// crate has no logger for.
    pub fn get_path(&self, id: i64) -> Result<String, LibraryError> {
        // Go's reload (`:53-62`): GetAll with no options, then index by id.
        let libraries = self.get_all_plain()?;
        libraries
            .iter()
            .find(|lib| lib.id == id)
            .map(|lib| lib.path.clone())
            .ok_or(LibraryError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Arrange/Act/Assert helpers: every test pins the (sql, args) pair Go's
    // squirrel/dbx layer materializes, against the literal shapes of
    // library_repository.go.

    #[test]
    fn get_and_count_pin_the_base_selects() {
        // Arrange: Get (:35-40) and CountAll (:193-196) over library.
        // Act + Assert:
        assert_eq!(
            get_sql(3),
            (
                "SELECT * FROM library WHERE id = ?".to_string(),
                vec![Value::Integer(3)]
            )
        );
        assert_eq!(
            count_all_sql(),
            (
                // `count` installs its DISTINCT column and APPENDS
                // `ORDER BY library.id` (`sql_base_repository.go:565-570`:
                // RemoveColumns().Columns(…).RemoveOffset().RemoveLimit()
                // .OrderBy(table+".id").From(table) — the comment there says "To
                // remove any ORDER BY clause", but squirrel's `OrderBy` is
                // `builder.Append` (select.go:362-373) and `RemoveOrderBy` is never
                // called. Executed Go trace (oracle /tmp/sd3g1_oracle4) confirms the
                // leg is emitted on the bare call, matching
                // `radio_repo::count_all_sql` (S-D3f). An earlier expectation here
                // had no ORDER BY; the executed trace contradicted it.
                "SELECT count(distinct library.id) as count FROM library ORDER BY library.id"
                    .to_string(),
                Vec::new()
            )
        );
    }

    #[test]
    fn get_all_renders_pagination_and_caller_order() {
        // Arrange: GetAll (:186-191) under parsed options.
        // Act:
        let (sql, args) = get_all_sql(&["name asc"], 10, 20);
        // Assert: the bounds render as squirrel literals (`select.go:139-147`),
        // so this select binds nothing at all.
        assert_eq!(
            sql,
            "SELECT * FROM library ORDER BY name asc LIMIT 10 OFFSET 20"
        );
        assert!(args.is_empty());
        assert_eq!(
            get_all_sql(&[], 0, 0).0,
            "SELECT * FROM library",
            "non-positive bounds leave the clauses unset (applyOptions)"
        );
    }

    #[test]
    fn put_update_sets_all_columns_in_sorted_key_order() {
        // Arrange: Put's SetMap (:85-87) with no colsToUpdate — squirrel
        // sorts the SET keys (squirrel@v1.5.4/update.go:237).
        let update = LibraryUpdate {
            name: "Renamed",
            path: "/music/updated",
            remote_path: "/remote/original",
            default_new_users: true,
            pid_album: "alb",
            pid_track: "trk",
            updated_at: 1_700_000_000,
        };
        // Act:
        let (sql, args) = put_update_sql(2, &update, &[]);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET default_new_users = ?, name = ?, path = ?, \
             pid_album = ?, pid_track = ?, remote_path = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(true),
                Value::Text("Renamed".to_string()),
                Value::Text("/music/updated".to_string()),
                Value::Text("alb".to_string()),
                Value::Text("trk".to_string()),
                Value::Text("/remote/original".to_string()),
                Value::Integer(1_700_000_000),
                Value::Integer(2),
            ]
        );
    }

    #[test]
    fn put_update_honors_cols_to_update_and_always_stamps_updated_at() {
        // Arrange: Put(ctx, lib, "name", "path") (:65-69) — only the
        // requested columns plus the mandatory updated_at (:83).
        let update = LibraryUpdate {
            name: "Renamed",
            path: "/music/original",
            remote_path: "/remote/original",
            default_new_users: true,
            pid_album: "alb",
            pid_track: "trk",
            updated_at: 99,
        };
        // Act:
        let (sql, args) = put_update_sql(2, &update, &["name", "path"]);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET name = ?, path = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("Renamed".to_string()),
                Value::Text("/music/original".to_string()),
                Value::Integer(99),
                Value::Integer(2),
            ]
        );
    }

    #[test]
    fn insert_binds_id_first_when_given_and_renders_all_row_columns() {
        // Arrange: dbx Model(l).Insert() (:78, :96-101) — the full row in
        // model.Library declaration order; id first for the ID-999 case.
        // Act:
        let (sql, args) = insert_sql(Some(999), vec![Value::from("x"); 21]);
        // Assert:
        assert!(sql.starts_with(
            "INSERT INTO library (id, name, path, remote_path, last_scan_at, \
             last_scan_started_at, full_scan_in_progress, updated_at, created_at, "
        ));
        assert!(sql.ends_with(&format!(
            "scanned_pid_track) VALUES ({})",
            vec!["?"; 22].join(", ")
        )));
        assert_eq!(args.len(), 22);
        assert_eq!(args[0], Value::Integer(999));
        assert!(insert_sql(None, vec![Value::from("x"); 21])
            .0
            .contains("INSERT INTO library (name, path,"));
    }

    #[test]
    fn assign_admins_is_the_argless_cross_join_upsert() {
        // Arrange: Put's admin assignment (:105-111) — the exact literal.
        // Act:
        let (sql, args) = assign_admins_sql();
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO user_library (user_id, library_id)\nSELECT u.id, l.id\n\
             FROM user u\nCROSS JOIN library l\nWHERE u.is_admin = true\n\
             ON CONFLICT (user_id, library_id) DO NOTHING;"
        );
        assert!(args.is_empty(), "identifiers only — nothing to bind");
    }

    #[test]
    fn store_music_folder_guards_on_unchanged_path() {
        // Arrange: StoreMusicFolder (:117-133) — Set order is call order.
        // Act:
        let (sql, args) = store_music_folder_sql("/music", 5);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET path = ?, updated_at = ? WHERE id = ? AND path <> ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("/music".to_string()),
                Value::Integer(5),
                Value::Integer(DEFAULT_LIBRARY_ID),
                Value::Text("/music".to_string()),
            ]
        );
    }

    #[test]
    fn scan_lifecycle_and_pid_updates_pin_set_order() {
        // Arrange: ScanBegin/ScanEnd/SetScannedPID (:147-178) — .Set call order.
        // Act + Assert:
        assert_eq!(
            scan_begin_sql(1, 7, true),
            (
                "UPDATE library SET last_scan_started_at = ?, full_scan_in_progress = ? WHERE id = ?".to_string(),
                vec![Value::Integer(7), Value::Bool(true), Value::Integer(1)]
            )
        );
        assert_eq!(
            scan_end_sql(1, 8),
            (
                "UPDATE library SET last_scan_at = ?, full_scan_in_progress = ?, \
                 last_scan_started_at = ? WHERE id = ?"
                    .to_string(),
                vec![
                    Value::Integer(8),
                    Value::Bool(false),
                    Value::Integer(ZERO_TIME),
                    Value::Integer(1)
                ]
            )
        );
        assert_eq!(
            set_scanned_pid_sql(2, "a", "t"),
            (
                "UPDATE library SET scanned_pid_album = ?, scanned_pid_track = ? WHERE id = ?"
                    .to_string(),
                vec![
                    Value::Text("a".to_string()),
                    Value::Text("t".to_string()),
                    Value::Integer(2)
                ]
            )
        );
    }

    #[test]
    fn scan_in_progress_compares_against_the_same_zero_scan_end_writes() {
        // Arrange: ScanInProgress (:180-184) — the NotEq against time.Time{}.
        // The constant must be the one scan_end_sql binds, or a finished
        // scan reads as in-progress.
        let (_, end_args) = scan_end_sql(1, 8);
        // Act:
        let (sql, args) = scan_in_progress_sql();
        // Assert:
        assert_eq!(sql, "SELECT * FROM library WHERE last_scan_started_at <> ?");
        assert_eq!(args[0], end_args[2]);
    }

    #[test]
    fn refresh_stats_probes_pin_the_eight_aggregates() {
        // Arrange: RefreshStats' run.Parallel probes (:199-236) — each
        // two-key Eq map renders alphabetically (library_id, missing).
        // Act:
        let probes = refresh_stats_probes_sql(1);
        // Assert:
        assert_eq!(
            probes[0].0,
            "SELECT count(*) as count FROM media_file WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(probes[0].1, vec![Value::Integer(1), Value::Bool(false)]);
        assert_eq!(
            probes[2].0,
            "SELECT count(*) as count FROM library_artist la \
             INNER JOIN artist a ON la.artist_id = a.id \
             WHERE la.library_id = ? AND a.missing = ?"
        );
        assert_eq!(
            probes[3].0,
            "SELECT count(*) as count FROM folder WHERE library_id = ? AND missing = ? AND num_audio_files > ?"
        );
        assert_eq!(
            probes[4].0,
            "SELECT ifnull(sum(num_audio_files + num_playlists + json_array_length(image_files)),0) as count FROM folder WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(
            probes[5].1,
            vec![Value::Integer(1), Value::Bool(true)],
            "the missing-files probe flips missing to true (:228-230)"
        );
        assert_eq!(
            probes[6].0,
            "SELECT ifnull(sum(size),0) as sum FROM album WHERE library_id = ? AND missing = ?"
        );
        assert_eq!(
            probes[7].0,
            "SELECT ifnull(sum(duration),0) as sum FROM album WHERE library_id = ? AND missing = ?"
        );
    }

    #[test]
    fn refresh_stats_update_sets_totals_in_probe_order() {
        // Arrange: the totals UPDATE (:229-238) — .Set call order, values
        // ordered to the probes.
        // Act:
        let totals = RefreshStatsTotals {
            id: 1,
            songs: 10,
            albums: 2,
            artists: 3,
            folders: 4,
            files: 5,
            missing: 6,
            size: 7,
            duration: 8.5,
            updated_at: 9,
        };
        // Act:
        let (sql, args) = refresh_stats_update_sql(&totals);
        // Assert:
        assert_eq!(
            sql,
            "UPDATE library SET total_songs = ?, total_albums = ?, total_artists = ?, \
             total_folders = ?, total_files = ?, total_missing_files = ?, total_size = ?, \
             total_duration = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(10),
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(4),
                Value::Integer(5),
                Value::Integer(6),
                Value::Integer(7),
                Value::Real(8.5),
                Value::Integer(9),
                Value::Integer(1),
            ]
        );
    }

    #[test]
    fn add_artist_delete_and_users_join_pin_their_shapes() {
        // Arrange: AddArtist (:135-145), Delete (:242-270) and
        // GetUsersWithLibraryAccess (:198-208).
        // Act + Assert:
        assert_eq!(
            add_artist_sql(1, "ar-1"),
            (
                "INSERT INTO library_artist (library_id, artist_id) VALUES (?, ?) \
                 on conflict(library_id, artist_id) do nothing"
                    .to_string(),
                vec![Value::Integer(1), Value::Text("ar-1".to_string())]
            )
        );
        assert_eq!(
            delete_sql(1),
            (
                "DELETE FROM library WHERE id = ?".to_string(),
                vec![Value::Integer(1)]
            )
        );
        assert_eq!(
            get_users_with_library_access_sql(2),
            (
                "SELECT u.* FROM user u JOIN user_library ul ON u.id = ul.user_id \
                 WHERE ul.library_id = ? ORDER BY u.name"
                    .to_string(),
                vec![Value::Integer(2)]
            )
        );
    }
}
