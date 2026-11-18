//! Port of `persistence/transcoding_repository.go` (110L) — the transcoding
//! configuration repository.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `queryOne`/`queryAll`/`put`/`deleteByID`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-373`) as `(sql, args)`; execution and
//! logging stay with `store.rs`/the caller, so the port stops at the same
//! `(sql, args)` pairs (see [`Sql`]). The `loggedUser(ctx).IsAdmin` guards
//! (`rest.ErrPermissionDenied`) become [`is_admin`] parameters on the mutating
//! methods, mirroring how sibling slices pass the auth context in.
//!
//! Ported here: `Get`/`FindByFormat`'s Eq selects (`transcoding_repository.go:24`,
//! `:35`), `CountAll`/`Count` via the base `count()` shape
//! (`sql_base_repository.go:565-575`), `Read`/`ReadAll`'s admin-only `command`
//! masking (`transcoding_repository.go:58-59`, `:71-74`), `Put`/`Save`/`Update`
//! via the base `put()` upsert (`sql_base_repository.go:618-642`) and `Delete`
//! via `deleteByID` (`sql_base_repository.go:650-659`).
//!
//! Not ported, with the reason: nothing of the READ side remains — the rest
//! option layer ([`rest_repo`]/[`sanitize_sort`]/[`build_sort_order`]/
//! [`parse_rest_options`]) and [`TranscodingRepo`] EXECUTE `Get`/`FindByFormat`/
//! `Read`/`ReadAll`/`CountAll`/`Count` through [`crate::execution`], in the shape
//! `radio_repo::RadioRepo` (`radio_repo.rs:508-620`) and `library_repo::LibraryRepo`
//! (`library_repo.rs:911-1122`) establish. `ReadAll`'s option parsing runs INSIDE
//! [`TranscodingRepo::read_all`] — Go calls `parseRestOptions` inside `ReadAll`
//! (`transcoding_repository.go:65`), so `sanitizeSort` is unconditional there and
//! a caller cannot hand an unsanitized sort to the executed statement (see the
//! warning on [`read_all_query_sql`]). The WRITES still stop at the [`Sql`] pairs
//! (see [`Sql`]), and `logSQL` (`sql_base_repository.go:661-678`) stays unported
//! crate-wide (`Repository::log_sql`), so these reads inherit that.
//!
//! `command` stays a WHITELISTED sort field: Go's whitelist is built from
//! `model.Transcoding`'s struct fields plus `model.Annotations`
//! (`sql_restful.go:148-163`), so a bare `sort=command` orders a non-admin's rows
//! in Go too (`sanitizeSort` validates against the whitelist, `:70-77`). This is
//! inherited Go behavior, pinned by the executed tests rather than diverged from;
//! only expressions the whitelist rejects are dropped.
//!
//! Masking is a READ-boundary concern only: `Get` (`:23-28`) and `FindByFormat`
//! (`:34-39`) return the real `command` because `core/players.go:87-91` and
//! `core/stream/decider.go:315,330` feed it to ffmpeg; `Read` (`:53-62`) and
//! `ReadAll` (`:64-77`) blank it for a non-admin but still return the row
//! ([`mask_command`]/[`mask_commands`]).

use crate::sql_restful::QueryOptions;
use crate::sqlrepo::{Repository, Value};

/// The `transcoding` table (`registerModel(&model.Transcoding{})`, `:19`).
pub const TABLE: &str = "transcoding";

/// `model.Transcoding` (`model/transcoding.go`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcoding {
    /// Go `ID`.
    pub id: String,
    /// Go `Name`.
    pub name: String,
    /// Go `TargetFormat`.
    pub target_format: String,
    /// Admin-only in REST reads (`:59`/`:73`) — masked by
    /// [`mask_command`] for non-admins.
    pub command: String,
    /// Go `DefaultBitRate`.
    pub default_bit_rate: i64,
}

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-373`) output shape.
pub type Sql = (String, Vec<Value>);

/// `model.Transcodings`.
pub type Transcodings = Vec<Transcoding>;

/// `rest.ErrPermissionDenied` (`transcoding_repository.go:43`, `:81`, `:88`, `:98`):
/// the mutating methods' non-admin failure, spelled as data rather than an error
/// enum the crate does not own. No READ leg returns it: `Read`/`ReadAll` mask rather
/// than refuse (`:58-59`, `:71-74`).
pub const ERR_PERMISSION_DENIED: &str = "permission denied";

/// The non-admin `command` mask: Go blanks `res.Command` (`:59`) / every
/// `res[i].Command` (`:73`) rather than dropping the row.
#[must_use]
pub fn mask_command(mut t: Transcoding, is_admin: bool) -> Transcoding {
    if !is_admin {
        t.command = String::new();
    }
    t
}

/// `ReadAll`'s loop shape (`:71-74`): mask every row for non-admins.
#[must_use]
pub fn mask_commands(list: Transcodings, is_admin: bool) -> Transcodings {
    list.into_iter()
        .map(|t| mask_command(t, is_admin))
        .collect()
}

/// `Get` (`:23-28`): `SELECT * FROM transcoding WHERE id = ?` (statement `:24`).
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    let (sql, args) = Repository::new(TABLE)
        .where_eq("id", id)
        .to_sql()
        .expect("the get select materializes");
    (sql, args)
}

/// `FindByFormat` (`:34-39`): `SELECT * FROM transcoding WHERE target_format = ?`
/// (statement `:35`).
#[must_use]
pub fn find_by_format_sql(format: &str) -> Sql {
    let (sql, args) = Repository::new(TABLE)
        .where_eq("target_format", format)
        .to_sql()
        .expect("the find-by-format select materializes");
    (sql, args)
}

/// `CountAll`/`Count` (`:30-31`, `:49-51`) via the base `count()`
/// (`sql_base_repository.go:565-575`): `count(distinct transcoding.id) as count`,
/// pagination and caller order dropped, `options.Filters` kept.
///
/// `count` APPENDS `OrderBy(r.tableName + ".id")` to the statement it is handed
/// (`:569`, "To remove any ORDER BY clause that could slow down the query"), and
/// Go's `CountAll`/`Count` hand it a fresh `Select()` (`:30-31`, `:49-51`), so that
/// reset leg is the ONLY ORDER BY in the statement: Go's traced text is
/// `SELECT count(distinct transcoding.id) as count FROM transcoding ORDER BY
/// transcoding.id`. [`Repository::count`] drops the sort legs
/// (`sqlrepo.rs:294-301`), so this restores the reset one. The leg contributes no
/// argument and cannot change a count, but `radio_repo::count_all_sql`
/// (`radio_repo.rs:248`) and `library_repo::count_all_sql` (`library_repo.rs:244`)
/// both render it, and this file must not disagree with them.
///
/// Unlike those two repositories NO caller-sort leg precedes it: transcoding's
/// `count` receives a bare `Select()` rather than `newSelect`'s output
/// (`library_repository.go:291-292` passes the built select, whose caller ORDER BY
/// squirrel's `Append` keeps), so `options.sort` never reaches this statement —
/// which is why [`TranscodingRepo::count`] is invariant under a caller sort.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize (it always
/// can here — the table is fixed).
pub fn count_sql(options: Option<&QueryOptions>) -> Sql {
    let mut sq = Repository::new(TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    let (sql, args) = sq
        .count(TABLE)
        .order_by([format!("{TABLE}.id")])
        .to_sql()
        .expect("the count query materializes");
    (sql, args)
}
/// `ReadAll` (`:64-77`, select built at `:65`): `newSelect(ctx,
/// parseRestOptions(…)).Columns("*")` —
/// filters, the `buildSortOrder` leg and Max/Offset
/// (`sql_base_repository.go:139-147`, `:149-162`, `:247-252`). `Read` (`:53-62`)
/// is [`get_sql`] plus [`mask_command`].
///
/// WARNING: `options` here must ALREADY be the output of [`parse_rest_options`]
/// (`sql_restful.go:54-67`) — this renderer is a pure statement builder and does
/// NOT sanitize `options.sort` itself, so a hand-built [`QueryOptions`] carrying an
/// arbitrary sort string reaches `ORDER BY` verbatim (injection through the sort
/// field). Go has no such gap because `ReadAll` calls `parseRestOptions` itself
/// (`transcoding_repository.go:65`) and `sanitizeSort`
/// (`sql_restful.go:69-86`) therefore always runs first; [`TranscodingRepo::read_all`]
/// mirrors that by parsing its own rest options, which is why the executed path is
/// safe. The residual risk is confined to callers of this public renderer: the
/// whitelist check lives in [`sanitize_sort`], not here.
///
/// The sort leg is applied HERE, not by [`crate::sql_search::new_select`], which
/// carries pagination and filters but no ORDER BY (`sql_search.rs:129-135` —
/// `applyOptions`' sort half needs the base repository's per-repository
/// `sortMappings`/`buildSortOrder`): a `sort` option used to render
/// `SELECT * FROM transcoding LIMIT 20` and silently change nothing. Go's
/// `applyOptions` appends `OrderBy(buildSortOrder(sort, order))` whenever
/// `Sort != ""` (`sql_base_repository.go:157-159`), so this follows
/// `library_repo::get_all_query_sql` (`library_repo.rs:422-434`) rather than the
/// artist/radio lanes, whose hand-rendered tails remain documented residuals.
///
/// Clause order is squirrel's by-kind order (`sqlrepo.rs:343-358`), which is
/// Go's executed order: the filters render in WHERE ahead of the ORDER BY leg
/// even though Go called `applyOptions` first.
///
/// `options == None` is Go's `newSelect(ctx)` with an empty option list
/// (`sql_base_repository.go:139-147`): no filter, no page, no ORDER BY at all.
///
/// Deviation kept, not fixed: an `Offset` with no `Max` renders `LIMIT -1 OFFSET n`
/// ([`Repository::to_sql`], `sqlrepo.rs:313-319`) where Go's bare `OFFSET n` is a
/// SQLite parse error, so the port EXECUTES that page and skips rows — the same
/// cited divergence as `radio_repo::RadioRepo::get_all`.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize.
pub fn read_all_query_sql(options: Option<&QueryOptions>) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = Repository::new(TABLE).select(["*"]);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
        sq = crate::sql_search::apply_pagination(sq, options);
    }
    sq.to_sql()
}

/// [`read_all_query_sql`] with the materialization failure flattened, matching
/// this file's other renderers, which `expect` a builder that cannot fail here.
///
/// Panics if the fixed-table builder cannot materialize. [`crate::sqlrepo::SqlError`]
/// has two arms: `NoTable` needs an unset `from` table, which this renderer never
/// leaves unset, and [`crate::sqlrepo::SqlError::EmptyOrder`] is declared
/// (`sqlrepo.rs:36`) and documented as an error case (`sqlrepo.rs:323`) but is
/// constructed NOWHERE in the crate — so neither arm is producible here.
///
/// The consequence is stated rather than smoothed over: a whitespace-only sort
/// makes [`build_sort_order`] return an empty string, `order_by` appends that empty
/// expression, and the result is a trailing `ORDER BY ` with nothing after it — a
/// SQLite syntax error at EXECUTION time, not a builder error. Go's squirrel renders
/// the same dangling clause (`squirrel@v1.5.4 select.go:132-138`), so rendering is
/// left as-is. Through [`TranscodingRepo::read_all`] the case cannot arise,
/// because [`sanitize_sort`] drops a sort the whitelist rejects before it gets here.
#[must_use]
pub fn read_all_sql(options: Option<&QueryOptions>) -> Sql {
    let (sql, args) = read_all_query_sql(options).expect("the read-all select materializes");
    (sql, args)
}

/// `registerModel(&model.Transcoding{}, nil)`'s rest-facing half
/// (`transcoding_repository.go:19` → `sql_base_repository.go:104-112` plus
/// `registerModelWhiteList`/`registerFieldWhiteList`, `sql_restful.go:142-146` and
/// `:148-163`): the `transcoding` table with NO filter mappings — Go's second
/// `registerModel` argument is `nil`, so every filter falls through to the whitelist
/// arm — and the whitelist of `model.Transcoding`'s five struct fields plus the
/// annotation fields (`sql_restful.go:159-162`).
///
/// The whitelist is what makes Go drop a filter it does not recognize
/// (`Ignoring filter not whitelisted`, `sql_restful.go:38-41`); a whitelisted field
/// ending in `id` takes the exact-match arm (`:43-47`) and everything else "starts
/// with" (`:48-49`). NOTE that the annotation fields are members of the whitelist even
/// though `transcoding` has no such columns (`nd-dbschema/src/schema_ddl.rs:56`):
/// a REST filter or sort on `starred` reaches SQLite against a column that does not
/// exist and errors THERE, exactly as Go does. Reported, not silently dropped —
/// [`sanitize_sort`] drops a NON-whitelisted sort but cannot drop a whitelisted-
/// yet-absent column.
#[must_use]
pub fn rest_repo() -> crate::sql_restful::RestRepository {
    crate::sql_restful::RestRepository::new(TABLE).with_field_white_list([
        "ID",
        "Name",
        "TargetFormat",
        "Command",
        "DefaultBitRate",
    ])
}

/// `setSortMappings` for this repository: NONE. `transcodingRepository` never calls
/// it — the `nil` second `registerModel` argument (`:19`) only sets `filterMappings`
/// (`sql_base_repository.go:104-112`), so `sortMappings` stays nil and `sortMapping`
/// falls back to `toSnakeCase` for every field
/// (`sql_base_repository.go:165-183`, `:187-197`). Kept as an explicit constant so
/// the empty map is a pinned fact rather than an accident: it is why a caller sort
/// of `targetFormat` reaches SQLite as `target_format`.
pub const SORT_MAPPINGS: [(&str, &str); 0] = [];

/// `lookupSortMapping` over this repository's (empty) map
/// (`sql_base_repository.go:187-197`).
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort || *key == crate::sql_restful::to_snake_case(sort))
        .map(|(_, value)| *value)
}

/// `sortMapping` (`sql_base_repository.go:165-183`) over [`SORT_MAPPINGS`]: resolve
/// each comma-separated part on its own, snake-casing the parts it does not know,
/// so a mix of mapped keys and plain columns keeps the mappings it recognizes.
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

/// `buildSortOrder` (`sql_base_repository.go:199-228`) over THIS repository's
/// (empty) mappings: fold the caller's direction to `asc`/`desc` and flip a part
/// that spells its own direction. Local rather than reused from
/// `player_repo::build_sort_order`, which resolves THE PLAYER's mappings
/// (`player_repo.rs:557`) and would mis-map a transcoding sort — the same decision
/// `library_repo.rs:337` and `folder_repo.rs:261` record.
///
/// Empty-input behavior: a sort that resolves to no expression at all — a
/// whitespace-only string — returns an EMPTY string here (Go's `strings.Join` over
/// zero parts, `:227`), and [`read_all_query_sql`] then renders a trailing
/// `ORDER BY ` with nothing after it. That is a SQLite syntax error at execution,
/// not a builder error — see [`read_all_sql`] for why no
/// [`crate::sqlrepo::SqlError::EmptyOrder`] is ever produced — and Go's squirrel
/// renders the same dangling clause.
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

/// `sanitizeSort` for this repository's REST path (`sql_restful.go:69-86` via
/// `RestRepository::sanitize_sort`, plus the per-repository `lookupSortMapping` arm
/// Go validates against at `:72-77`). The mapping arm is vacuous because
/// [`SORT_MAPPINGS`] is empty — asserted by `sort_mappings_are_empty_as_a_pinned_fact`
/// — so a sort field survives only when it is a whitelisted field name; `order` folds
/// to `asc`/`desc` and an empty order stays empty (`:79-84`). A non-whitelisted sort
/// is DROPPED outright: Go logs `Ignoring sort not whitelisted` and applies no ORDER
/// BY at all (`sql_restful.go:74-77`); there is no logging dependency here so the
/// warning drops silently with the same semantics (`sql_restful.rs:16-19`).
///
/// NOTE that `command` IS whitelisted (`registerFieldWhiteList` merges
/// `model.Transcoding`'s struct fields, `sql_restful.go:154-158`), so this function
/// keeps a `sort=command` exactly as Go does. The security consequence — masking
/// blanks the FIELD but does not restrict ORDER BY over it — is therefore inherited
/// from Go, not introduced here; what this slice fixes is that the executed read must
/// go THROUGH this function, which Go does by calling `parseRestOptions` inside
/// `ReadAll` (`transcoding_repository.go:65`) and [`TranscodingRepo::read_all`] now
/// mirrors.
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

/// `parseRestOptions` for this repository (`sql_restful.go:54-67`): the parsed
/// [`QueryOptions`] that [`read_all_sql`] and [`count_sql`] consume — Go's `ReadAll`
/// (`transcoding_repository.go:65`) and `Count` (`:50`) option parsing in full,
/// including `sanitizeSort` (`:57`) and the `"seed"` lift-out (`:60-63`).
///
/// This is the ONLY sanctioned way to build the options handed to
/// [`read_all_query_sql`]: it is what makes the sort leg whitelist-checked before it
/// reaches `ORDER BY`.
#[must_use]
pub fn parse_rest_options(options: Option<&crate::sql_restful::RestQueryOptions>) -> QueryOptions {
    let Some(options) = options else {
        return QueryOptions::default();
    };
    let (sort, order) = sanitize_sort(&options.sort, &options.order);
    QueryOptions {
        sort,
        order,
        max: options.max,
        offset: options.offset,
        seed: options.filters.get("seed").cloned(),
        filters: rest_repo().parse_rest_filters(options),
    }
}

/// Errors of the EXECUTED transcoding reads: Go's failure set is `queryOne`'s
/// `sql.ErrNoRows` → `model.ErrNotFound` (`sql_base_repository.go:375-388`) plus the
/// statement/scan failure; the shape is `radio_repo::RadioError`
/// (`radio_repo.rs:426-483`) and `library_repo::LibraryError`
/// (`library_repo.rs:847-890`).
///
/// The MUTATING legs keep spelling [`ERR_PERMISSION_DENIED`] as data: Go's
/// `Put`/`Save`/`Update`/`Delete` return `rest.ErrPermissionDenied` before any
/// statement is built (`transcoding_repository.go:43`, `:81`, `:88`, `:98`) and those
/// legs stay render-only here, so no executed read of this repository can produce a
/// permission failure.
#[derive(Debug, thiserror::Error)]
pub enum TranscodingError {
    /// The SQLite statement failed, or a row would not scan. The reachable scan
    /// failure here is a NULL `default_bit_rate` in a column Go scans into a
    /// non-pointer `int` (`model/transcoding.go:14`):
    /// [`crate::hydrate::row_to_transcoding`] reports it through
    /// [`crate::hydrate::RowGet::get_i64`], NOT as [`TranscodingError::NotFound`] and
    /// NOT as a zero bit rate. The fresh DDL declares the column
    /// `default_bit_rate int default 192` with NO `not null`
    /// (`nd-dbschema/src/schema_ddl.rs:56`), so a NULL cell is reachable.
    ///
    /// Go's `ReadAll` DISCARDS the partially-filled slice on a `queryAll` error — it
    /// returns `nil, err` (`transcoding_repository.go:68-70`) — even though dbx appends
    /// each row only after a successful scan (`dbx@v1.12.0/rows.go:192`) and aborts at
    /// the failing one (`:169-171`), so the rows scanned before a failing row survive in
    /// Go's `res` and are then dropped on return. `Result` has no value-beside-error, so
    /// the port reports only the error and drops them — the same decision already
    /// recorded for [`crate::radio_repo::RadioRepo::get_all`] and
    /// [`crate::library_repo::LibraryRepo::get_all`].
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The statement could not be materialized — Go's `toSQL` failure
    /// (`sql_base_repository.go:358-373`), which `queryOne`/`queryAll` return unchanged
    /// (`:377-379`, `:446-448`). For this table the builder's failure modes are an unset
    /// table (`SqlError::NoTable`) and an empty order-by list
    /// (`SqlError::EmptyOrder`, declared at `sqlrepo.rs:36` but constructed NOWHERE in
    /// the crate), neither producible with the fixed `transcoding` table; the arm exists
    /// so the port does not invent a `rusqlite::Error` for a builder failure.
    #[error(transparent)]
    Builder(#[from] crate::sqlrepo::SqlError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`), which
    /// `queryOne` maps from `sql.ErrNoRows` (`sql_base_repository.go:382-385`,
    /// `dbx@v1.12.0/rows.go:237-245`). Only the two `queryOne` reads report it:
    /// [`TranscodingRepo::get`] and [`TranscodingRepo::find_by_format`], plus
    /// [`TranscodingRepo::read`] which delegates to `Get`.
    ///
    /// [`TranscodingRepo::read_all`] NEVER reports this variant: `queryAll`'s dbx `All`
    /// makes an empty non-nil slice for a zero-row result
    /// (`dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), so the ErrNoRows arm at
    /// `sql_base_repository.go:451-454` is dead code on that path. Nor does
    /// [`TranscodingRepo::count_all`]: `count` reads `res.Count` beside the error
    /// (`:572-574`), which the port reports as an error and defaults to 0.
    ///
    /// **Go quirk, ported as information rather than as a value:** Go returns
    /// `(&res, err)` (`transcoding_repository.go:27`, `:38`), so the pointer beside the
    /// error is NON-nil and points at a zero `Transcoding`; the port carries the same
    /// information without that dead pointer, which no Go caller can legitimately read.
    #[error("data not found")]
    NotFound,
}

/// `transcodingRepository` (`transcoding_repository.go:12-21`) bound to one pooled
/// connection — the executed READ side of the repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate has
/// no ambient context (the `sqlrepo.rs` DI rule), so the caller threads a lease from
/// [`crate::store::SQLStore::with_connection_sync`] (a transaction derefs to a
/// `&Connection`) and every statement runs through [`crate::execution`] — the port of
/// `queryOne`/`queryAll`/`count` (`sql_base_repository.go:375-388`, `:441-457`,
/// `:565-575`). Rows hydrate through [`crate::hydrate::row_to_transcoding`] — a SINGLE
/// phase: `grep -n "dbTranscoding\|PostScan" persistence/transcoding_repository.go` is
/// empty, so there is no second phase and no artwork layer for this table
/// (`hydrate-contract.md:218` lists Transcoding as hook-free).
///
/// No read takes a user: `transcoding` has no `user_id` column and the repository
/// defines no `userFilter` (`grep -n "userFilter" persistence/transcoding_repository.go`
/// is empty), so `newSelect` adds no ownership clause
/// (`sql_base_repository.go:139-147`). The admin flag is a plain `bool`, because Go's
/// check is `loggedUser(ctx).IsAdmin` (`transcoding_repository.go:58`, `:71`) with no
/// ownership comparison; a caller holding a `LoggedUser` (`player_repo.rs:113-118`)
/// passes its `is_admin`.
#[derive(Debug)]
pub struct TranscodingRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> TranscodingRepo<'a> {
    /// `NewTranscodingRepository` (`:16-21`): bind to `conn` (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`:23-28`): [`get_sql`] over the id, hydrated, and NOT masked.
    ///
    /// The real `command` is the point of this read: `core/players.go:87-91` resolves a
    /// player's profile through it and `core/stream/decider.go:315,330` hand the
    /// command to ffmpeg, so masking here would break streaming for a non-admin. Go's
    /// own test pins the unmasked command read by a regular user
    /// (`persistence/transcoding_repository_test.go:111-119` for `Get`,
    /// `:121-129` for `FindByFormat`).
    ///
    /// NotFound is `queryOne`'s no-rows condition (see [`TranscodingError::NotFound`]),
    /// decided before any field is read.
    ///
    /// # Errors
    /// [`TranscodingError::NotFound`] when the id matches nothing;
    /// [`TranscodingError::Sqlite`] on a statement or scan failure (see that variant
    /// for the NULL `default_bit_rate` case).
    pub fn get(&self, id: &str) -> Result<Transcoding, TranscodingError> {
        self.read_one(&get_sql(id))
    }

    /// `FindByFormat` (`:34-39`): [`find_by_format_sql`] over the format, hydrated,
    /// and NOT masked — the stream decider's lookup (`decider.go:315,330`).
    ///
    /// No ORDER BY and no LIMIT, exactly as in Go; the single-row semantics come from
    /// the DDL's `unique (target_format)` (`nd-dbschema/src/schema_ddl.rs:56`), which
    /// also makes a multi-row result unreachable for this statement.
    ///
    /// # Errors
    /// Same as [`Self::get`].
    pub fn find_by_format(&self, format: &str) -> Result<Transcoding, TranscodingError> {
        self.read_one(&find_by_format_sql(format))
    }

    /// `Read` (`:53-62`): [`Self::get`] plus [`mask_command`] for a non-admin.
    ///
    /// Masking blanks the field and still returns the row (`:59`), and the admin check
    /// runs AFTER `Get`, so a non-admin reading a missing id gets
    /// [`TranscodingError::NotFound`] with no masking step in between (`:55-57`).
    ///
    /// # Errors
    /// Same as [`Self::get`].
    pub fn read(&self, id: &str, is_admin: bool) -> Result<Transcoding, TranscodingError> {
        Ok(mask_command(self.get(id)?, is_admin))
    }

    /// `ReadAll` (`:64-77`): [`read_all_query_sql`] under options THIS method parses
    /// itself via [`parse_rest_options`], then [`mask_commands`] for a non-admin.
    ///
    /// The option parsing happens HERE rather than in the caller, mirroring Go's
    /// `ReadAll`, which calls `r.parseRestOptions(ctx, options…)` inside its own body
    /// (`transcoding_repository.go:65`). That is a SECURITY property, not a convenience:
    /// `sanitizeSort` (`sql_restful.go:69-86`) is what keeps a caller's sort string off
    /// `ORDER BY` unless it is a whitelisted field, and an earlier version of this method
    /// took an already-built [`QueryOptions`] — a shape nothing enforced — so a hostile
    /// sort such as `(CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END) asc` reached
    /// the statement verbatim and flipped row order for a non-admin whose `command` had
    /// already been blanked: a bit-at-a-time oracle on masked data. Taking
    /// [`crate::sql_restful::RestQueryOptions`] and parsing it here makes that path
    /// structurally impossible;
    /// the hostile-expression tests in
    /// `tests/runtime_transcoding_repo.rs` pin it. Masking still runs AFTER the query
    /// (`:71-74`), so the ORDER BY is evaluated against the REAL column exactly as in Go
    /// — which is why the whitelist, not the mask, is the actual boundary.
    ///
    /// An empty table is an empty `Vec` with no error (see
    /// [`TranscodingError::NotFound`]'s dbx proof), and masking never drops a row
    /// (`:72-74` rewrites the field in place).
    ///
    /// `options == None` is Go's empty variadic list: `parseRestOptions` returns the
    /// zero `model.QueryOptions` (`sql_restful.go:55-56,66`), which is
    /// [`QueryOptions::default`] — no filter, no page, no ORDER BY.
    ///
    /// The offset-without-limit page EXECUTES (`LIMIT -1 OFFSET n`) where Go's bare
    /// `OFFSET` is a SQLite syntax error — the builder's own cited deviation
    /// (`sqlrepo.rs:313-319`), pinned rather than "fixed".
    ///
    /// # Errors
    /// [`TranscodingError::Sqlite`] when the statement fails or a row will not scan;
    /// [`TranscodingError::Builder`] if the builder fails to materialize. Never
    /// [`TranscodingError::NotFound`].
    pub fn read_all(
        &self,
        options: Option<&crate::sql_restful::RestQueryOptions>,
        is_admin: bool,
    ) -> Result<Transcodings, TranscodingError> {
        let statement = read_all_query_sql(Some(&parse_rest_options(options)))?;
        let rows =
            crate::execution::query_all(self.conn, &statement, crate::hydrate::row_to_transcoding)?;
        Ok(mask_commands(rows, is_admin))
    }

    /// `CountAll` (`:30-31`): the DISTINCT aggregate of [`count_sql`] — filters kept,
    /// page and caller sort dropped. A table with no rows counts 0, never NotFound.
    ///
    /// It deliberately KEEPS the [`QueryOptions`] signature that [`Self::read_all`]
    /// abandoned: `count` replaces every order leg with `transcoding.id`
    /// (`sql_base_repository.go:569`) and drops the page (`:568`), so a caller sort
    /// cannot change either the row set or the order this returns — it is not an
    /// ordering channel and carries no [`sanitize_sort`]-style leak. [`Self::count`] is
    /// the REST-shaped entry point and parses options itself (`transcoding_repository.go:50`),
    /// which is where the whitelist check lives on that path.
    ///
    /// # Errors
    /// [`TranscodingError::Sqlite`] when the statement fails. Go's `count` returns
    /// `res.Count` beside a `queryOne` error (`sql_base_repository.go:572-574`), so a
    /// failing count surfaces both; the port reports only the error and the count then
    /// defaults to 0.
    pub fn count_all(&self, options: Option<&QueryOptions>) -> Result<i64, TranscodingError> {
        let statement = count_sql(options);
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `Count` (`:49-51`): `count(ctx, Select(), parseRestOptions(…))` — [`count_sql`]
    /// driven by rest options through [`parse_rest_options`] (option parsing at `:50`).
    /// Go hands `count` a fresh
    /// `Select()` rather than `newSelect`'s output, so a caller sort never reaches the
    /// statement and the count is invariant under sort and page (see [`count_sql`]).
    ///
    /// # Errors
    /// Same as [`Self::count_all`].
    pub fn count(
        &self,
        options: Option<&crate::sql_restful::RestQueryOptions>,
    ) -> Result<i64, TranscodingError> {
        self.count_all(Some(&parse_rest_options(options)))
    }

    /// `queryOne` over the transcoding select (`sql_base_repository.go:375-388`): one
    /// row, with `sql.ErrNoRows` mapped to [`TranscodingError::NotFound`].
    fn read_one(&self, statement: &Sql) -> Result<Transcoding, TranscodingError> {
        crate::execution::query_optional(self.conn, statement, crate::hydrate::row_to_transcoding)?
            .ok_or(TranscodingError::NotFound)
    }
}
/// The model's column set, `registerModel(&model.Transcoding{})`'s
/// `toSQLArgs` output in struct-declaration order (`model/transcoding.go`).
const COLUMNS: [&str; 5] = ["id", "name", "target_format", "command", "default_bit_rate"];

/// `put`'s `values` (`sql_base_repository.go:618-642` via `toSQLArgs`): the
/// column-value pairs in declaration order.
fn set_values(t: &Transcoding) -> (String, Vec<Value>) {
    (
        COLUMNS.join(", ").to_string(),
        vec![
            Value::from(t.id.clone()),
            Value::from(t.name.clone()),
            Value::from(t.target_format.clone()),
            Value::from(t.command.clone()),
            Value::Integer(t.default_bit_rate),
        ],
    )
}

/// `Put`/`Save`/`Update` (`:41-47`, `:79-84`, `:86-94`) via `put` — the
/// update-then-insert upsert pair (`sql_base_repository.go:618-642`). `Update` pins
/// `t.ID = id` first (`:91`), which the caller does before calling. `Put`/`Save`/`Update`
/// return [`ERR_PERMISSION_DENIED`] for non-admins (`:43`, `:81`, `:88`).
///
/// `# Errors`
/// [`ERR_PERMISSION_DENIED`] when `is_admin` is false.
pub fn put_sql(
    is_admin: bool,
    t: &Transcoding,
    cols_to_update: &[&str],
) -> Result<(Option<Sql>, Sql), &'static str> {
    if !is_admin {
        return Err(ERR_PERMISSION_DENIED);
    }
    // filterUpdateValues (sql_base_repository.go:612-616): selectUpdateColumns
    // (:593-610) keeps only requested cols (all when none given), then pins the id.
    let (_, values) = set_values(t);
    let mut set_args = Vec::new();
    let mut set_cols = Vec::new();
    for (col, value) in COLUMNS.iter().zip(values.iter()) {
        if cols_to_update.is_empty() || cols_to_update.iter().any(|c| c == col) {
            set_cols.push((*col).to_string());
            set_args.push(value.clone());
        }
    }
    let update = if t.id.is_empty() {
        None
    } else {
        let assignments = set_cols
            .iter()
            .zip(set_args.iter())
            .map(|(c, _)| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut args = set_args.clone();
        args.push(Value::from(t.id.clone()));
        Some((
            format!("UPDATE {TABLE} SET {assignments} WHERE id = ?"),
            args,
        ))
    };
    // Insert leg: the model's full column set (Go SetMap, all values).
    let (cols, vals) = set_values(t);
    let placeholders = vec!["?"; COLUMNS.len()].join(", ");
    let insert = (
        format!("INSERT INTO {TABLE} ({cols}) VALUES ({placeholders})"),
        vals,
    );
    Ok((update, insert))
}

/// `Delete` (`:96-105`) via `deleteByID` (`sql_base_repository.go:650-659`): a
/// missing row is Go's `model.ErrNotFound` (`model/errors.go:10` →
/// `rest.ErrNotFound`, text `data not found`, `deluan/rest@v1.0.1/errors.go:14`),
/// spelled here as DATA rather than as an error variant — the same convention as
/// [`ERR_PERMISSION_DENIED`] next to it.
///
/// NOTE the text divergence: this constant is `"not found"`, NOT Go's
/// `"data not found"`, and NOTHING in this crate references it. `delete_sql` only
/// renders statements, so the miss would surface as the caller's own NotFound
/// handling; the executed path's NotFound text is
/// [`TranscodingError::NotFound`], which DOES carry Go's text.
pub const ERR_NOT_FOUND: &str = "not found";

/// `Delete` (`:96-105`): one delete per id; non-admins get
/// [`ERR_PERMISSION_DENIED`] (`:97-99`, `rest.ErrPermissionDenied` returned at `:98`).
///
/// `# Errors`
/// [`ERR_PERMISSION_DENIED`] when `is_admin` is false.
pub fn delete_sql(is_admin: bool, ids: &[&str]) -> Result<Vec<Sql>, &'static str> {
    if !is_admin {
        return Err(ERR_PERMISSION_DENIED);
    }
    Ok(ids
        .iter()
        .map(|id| {
            (
                format!("DELETE FROM {TABLE} WHERE id = ?"),
                vec![Value::from(*id)],
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_find_by_format_bind_identity_values() {
        // Arrange/Act: Get (`:23-28`, statement `:24`) and FindByFormat (`:34-39`,
        // statement `:35`).
        let get = get_sql("t1");
        let find = find_by_format_sql("mp3");
        // Assert:
        assert_eq!(get.0, "SELECT * FROM transcoding WHERE id = ?");
        assert_eq!(get.1, vec![Value::Text("t1".to_string())]);
        assert_eq!(find.0, "SELECT * FROM transcoding WHERE target_format = ?");
        assert_eq!(find.1, vec![Value::Text("mp3".to_string())]);
    }

    #[test]
    fn count_drops_pagination_and_keeps_filters() {
        // Arrange: base count() (`transcoding_repository.go:30-31` via
        // sql_base_repository.go:565-575)
        // with one filter and a page/sort that must not survive it.
        let options = QueryOptions {
            sort: "name".to_string(),
            order: "desc".to_string(),
            max: 5,
            offset: 3,
            filters: Some(vec![crate::sql_restful::eq_filter("target_format", "mp3")]),
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = count_sql(Some(&options));
        // Assert: the filter survives, the page is dropped, and the sort is
        // REPLACED by `count`'s reset `ORDER BY transcoding.id`
        // (`sql_base_repository.go:569`). This expectation was previously
        // `… FROM transcoding WHERE target_format = ?` with no ORDER BY, which
        // contradicted Go's trace and the sibling renderers
        // (`radio_repo.rs:248`, `library_repo.rs:244`); the correction is the
        // parity fix this slice exists for.
        assert_eq!(
            sql,
            "SELECT count(distinct transcoding.id) as count FROM transcoding WHERE target_format = ? ORDER BY transcoding.id"
        );
        assert_eq!(args, vec![Value::Text("mp3".to_string())]);
    }

    #[test]
    fn count_renders_the_reset_order_with_no_options() {
        // Arrange/Act: CountAll (:30-31) with no options at all.
        let (sql, args) = count_sql(None);
        // Assert: Go's traced count text for a bare `Select()`.
        assert_eq!(
            sql,
            "SELECT count(distinct transcoding.id) as count FROM transcoding ORDER BY transcoding.id"
        );
        assert!(args.is_empty());
    }

    #[test]
    fn read_all_selects_star_with_sort_and_pagination() {
        // Arrange: ReadAll (`:64-67`) with options of the parsed shape.
        let options = QueryOptions {
            sort: "name".to_string(),
            order: "asc".to_string(),
            max: 20,
            ..QueryOptions::default()
        };
        // Act: the renderer (NOT `TranscodingRepo::read_all`, which parses its own).
        let (sql, args) = read_all_sql(Some(&options));
        // Assert: `applyOptions`' ORDER BY leg (`sql_base_repository.go:157-159`)
        // now reaches the statement. This expectation previously read
        // "SELECT * FROM transcoding LIMIT 20" — a sort that changed nothing; the
        // fix strengthens the pin rather than weakening it.
        assert_eq!(sql, "SELECT * FROM transcoding ORDER BY name asc LIMIT 20");
        // Assert: squirrel renders the bound as a literal (`select.go:140-143`).
        assert!(args.is_empty());
    }

    #[test]
    fn read_all_renders_filters_then_order_then_page() {
        // Arrange: a filter, a desc sort and a page.
        let options = QueryOptions {
            sort: "targetFormat".to_string(),
            order: "desc".to_string(),
            max: 2,
            offset: 1,
            filters: Some(vec![crate::sql_restful::eq_filter("name", "mp3 audio")]),
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = read_all_sql(Some(&options));
        // Assert: squirrel's by-kind clause order — WHERE before ORDER BY before
        // LIMIT/OFFSET (`sqlrepo.rs:343-372`), which is Go's executed order.
        assert_eq!(
            sql,
            "SELECT * FROM transcoding WHERE name = ? ORDER BY target_format desc LIMIT 2 OFFSET 1"
        );
        assert_eq!(args, vec![Value::Text("mp3 audio".to_string())]);
    }

    #[test]
    fn read_all_without_options_has_no_legs_at_all() {
        // Arrange/Act: Go's `newSelect(ctx)` with an empty option list
        // (`sql_base_repository.go:139-147`).
        let (sql, args) = read_all_sql(None);
        // Assert:
        assert_eq!(sql, "SELECT * FROM transcoding");
        assert!(args.is_empty());
    }

    #[test]
    fn sort_mappings_are_empty_as_a_pinned_fact() {
        // Assert: `setSortMappings` is NEVER called for this repository
        // (`transcoding_repository.go:19` passes `nil` to `registerModel`, which only
        // sets `filterMappings`, sql_base_repository.go:104-112), so `sortMappings`
        // stays nil and every sort field falls through to `toSnakeCase`
        // (`:165-183`). Without this assertion the empty map is a vacuous claim: an
        // accidental player-style entry would still make every other test pass.
        assert!(SORT_MAPPINGS.is_empty());
        // Assert: the fallback rather than a mapping — `userName` has no entry, so it
        // reaches SQLite snake_cased. `player_repo::SORT_MAPPINGS` DOES map
        // `user_name`→`username` (`player_repo.rs:557`); if that map ever leaked into
        // this one, this assertion is the one that fails.
        assert_eq!(build_sort_order("userName", "asc"), "user_name asc");
        // Assert: `lookupSortMapping` (`sql_base_repository.go:187-197`) over the empty
        // map resolves nothing, for both the raw and the snake_cased key. Directly,
        // not only transitively through `mapped_sort`.
        assert_eq!(sort_mapping("userName"), None);
        assert_eq!(sort_mapping("user_name"), None);
        assert_eq!(sort_mapping("targetFormat"), None);
        assert_eq!(sort_mapping("target_format"), None);
        assert_eq!(sort_mapping(""), None);
        // Assert: `sortMapping` (`sql_base_repository.go:165-183`) resolves each
        // comma-separated part on its own and snake-cases every part it does not know.
        assert_eq!(mapped_sort("targetFormat"), "target_format");
        assert_eq!(mapped_sort("Name,defaultBitRate"), "name, default_bit_rate");
        assert_eq!(mapped_sort("userName id"), "user_name id");
        // Assert: mapped_sort is NOT identity — it rewrites case and separators.
        assert_ne!(mapped_sort("targetFormat"), "targetFormat");
        // Assert: the combined path, `buildSortOrder` over `mapped_sort`.
        assert_eq!(
            build_sort_order("Name,defaultBitRate", "desc"),
            "name desc, default_bit_rate desc"
        );
    }

    #[test]
    fn sanitize_sort_keeps_whitelisted_fields_and_drops_the_rest() {
        // Arrange/Act: sanitizeSort (`sql_restful.go:69-86`) over the transcoding
        // whitelist (5 model fields + the 7 annotation fields,
        // `sql_restful.go:154-162`).
        // Assert: a model field survives, snake_cased, with a folded order.
        assert_eq!(
            sanitize_sort("TargetFormat", "DESC"),
            ("target_format".to_string(), "desc".to_string())
        );
        assert_eq!(
            sanitize_sort("defaultBitRate", "weird"),
            ("default_bit_rate".to_string(), "asc".to_string())
        );
        // Assert: a non-whitelisted field is dropped outright, so no ORDER BY
        // leg is rendered at all (`read_all_sql` skips an empty sort).
        assert_eq!(
            sanitize_sort("bogus", "desc"),
            (String::new(), "desc".to_string())
        );
        // Assert: the annotation fields ARE whitelisted even though the table has
        // no such columns (Go behaves identically — the ORDER BY then errors in
        // SQLite).
        assert_eq!(
            sanitize_sort("starred", ""),
            ("starred".to_string(), String::new())
        );
        // Assert: an empty sort keeps an empty order, and BOTH halves are checked
        // against a value the identity function could not produce. `sanitize_sort("",
        // "desc")` alone would pass if the function simply returned its input, because
        // `("", "desc")` is its own fixpoint; the folds below are not.
        assert_eq!(sanitize_sort("", ""), (String::new(), String::new()));
        assert_eq!(
            sanitize_sort("", "DESC"),
            (String::new(), "desc".to_string())
        );
        assert_eq!(
            sanitize_sort("", "weird"),
            (String::new(), "asc".to_string())
        );
        assert_eq!(
            sanitize_sort("", " Desc "),
            (String::new(), "asc".to_string())
        );
        // Assert: a whitespace-only sort is NOT a sort. Identity would hand the
        // whitespace straight through and `read_all_sql` would then append
        // `ORDER BY  `; the whitelist rejects it, so the sort is dropped.
        assert_eq!(
            sanitize_sort("  ", "desc"),
            (String::new(), "desc".to_string())
        );
        assert_eq!(sanitize_sort(" \t", ""), (String::new(), String::new()));
        // Assert: `command` IS whitelisted, so a bare `command` sort SURVIVES here
        // exactly as it does in Go (`sanitizeSort` validates against the whitelist,
        // `sql_restful.go:72-77`, and the whitelist is built from `model.Transcoding`'s
        // fields, `:154-158`). That is the inherited Go behavior; the protection is the
        // whitelist, not the mask, which is why the hostile-expression test below matters.
        assert_eq!(
            sanitize_sort("Command", "DESC"),
            ("command".to_string(), "desc".to_string())
        );
        // Assert: buildSortOrder over the empty SORT_MAPPINGS snake-cases and
        // folds the direction (`sql_base_repository.go:199-228`). A part that spells
        // its own direction does NOT simply win: Go compares it against the literal
        // `"asc"` only (`:219-223`), so `name desc` under `order=asc` keeps `desc`
        // (the reverse of the folded `asc`) and `name asc` under `order=desc` becomes
        // `desc`. Go's quirk, pinned rather than "fixed".
        assert_eq!(
            build_sort_order("TargetFormat", "desc"),
            "target_format desc"
        );
        assert_eq!(build_sort_order("name desc", "asc"), "name desc");
        assert_eq!(build_sort_order("name asc", "desc"), "name desc");
        assert_eq!(build_sort_order("name", ""), "name asc");
        // Assert: a whitespace-only sort yields an EMPTY order expression, and
        // `read_all_query_sql` then renders a dangling `ORDER BY ` (a SQLite syntax
        // error at execution, not a builder error — `SqlError::EmptyOrder` is never
        // constructed). Go's squirrel renders the same clause, so this is pinned as
        // parity rather than "fixed"; it is unreachable through
        // [`TranscodingRepo::read_all`], which sanitizes first.
        assert_eq!(build_sort_order("  ", "asc"), "");
        assert_eq!(
            read_all_query_sql(Some(&QueryOptions {
                sort: "  ".to_string(),
                order: "asc".to_string(),
                ..QueryOptions::default()
            }))
            .expect("the builder never errors — EmptyOrder is never constructed")
            .0,
            "SELECT * FROM transcoding ORDER BY "
        );
    }

    #[test]
    fn a_hostile_sort_is_dropped_and_renders_no_order_leg() {
        // The attack this slice closes: Go's `ReadAll` parses its own options
        // (`transcoding_repository.go:65`) so `sanitizeSort` (`sql_restful.go:69-86`)
        // always runs; an earlier version of `TranscodingRepo::read_all` took an
        // already-built `QueryOptions`, and this expression reached ORDER BY verbatim.
        let hostile = crate::sql_restful::RestQueryOptions {
            sort: "(CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END) asc".to_string(),
            order: "asc".to_string(),
            ..crate::sql_restful::RestQueryOptions::default()
        };
        // Act: through the sanitizing entry point.
        let parsed = parse_rest_options(Some(&hostile));
        // Assert: the sort is gone; only the direction survives, and a direction alone
        // renders no leg (`read_all_query_sql` keys on `sort`).
        assert_eq!(parsed.sort, "");
        assert_eq!(parsed.order, "asc");
        let (sql, args) = read_all_sql(Some(&parsed));
        // Assert: no ORDER BY leg at all, and no fragment of the injected expression
        // anywhere in the statement or its args.
        assert!(!sql.contains("ORDER BY"), "{sql}");
        assert!(!sql.to_lowercase().contains("case"), "{sql}");
        assert!(!sql.contains("secret-a"), "{sql}");
        assert!(args.is_empty());
        // Assert: the same expression handed to the RAW renderer still reaches ORDER BY
        // — which is exactly why the renderer's doc says it must receive options
        // already produced by `parse_rest_options`.
        let unsanitized = QueryOptions {
            sort: "(CASE WHEN command LIKE '%secret-a%' THEN 1 ELSE 0 END) asc".to_string(),
            order: "asc".to_string(),
            ..QueryOptions::default()
        };
        assert!(
            read_all_sql(Some(&unsanitized))
                .0
                .contains("case when command"),
            "the renderer is documented as unsanitized; if this stops rendering the \
             injected expression, the WARNING on read_all_query_sql is stale"
        );
        assert!(
            read_all_sql(Some(&unsanitized)).0.contains("secret-a"),
            "the raw renderer leaks the secret substring, which is the point of the warning"
        );
    }

    #[test]
    fn a_whitelisted_command_sort_reaches_the_statement_like_go() {
        // `command` is a whitelisted field, so a bare `sort=command` is kept here AND
        // in Go (`sql_restful.go:72-77`). Pinned so the parity is visible: masking
        // blanks the returned field but does not restrict ORDER BY over it.
        let parsed = parse_rest_options(Some(&crate::sql_restful::RestQueryOptions {
            sort: "command".to_string(),
            order: "asc".to_string(),
            ..crate::sql_restful::RestQueryOptions::default()
        }));
        assert_eq!(parsed.sort, "command");
        assert_eq!(
            read_all_sql(Some(&parsed)).0,
            "SELECT * FROM transcoding ORDER BY command asc"
        );
    }

    #[test]
    fn parse_rest_options_applies_the_whitelist_to_filters_and_sort() {
        // Arrange: a whitelisted filter, a camelCase filter KEY, a non-whitelisted
        // one, a whitelisted sort and a page (Go's `parseRestOptions`,
        // `sql_restful.go:54-67`). Filter keys are matched against the whitelist
        // after `strings.ToLower` but WITHOUT snake-casing (`:31`, `:39`), so a
        // camelCase `targetFormat` key is dropped exactly like an unknown one —
        // pinned here rather than guessed.
        let options = crate::sql_restful::RestQueryOptions {
            sort: "Name".to_string(),
            order: "desc".to_string(),
            max: 3,
            offset: 2,
            filters: std::collections::BTreeMap::from([
                ("target_format".to_string(), "mp3".to_string()),
                ("targetFormat".to_string(), "mp3".to_string()),
                ("bogus".to_string(), "x".to_string()),
            ]),
        };
        // Act:
        let parsed = parse_rest_options(Some(&options));
        // Assert: sort/page carried, the whitelisted filter kept (a non-`id` field
        // takes the "starts with" arm, `sql_restful.go:48-49`), the other two dropped.
        assert_eq!(parsed.sort, "name");
        assert_eq!(parsed.order, "desc");
        assert_eq!(parsed.max, 3);
        assert_eq!(parsed.offset, 2);
        assert_eq!(
            parsed.filters,
            Some(vec![crate::sql_restful::Filter::Like {
                column: "target_format".to_string(),
                pattern: "mp3%".to_string(),
            }])
        );
        // Assert: the parsed shape renders the full statement Go traces.
        assert_eq!(
            read_all_sql(Some(&parsed)).0,
            "SELECT * FROM transcoding WHERE target_format LIKE ? ORDER BY name desc LIMIT 3 OFFSET 2"
        );
        // Assert: None is Go's empty option list.
        assert_eq!(parse_rest_options(None), QueryOptions::default());
    }

    #[test]
    fn mask_command_blanks_command_only_for_non_admins() {
        // Arrange: Read (`:58-59`) / ReadAll (`:72-74`) masking.
        let t = Transcoding {
            command: "ffmpeg %s".to_string(),
            ..Transcoding::default()
        };
        // Act/Assert:
        assert_eq!(mask_command(t.clone(), true).command, "ffmpeg %s");
        assert_eq!(mask_command(t.clone(), false).command, "");
        let masked = mask_commands(vec![t], false);
        assert_eq!(masked[0].command, "");
    }

    #[test]
    fn put_requires_admin_and_emits_update_then_insert() {
        // Arrange: Put (`:41-47`) over put()'s upsert (`sql_base_repository.go:618-642`).
        let t = Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "mp3".to_string(),
            command: "ffmpeg -i %s".to_string(),
            default_bit_rate: 160,
        };
        // Act:
        let denied = put_sql(false, &t, &[]);
        let (update, insert) = put_sql(true, &t, &[]).expect("admin put renders");
        // Assert: non-admin is refused before any statement is built.
        assert_eq!(denied, Err(ERR_PERMISSION_DENIED));
        let (sql, args) = update.expect("an id-bearing put updates first");
        assert_eq!(
            sql,
            "UPDATE transcoding SET id = ?, name = ?, target_format = ?, command = ?, default_bit_rate = ? WHERE id = ?"
        );
        assert_eq!(args.len(), 6);
        let (sql, args) = insert;
        assert_eq!(
            sql,
            "INSERT INTO transcoding (id, name, target_format, command, default_bit_rate) VALUES (?, ?, ?, ?, ?)"
        );
        assert_eq!(args[4], Value::Integer(160));
    }

    #[test]
    fn put_update_respects_cols_to_update() {
        // Arrange: Update's colsToUpdate (`:86-94`) narrowing the SET list.
        let t = Transcoding {
            id: "t1".to_string(),
            name: "mp3 audio".to_string(),
            target_format: "opus".to_string(),
            command: "c".to_string(),
            default_bit_rate: 96,
        };
        // Act:
        let (update, _) = put_sql(true, &t, &["default_bit_rate"]).expect("admin put renders");
        // Assert: only the requested column (and the WHERE id) bind.
        let (sql, args) = update.expect("the update leg renders");
        assert_eq!(
            sql,
            "UPDATE transcoding SET default_bit_rate = ? WHERE id = ?"
        );
        assert_eq!(
            args,
            vec![Value::Integer(96), Value::Text("t1".to_string())]
        );
    }

    #[test]
    fn delete_requires_admin_and_deletes_each_id() {
        // Arrange: Delete (`:96-105`) via deleteByID (`sql_base_repository.go:650-659`).
        // Act:
        let denied = delete_sql(false, &["t1"]);
        let stmts = delete_sql(true, &["t1", "t2"]).expect("admin delete renders");
        // Assert:
        assert_eq!(denied, Err(ERR_PERMISSION_DENIED));
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "DELETE FROM transcoding WHERE id = ?");
        assert_eq!(stmts[0].1, vec![Value::Text("t1".to_string())]);
        assert_eq!(stmts[1].1, vec![Value::Text("t2".to_string())]);
    }
}
