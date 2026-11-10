//! Port of `persistence/player_repository.go` (231L) — the player repository's
//! SQL shaping plus its executed read/write path, including the API-key half
//! that `/player` and API-key issuance (#26) depend on.
//!
//! Like `user_repo.rs`, the shaping ports stop at the same `(sql, args)` pairs
//! `toSQL()` produces (`sql_base_repository.go:358-372`) and [`PlayerRepo`] is
//! the `executeSQL`/`queryOne`/`queryAll` half
//! (`sql_base_repository.go:336-355,375-388,441-457`) run through
//! [`crate::execution`] on a leased connection.
//!
//! There is no `model.Player` DTO in nd-model, so [`PlayerRecord`] is local to
//! this module exactly the way [`crate::user_repo::UserRecord`] is — nd-model is
//! untouched.
//!
//! Ownership: Go derives the logged user from the ambient `ctx`
//! (`sql_base_repository.go:53-57`), which the crate does not have (the
//! `sqlrepo.rs` DI rule, as in `share_repo.rs`/`radio_repo.rs`). The caller
//! threads a [`LoggedUser`] explicitly; [`PlayerRepo::save`] additionally takes
//! the logged id it stamps onto the row, because Go reads it from the same
//! context (`player_repository.go:134-140`).
//!
//! SQL rendering follows squirrel v1.5.4 literally, since that is the library
//! Go builds these statements with: `SetMap` sorts its column keys
//! (`squirrel@v1.5.4/insert.go:271-287`, `squirrel@v1.5.4/update.go:230-243`), `Eq` sorts its keys and an `And`
//! wraps its parts in parens (`squirrel@v1.5.4/expr.go:161,369-388`), an empty `And`
//! renders the constant `(1=1)` (`squirrel@v1.5.4/expr.go:370-372`), `Columns` extends rather
//! than replaces (`squirrel@v1.5.4/select.go:257-263`), and every accumulated `Where` is
//! AND-joined in call order. Two deliberate render deviations, both
//! behaviour-neutral and pinned by tests:
//! - the owner JOIN is emitted as `INNER JOIN` because
//!   [`crate::sqlrepo::Repository`] has no bare-`JOIN` verb; SQLite treats the
//!   two as identical (`squirrel@v1.5.4/select.go:298-300`
//!   renders `JOIN ` + the fragment, `:313-315` renders `INNER JOIN ` + the same
//!   fragment; SQLite treats the two as identical).
//! - the sort-expression splitter skips Go's paren tracking
//!   (`sql_base_repository.go:230-245`): this repository's only mapping value is
//!   the bare `username`, so no parenthesized expression with embedded spaces
//!   can reach it.
//!
//! Timestamps: `last_seen` is the stored datetime TEXT the caller stamps (Go
//! binds `time.Time`, which go-sqlite3 writes as datetime text), so stored bytes
//! stay identical to the Go writer's — unlike `share_repo.rs`'s epoch-seconds
//! convention. `None` binds NULL, which Go cannot produce through `Put` (its
//! `LastSeen` is a non-pointer `time.Time`).
//!
//! Schema dependency: the unique index that settles key races is added by the Go
//! migration `db/migrations/20260924010054_add_player_api_key_hash.sql:4`, not
//! by `nd-dbschema`'s fresh DDL (`schema_ddl.rs:65` declares the column only,
//! and the crate emits no secondary indexes at all — see the gap report in the
//! lane hand-off). [`crate::store`]-level callers must ensure that index exists;
//! `tests/runtime_player_repo.rs` creates it to mirror a migrated Go database.

use std::collections::HashMap;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::execution;
use crate::sql_bookmarks::INVALID_USER_ID;
use crate::sql_restful::{
    contains_filter, to_snake_case, Filter, FilterFn, QueryOptions, RestQueryOptions,
    RestRepository,
};
use crate::sqlrepo::{Repository, Value};
use crate::store::SQLStore;

/// `r.tableName` (`player_repository.go:25` via `registerModel(&model.Player{})`,
/// `sql_base_repository.go:104-112`).
pub const PLAYER_TABLE: &str = "player";

/// `consts.APIKeyPrefix` (`consts/consts.go:52`), the literal half of the key
/// format regex (`player_repository.go:120`).
pub const API_KEY_PREFIX: &str = "nds_";

/// Characters after the prefix in a valid key (`^nds_[0-9A-Za-z]{22}$`,
/// `player_repository.go:120`).
pub const API_KEY_BODY_LENGTH: usize = 22;

/// `'NavidromeUI'`, the client the `CountByClient` CASE substitutes the player
/// name for (`player_repository.go:80`). It is a Go literal with no `consts/`
/// home; verified by grep over the Go tree.
pub const NAVIDROME_UI_CLIENT: &str = "NavidromeUI";

/// The player select's computed `has_api_key` column
/// (`player_repository.go:49`).
pub const HAS_API_KEY_COLUMN: &str = "player.api_key_hash is not null as has_api_key";

/// The joined owner name column (`player_repository.go:51`).
pub const USERNAME_COLUMN: &str = "user.user_name username";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// One `SET column = ?` pair, the shape squirrel's `SetMap` sorts and renders
/// (`squirrel@v1.5.4/update.go:230-243`). Column names are this repository's registered
/// identifiers, never user text; values are always bound.
pub type SetPair = (&'static str, Value);

/// `writeAccess` (`sql_base_repository.go:91-95`): who may change a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAccess {
    /// `ownerOrAdmin` (`:92`) — admins may write any row.
    OwnerOrAdmin,
    /// `ownerOnly` (`:93`) — even admins may only write their own rows.
    OwnerOnly,
}

/// `loggedUser(ctx)` (`sql_base_repository.go:53-57`): the caller's identity.
///
/// Go falls back to `&model.User{ID: invalidUserId}` when the context carries no
/// user; [`LoggedUser::headless`] is that arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedUser {
    /// `model.User.ID`.
    pub id: String,
    /// `model.User.IsAdmin`.
    pub is_admin: bool,
}

impl LoggedUser {
    /// The no-user arm of `loggedUser` (`:54-56`): `ID == INVALID_USER_ID`,
    /// not an admin.
    #[must_use]
    pub fn headless() -> Self {
        Self {
            id: INVALID_USER_ID.to_string(),
            is_admin: false,
        }
    }
}

/// `model.QueryOptions` for this repository, i.e. the output of
/// `parseRestOptions` (`sql_restful.go:55-67`) narrowed to what the player
/// queries consume.
///
/// Go's `QueryOptions.Filters` is a `squirrel.Gt`/`Eq`/`And` Sqlizer built by
/// the caller or by `parseRestFilters` (`model/query_options.go`), so a value
/// travels inside it. [`crate::sql_restful::QueryOptions`] models filters as
/// renderable clauses with no place to keep those values, which is why the
/// count/census paths carry them here: without it, Go's
/// `Filters: squirrel.Gt{"last_seen": …}` (`core/metrics/insights.go:305-307`)
/// would silently become a filter-less census over the whole table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    /// Sanitized sort field (`sanitizeSort`, empty when dropped).
    pub sort: String,
    /// Sanitized direction (`asc`/`desc`, empty when unset).
    pub order: String,
    /// Page size (`Max`).
    pub max: i64,
    /// Page start (`Offset`).
    pub offset: i64,
    /// The `WHERE` body (`player_repository.go` callers feed it either parsed
    /// rest filters or a hand-built Sqlizer).
    pub filters: Option<String>,
    /// Values bound to the `?` placeholders in [`Self::filters`], in order.
    pub filter_args: Vec<Value>,
}

impl Options {
    /// `parseRestOptions` (`sql_restful.go:55-67`) for this repository: sanitize
    /// sort/order through [`sanitize_sort`], copy `Max`/`Offset`, drop `"seed"`
    /// (`:38-40`, nothing here uses it), and parse the remaining filters through
    /// [`rest_repo`], keeping their bound values.
    #[must_use]
    pub fn from_rest(options: Option<&RestQueryOptions>) -> Self {
        let Some(options) = options else {
            return Self::default();
        };
        let (sort, order) = sanitize_sort(&options.sort, &options.order);
        // `QueryOptions::where_clause` renders the parsed filters and their
        // bound values together (sql_restful.go:133-145), so the values survive
        // the trip into this repository's Options.
        let parsed = rest_repo().parse_rest_options(Some(options));
        let (filters, filter_args) = match parsed.where_clause() {
            Some((sql, args)) => (Some(sql), args),
            None => (None, Vec::new()),
        };
        Self {
            sort,
            order,
            max: options.max,
            offset: options.offset,
            filters,
            filter_args,
        }
    }
}

/// The hydrated player: `model.Player` (`model/player.go:10-26`).
///
/// The field set is exactly what dbx's `ScanStruct` populates — it maps each
/// column onto a field by its `db` tag and falls back to the field name
/// (`dbx@v1.12.0/struct.go:160-212`, `dbx@v1.12.0/rows.go:75-86`), so `structs:"-"` is irrelevant to
/// reads and only decides what `toSQLArgs` writes (`helpers.go:20-43` walks
/// `structs.Map`, which skips the `structs:"-"` fields entirely):
/// - `username` is NOT an insert column but IS scanned, because the select's
///   joined `user.user_name username` matches the field name.
/// - `has_api_key` is scanned through its `db:"has_api_key"` tag, which is why
///   the select computes it.
/// - `api_key` is `structs:"-"` too and the select has no `api_key` column at
///   all, so the column lands in dbx's throwaway `sql.NullString`
///   (`dbx@v1.12.0/rows.go:80-86`): the plaintext key is never read back and reads always
///   hand back `None`. `api_key_hash` is likewise scanned into the throwaway —
///   `model.Player` has no field for it — so the record does not carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerRecord {
    /// `username` — the joined `user.user_name`, read-only (never written).
    pub username: String,
    /// `player.id`.
    pub id: String,
    /// `player.name`.
    pub name: String,
    /// `player.user_agent`.
    pub user_agent: String,
    /// `player.user_id`.
    pub user_id: String,
    /// `player.client`.
    pub client: String,
    /// `player.ip`.
    pub ip: String,
    /// `player.last_seen` — stored datetime text, `None` for NULL.
    pub last_seen: Option<String>,
    /// `player.transcoding_id`.
    pub transcoding_id: String,
    /// `player.max_bit_rate`.
    pub max_bit_rate: i64,
    /// `player.report_real_path`.
    pub report_real_path: bool,
    /// `player.scrobble_enabled`.
    pub scrobble_enabled: bool,
    /// `has_api_key` — the select's computed column, never a written column.
    pub has_api_key: bool,
    /// The plaintext key: input-only (Save/Update), always `None` on reads.
    pub api_key: Option<String>,
}

/// Failures surfaced by the executed player repository.
///
/// Validation errors are typed and carry the Go message key rather than an HTTP
/// status: Go returns `&rest.ValidationError{Errors: {"apiKey": msg}}`
/// (`player_repository.go:122-124`) and the API layer does the translation.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    /// The SQLite statement failed (read or write).
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The store boundary failed (checkout, commit, or exhausted busy retry).
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),

    /// No row matched — Go's `model.ErrNotFound` from `queryOne`'s
    /// `sql.ErrNoRows` mapping (`sql_base_repository.go:382-385`), and
    /// `rest.ErrNotFound` from `classifyOwnedWriteMiss`
    /// (`sql_base_repository.go:554-563`). Both Go errors read "record not
    /// found"; the port keeps one variant.
    #[error("record not found")]
    NotFound,

    /// The caller may not touch the row — `rest.ErrPermissionDenied`, from
    /// `Save`'s owner check (`player_repository.go:138-140`) and from
    /// `classifyOwnedWriteMiss` when the row exists but is not the caller's
    /// (`sql_base_repository.go:559-561`).
    #[error("not allowed to modify this record")]
    PermissionDenied,

    /// `apiKeyValidationError` (`player_repository.go:122-124`): the key failed
    /// validation, carrying the `rest.ValidationError` message key.
    #[error("apiKey: {0}")]
    ApiKeyValidation(ApiKeyValidationError),
}

/// `rest.ValidationError` restricted to the one field this repository
/// validates: the `apiKey` field and its message key
/// (`player_repository.go:122-124`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiKeyValidationError {
    /// The `Errors` map key — always `"apiKey"` in Go.
    pub field: &'static str,
    /// The localization message key, e.g. `ra.validation.unique`.
    pub message_key: &'static str,
}

impl ApiKeyValidationError {
    /// `apiKeyValidationError("ra.validation.required")`
    /// (`player_repository.go:142-144`).
    #[must_use]
    pub fn required() -> Self {
        Self {
            field: "apiKey",
            message_key: "ra.validation.required",
        }
    }

    /// `validateAPIKey`'s failure (`player_repository.go:126-131`).
    #[must_use]
    pub fn format() -> Self {
        Self {
            field: "apiKey",
            message_key: "resources.player.validation.apiKeyFormat",
        }
    }

    /// The unique-index collision arms (`player_repository.go:156-158,219-221`).
    #[must_use]
    pub fn unique() -> Self {
        Self {
            field: "apiKey",
            message_key: "ra.validation.unique",
        }
    }

    /// The `rest.Errors` map this error stands for.
    #[must_use]
    pub fn errors(&self) -> HashMap<String, String> {
        HashMap::from([(self.field.to_string(), self.message_key.to_string())])
    }
}

impl std::fmt::Display for ApiKeyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message_key.fmt(f)
    }
}

/// `hasAPIKeyFilter` (`player_repository.go:35-40`): the `hasapikey` filter
/// mapping (`:27`). Go folds the value case-insensitively
/// (`strings.EqualFold(v, "true")`), so `true`/`TRUE`/`True` select rows whose
/// hash is NOT NULL and anything else selects `IS NULL`.
///
/// squirrel renders `NotEq{"col": nil}` as `col IS NOT NULL` and
/// `Eq{"col": nil}` as `col IS NULL` with no bound args (`squirrel@v1.5.4/expr.go:139-206`), so
/// the clause is a bare fragment.
#[must_use]
pub fn has_api_key_filter(_field: &str, value: &str) -> String {
    if value.eq_ignore_ascii_case("true") {
        "player.api_key_hash IS NOT NULL".to_string()
    } else {
        "player.api_key_hash IS NULL".to_string()
    }
}

/// `apiKeyFormat` (`player_repository.go:120`): `^nds_[0-9A-Za-z]{22}$`
/// exactly. The pattern is an anchored prefix plus exactly 22 alphanumeric
/// characters, which is what these checks reproduce byte-for-byte; the crate has
/// no regex dependency, so the shape is spelled out rather than compiled.
#[must_use]
pub fn api_key_format_matches(key: &str) -> bool {
    let Some(body) = key.strip_prefix(API_KEY_PREFIX) else {
        return false;
    };
    body.len() == API_KEY_BODY_LENGTH && body.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// `validateAPIKey` (`player_repository.go:126-131`).
///
/// # Errors
/// [`PlayerError::ApiKeyValidation`] carrying
/// `resources.player.validation.apiKeyFormat`.
pub fn validate_api_key(key: &str) -> Result<(), PlayerError> {
    if !api_key_format_matches(key) {
        return Err(PlayerError::ApiKeyValidation(
            ApiKeyValidationError::format(),
        ));
    }
    Ok(())
}

/// `hashAPIKey` (`player_repository.go:195-198`): SHA-256 over the key's bytes,
/// lowercase hex, unsalted.
///
/// Go's rationale, kept verbatim (`:194`): keys are long random strings rather
/// than user-chosen passwords, so a fast unsalted hash is enough and it keeps
/// lookups indexed — [`PlayerRepo::find_by_api_key`] matches on the hash column
/// directly.
#[must_use]
pub fn hash_api_key(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    // `hex.EncodeToString`: lowercase, two chars per byte.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// `isUniqueViolation` (`player_repository.go:225-227`): Go matches the SQLite
/// message text rather than an error code, so the port does the same —
/// `UNIQUE constraint failed: player.api_key_hash` is what both drivers report.
#[must_use]
pub fn is_unique_violation(err: &rusqlite::Error) -> bool {
    err.to_string().contains("UNIQUE constraint failed")
}

/// `ownerFilter` (`sql_base_repository.go:67-72`): the ownership predicate for
/// tables with a `user_id` column; `None` means no restriction, which Go returns
/// for admins and for the headless invalid user. The predicate is unqualified,
/// so it stays unambiguous on this repository's single-`user_id` join.
#[must_use]
pub fn owner_filter(user: &LoggedUser) -> Option<&str> {
    if !user.is_admin && user.id != INVALID_USER_ID {
        Some(user.id.as_str())
    } else {
        None
    }
}

/// `addRestriction` (`sql_base_repository.go:77-86`): the caller's predicate
/// plus the owner filter, rendered as squirrel's `And` does — parenthesized, and
/// `(1=1)` when both halves are absent (`squirrel@v1.5.4/expr.go:370-372`).
///
/// The clause rides as a raw fragment because squirrel renders a nested `And`
/// (`squirrel@v1.5.4/expr.go:369-388`), for which [`Repository`] has no verb.
fn add_restriction(where_sql: Option<&str>, args: &mut Vec<Value>, user: &LoggedUser) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(clause) = where_sql {
        parts.push(clause.to_string());
    }
    if let Some(owner) = owner_filter(user) {
        parts.push("user_id = ?".to_string());
        args.push(Value::from(owner));
    }
    if parts.is_empty() {
        return "(1=1)".to_string();
    }
    format!("({})", parts.join(" AND "))
}

/// `ownedRow` (`sql_base_repository.go:97-102`).
///
/// `ownerOnly` is `And{Eq{"id": rowID}, Eq{"user_id": loggedUser.ID}}` — always
/// two clauses, admins included, and squirrel sorts `Eq` keys so `id` precedes
/// `user_id` (`squirrel@v1.5.4/expr.go:161` for the sort, `:384-386` for the
/// `And` paren). `ownerOrAdmin` is `addRestriction` over
/// `Eq{"id": rowID}`, so an admin or headless caller gets `(id = ?)`.
fn owned_row(
    row_id: &str,
    access: WriteAccess,
    args: &mut Vec<Value>,
    user: &LoggedUser,
) -> String {
    args.push(Value::from(row_id));
    if access == WriteAccess::OwnerOnly {
        args.push(Value::from(user.id.as_str()));
        return "(id = ? AND user_id = ?)".to_string();
    }
    add_restriction(Some("id = ?"), args, user)
}

/// squirrel `SetMap` rendering (`squirrel@v1.5.4/update.go:230-243`): column keys sorted, each
/// rendered `col = ?`, values bound in that same sorted order.
fn render_set_map(pairs: &[SetPair]) -> (String, Vec<Value>) {
    let mut sorted: Vec<&SetPair> = pairs.iter().collect();
    sorted.sort_by_key(|pair| pair.0);
    let mut args = Vec::with_capacity(sorted.len());
    let set = sorted
        .iter()
        .map(|pair| {
            args.push(pair.1.clone());
            format!("{} = ?", pair.0)
        })
        .collect::<Vec<_>>()
        .join(", ");
    (set, args)
}

/// squirrel `Insert(...).SetMap(values)` rendering (`squirrel@v1.5.4/insert.go:271-287` plus
/// `squirrel@v1.5.4/insert.go:74-94,117-140`): the sorted column list joined with `,` and the
/// placeholder tuple joined with `,`.
fn render_insert_map(table: &str, pairs: &[SetPair]) -> Sql {
    let mut sorted: Vec<&SetPair> = pairs.iter().collect();
    sorted.sort_by_key(|pair| pair.0);
    let columns = sorted
        .iter()
        .map(|pair| pair.0)
        .collect::<Vec<_>>()
        .join(",");
    let mut args = Vec::with_capacity(sorted.len());
    for pair in &sorted {
        args.push(pair.1.clone());
    }
    let placeholders = vec!["?"; sorted.len()].join(",");
    (
        format!("INSERT INTO {table} ({columns}) VALUES ({placeholders})"),
        args,
    )
}

/// squirrel's `getSortedKeys` (`squirrel@v1.5.4/expr.go:404-411`) over the `Eq` map inside an
/// `And`: keys sorted, each rendered `col = ?`, args bound in sorted order, and
/// the conjunction parenthesized (`squirrel@v1.5.4/expr.go:369-388`).
fn and_eq_sql(pairs: &[(&str, Value)]) -> (String, Vec<Value>) {
    let mut sorted: Vec<(&str, &Value)> = pairs
        .iter()
        .map(|(column, value)| (*column, value))
        .collect();
    sorted.sort_by_key(|pair| pair.0);
    let mut args: Vec<Value> = Vec::with_capacity(sorted.len());
    let clauses = sorted
        .iter()
        .map(|(column, value)| {
            args.push((*value).clone());
            format!("{column} = ?")
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    (format!("({clauses})"), args)
}

/// `registerModel`'s rest-facing half (`player_repository.go:25-28`): the
/// `player` table with the `name` contains-filter and the `hasapikey` custom
/// filter, plus the field whitelist.
///
/// The whitelist is `registerModelWhiteList` (`sql_base_repository.go:110`) over
/// `structs.Map(&model.Player{})` (`sql_restful.go:142-162`), which snake-cases
/// every key and adds `model.Annotations`; [`crate::sql_restful::register_model_white_list`]
/// does the same, so the field names are given in the model's CamelCase form as
/// the other repository ports do. The three `structs:"-"` fields (`Username`,
/// `HasAPIKey`, `APIKey`, `model/player.go:11,24-25`) are absent from the map,
/// so `username` is not a filterable field; it reaches ORDER BY only through the
/// sort mapping below.
#[must_use]
pub fn rest_repo() -> RestRepository {
    let has_api_key_mapping: FilterFn = Box::new(|field: &str, value: &str| {
        Some(Filter::Raw {
            sql: has_api_key_filter(field, value),
            raw_args: Vec::new(),
        })
    });
    RestRepository::new(PLAYER_TABLE)
        .with_filter_mappings([
            ("name", contains_filter("player.name")),
            ("hasapikey", has_api_key_mapping),
        ])
        .with_field_white_list([
            "ID",
            "Name",
            "UserAgent",
            "UserId",
            "Client",
            "IP",
            "LastSeen",
            "TranscodingId",
            "MaxBitRate",
            "ReportRealPath",
            "ScrobbleEnabled",
        ])
}

/// `setSortMappings` (`player_repository.go:29-31`): the one registered
/// mapping. Go's TODO on the line (rename `user_name`/`userName` to `username`)
/// is why the key is snake_case here: `sanitizeSort` snake-cases the sort field
/// before the lookup (`sql_restful.go:71-74`).
pub const SORT_MAPPINGS: [(&str, &str); 1] = [("user_name", "username")];

/// `lookupSortMapping` over this repository's map
/// (`sql_base_repository.go:187-197`). Go also probes the camelCase form before
/// the snake_case one; `sanitizeSort` hands this port an already-snake-cased
/// field, so the camelCase probe could only hit the same entry.
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort)
        .map(|(_, value)| *value)
}

/// `sanitizeSort` (`sql_restful.go:69-86`): snake-case the sort field and keep
/// it when EITHER arm knows it — `lookupSortMapping(sort)` (this repository's
/// [`SORT_MAPPINGS`]) OR `isFieldWhiteListed(sort)` — else drop it; fold the
/// order to `asc`/`desc` (empty stays empty, `:80-85`).
///
/// [`crate::sql_restful::RestRepository::sanitize_sort`] covers the whitelist
/// arm only, because the mapping arm is per-repository in Go, so the mapping is
/// resolved here rather than duplicated in the shared layer. Note Go validates
/// but does NOT map (`buildSortOrder` resolves the mapping later,
/// `sql_base_repository.go:199-200`), which is why a mapped field comes back
/// snake-cased, not as its mapping value.
#[must_use]
pub fn sanitize_sort(sort: &str, order: &str) -> (String, String) {
    let (whitelisted, order) = rest_repo().sanitize_sort(sort, order);
    if sort.is_empty() {
        return (String::new(), order);
    }
    let snake = to_snake_case(sort);
    let kept = if sort_mapping(&snake).is_some() || !whitelisted.is_empty() {
        snake
    } else {
        String::new()
    };
    (kept, order)
}

/// `sortMapping` (`sql_base_repository.go:165-185`): resolve each comma-
/// separated part through [`sort_mapping`] (unmapped parts snake_case), so a mix
/// of mapped keys and plain columns keeps the mappings it recognizes. A part that
/// carries its own direction token (`"user_name desc"`) is not a mapping key, so
/// it snake_cases as written — which is what Go does, since the whole part goes
/// through `lookupSortMapping` before `buildSortOrder` splits on whitespace.
#[must_use]
pub fn mapped_sort(sort: &str) -> String {
    sort.split(',')
        .map(|part| {
            let part = part.trim();
            match sort_mapping(part) {
                Some(mapping) => mapping.to_string(),
                None => to_snake_case(part),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `buildSortOrder` (`sql_base_repository.go:199-228`): resolve the sort through
/// [`mapped_sort`], fold the caller's direction, and flip a part that spells its
/// own direction explicitly.
///
/// Go's splitter is paren-aware (`:231-245`); see the module header for why the
/// plain split is sufficient for this repository's mapping.
#[must_use]
pub fn build_sort_order(sort: &str, order: &str) -> String {
    let order = order.trim().to_lowercase();
    let (order, reverse) = if order == "desc" {
        ("desc".to_string(), "asc".to_string())
    } else {
        ("asc".to_string(), "desc".to_string())
    };
    mapped_sort(sort)
        .split(", ")
        .map(|part| {
            let mut fields = part.split_whitespace();
            let field = fields.next().unwrap_or("");
            let direction = match fields.next() {
                None | Some("asc") => order.clone(),
                Some(_) => reverse.clone(),
            };
            format!("{field} {direction}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The shared spine of every read path (`selectPlayer`,
/// `player_repository.go:47-51`): the column list Go appends in two `Columns`
/// calls (extends, not replaces — `squirrel@v1.5.4/select.go:257-263`), the owner JOIN, and the
/// caller's pagination/filters (`newSelect`, `sql_base_repository.go:139-147`:
/// `applyOptions` then `applyFilters`).
///
/// `RESTRICTION_NONE` is the plain `selectPlayer`, which applies NO ownership
/// restriction — that is what lets `Get`, `FindMatch` and `FindByAPIKey` resolve a
/// player across users (`newRestSelect`, `:72-75`, is the only path that adds
/// `addRestriction`).
const RESTRICTION_NONE: Option<&LoggedUser> = None;

/// `selectPlayer` (+ optionally `newRestSelect`'s `addRestriction`, `:72-75`) as a
/// builder, so each caller extends it the way Go's method chain does
/// (`Columns`/`Where`/`OrderBy`/`RemoveColumns`) and the renderer alone decides
/// clause order. `to_sql` binds args in clause order, which is Go's call order.
fn player_select_builder(options: Option<&Options>, restrict: Option<&LoggedUser>) -> Repository {
    let mut sq = Repository::new(PLAYER_TABLE)
        .select(["player.*", HAS_API_KEY_COLUMN, USERNAME_COLUMN])
        .inner_join("user", "player.user_id = user.id");
    if let Some(options) = options {
        sq = crate::sql_search::apply_pagination(sq, &page_options(options));
        if let Some(filters) = &options.filters {
            sq = sq.where_raw(filters.clone(), options.filter_args.clone());
        }
    }
    if let Some(user) = restrict {
        let mut args = Vec::new();
        let clause = add_restriction(None, &mut args, user);
        sq = sq.where_raw(clause, args);
    }
    sq
}

/// The materialized spine (`toSQL()`, `sql_base_repository.go:358-372`).
#[must_use]
fn player_select(options: Option<&Options>, restrict: Option<&LoggedUser>) -> Sql {
    player_select_builder(options, restrict)
        .to_sql()
        .expect("the player select always renders")
}

/// Bridge so pagination reuse goes through [`crate::sql_search::apply_pagination`]
/// rather than a second copy of `applyOptions` (`sql_base_repository.go:149-161`).
fn page_options(options: &Options) -> QueryOptions {
    QueryOptions {
        sort: options.sort.clone(),
        order: options.order.clone(),
        max: options.max,
        offset: options.offset,
        seed: None,
        filters: None,
    }
}

/// `selectPlayer` narrowed by one extra WHERE part. Squirrel AND-joins its
/// accumulated `Where` clauses in call order (`squirrel@v1.5.4/select.go:111-113`), so the extra
/// clause's text goes last and its args bind last.
fn select_with_clause(
    options: Option<&Options>,
    restrict: Option<&LoggedUser>,
    clause: String,
    clause_args: Vec<Value>,
) -> Sql {
    player_select_builder(options, restrict)
        .where_raw(clause, clause_args)
        .to_sql()
        .expect("the player select always renders")
}

/// `Get` (`player_repository.go:54-59`): the UNRESTRICTED `selectPlayer`
/// narrowed to one player id. Go's `Get` deliberately does not call
/// `newRestSelect`, so an id is enough to read any row — the ownership check for
/// the REST surface lives in `rest.Repository`'s own read path, and
/// `player_repository_test.go:71-87` pins that an admin repo reads either user's
/// player with this method.
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    select_with_clause(
        None,
        RESTRICTION_NONE,
        "player.id = ?".to_string(),
        vec![Value::from(id)],
    )
}

/// `Read` (`player_repository.go:106-111`): the ownership-RESTRICTED read of one
/// id. A non-owner's miss surfaces as not-found, which is what
/// `player_repository_test.go:160-167` expects (`model.ErrNotFound` for the
/// non-admin branch).
#[must_use]
pub fn read_sql(id: &str, user: &LoggedUser) -> Sql {
    select_with_clause(
        None,
        Some(user),
        "player.id = ?".to_string(),
        vec![Value::from(id)],
    )
}

/// `newRestSelect` (`player_repository.go:72-75`): the restricted select under
/// the caller's [`Options`].
#[must_use]
pub fn new_rest_select_sql(options: Option<&Options>, user: &LoggedUser) -> Sql {
    player_select(options, Some(user))
}

/// `FindMatch` (`player_repository.go:61-70`): the unrestricted select narrowed
/// to the `And{client, user_agent, user_id}` triple, with squirrel's sorted keys
/// and parenthesized conjunction intact. Values bind, so an injection-shaped
/// client string matches nothing.
#[must_use]
pub fn find_match_sql(user_id: &str, client: &str, user_agent: &str) -> Sql {
    let (clause, args) = and_eq_sql(&[
        ("client", Value::from(client)),
        ("user_agent", Value::from(user_agent)),
        ("user_id", Value::from(user_id)),
    ]);
    select_with_clause(None, RESTRICTION_NONE, clause, args)
}

/// `FindByAPIKey` (`player_repository.go:200-209`): the unrestricted select
/// narrowed to the hash — the plaintext key never reaches the statement.
#[must_use]
pub fn find_by_api_key_sql(key: &str) -> Sql {
    select_with_clause(
        None,
        RESTRICTION_NONE,
        "player.api_key_hash = ?".to_string(),
        vec![Value::from(hash_api_key(key))],
    )
}

/// `CountByClient` (`player_repository.go:77-96`): the aggregate over
/// `newSelect` — NO ownership restriction (`:78` is `newSelect`, not
/// `newRestSelect`), so the client census spans every user — with the
/// `NavidromeUI` → `name` CASE and `GROUP BY client`.
///
/// Go passes no ORDER BY at all, which this renderer pins: squirrel emits
/// `GROUP BY` after WHERE (`squirrel@v1.5.4/select.go:103-148`). The CASE reads the
/// non-aggregated `name`, so SQLite picks one name per client group arbitrarily
/// — Go's query has the same property, and callers must not read the label as
/// stable. `options` carries the caller's filters: `core/metrics/insights.go:305`
/// passes `squirrel.Gt{"last_seen": now-7d}`, and those bound values must ride
/// the statement rather than be dropped.
#[must_use]
pub fn count_by_client_sql(options: Option<&Options>) -> Sql {
    let mut sq = Repository::new(PLAYER_TABLE).select([
        format!("case when client = '{NAVIDROME_UI_CLIENT}' then name else client end as player"),
        "count(*) as count".to_string(),
    ]);
    if let Some(options) = options {
        if let Some(filters) = &options.filters {
            sq = sq.where_raw(filters.clone(), options.filter_args.clone());
        }
    }
    sq.group_by(["client"])
        .to_sql()
        .expect("the client census always renders")
}

/// `CountAll` (`player_repository.go:98-100`) through `count`
/// (`sql_base_repository.go:565-576`): the restricted select's columns replaced
/// by `count(distinct player.id) as count`, LIMIT/OFFSET dropped, `ORDER BY
/// player.id` appended (Go's `OrderBy` extends and `newRestSelect` carries no
/// order, so it is exactly this one expression), and the caller's filters
/// appended AFTER the restriction — `count` applies them through `applyFilters`
/// (`:572`) after the builder was handed over, unlike `ReadAll` (`:114`) where
/// they arrive before `newRestSelect` adds the restriction.
#[must_use]
pub fn count_all_sql(options: Option<&Options>, user: &LoggedUser) -> Sql {
    // count() (`sql_base_repository.go:565-576`): RemoveColumns + the count
    // column, RemoveOffset/RemoveLimit, OrderBy(table.id), From(table) again,
    // then applyFilters. [`Repository::count`] is that same rewrite, so the
    // restricted select's JOIN/WHERE survive and the caller's filters land AFTER
    // the restriction (they arrive here, not in newSelect as in ReadAll).
    // user.id is the join's unique key, so the join cannot inflate the count.
    let mut sq = player_select_builder(None, Some(user)).count(PLAYER_TABLE);
    sq = sq.order_by([format!("{PLAYER_TABLE}.id")]);
    if let Some(options) = options {
        if let Some(filters) = &options.filters {
            sq = sq.where_raw(filters.clone(), options.filter_args.clone());
        }
    }
    sq.to_sql().expect("the count select always renders")
}

/// `ReadAll` (`player_repository.go:113-118`): the restricted select under the
/// caller's options — pagination and filters from `newSelect`, the restriction
/// from `newRestSelect`, then the resolved sort from `applyOptions`.
#[must_use]
pub fn read_all_sql(options: Option<&Options>, user: &LoggedUser) -> Sql {
    let mut sq = player_select_builder(options, Some(user));
    if let Some(options) = options {
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
    }
    sq.to_sql().expect("the player select always renders")
}

/// The player columns `toSQLArgs` yields (`helpers.go:20-43` over
/// `model/player.go:13-23`): the `structs:` tags only, so `username`,
/// `has_api_key` and `api_key` are NOT columns and the plaintext key can never
/// be written.
///
/// `hash` is `Some` only for `Save`'s INSERT; `toSQLArgs` has no `api_key_hash`
/// field (`values["api_key_hash"]` is added by hand at `:154`), so the UPDATE
/// paths never write it and [`put_sql`] never writes it either
/// (`player_repository_test.go`'s admin-edit-leaves-the-key case). `user_id`
/// stays here for the INSERT legs; [`update_owned_sql`] drops it because
/// ownership is immutable on update (`sql_base_repository.go:512`).
fn row_write_pairs(record: &PlayerRecord, id: &str, hash: Option<&str>) -> Vec<SetPair> {
    let mut pairs: Vec<SetPair> = vec![
        ("id", Value::from(id)),
        ("name", Value::from(record.name.as_str())),
        ("user_agent", Value::from(record.user_agent.as_str())),
        ("user_id", Value::from(record.user_id.as_str())),
        ("client", Value::from(record.client.as_str())),
        ("ip", Value::from(record.ip.as_str())),
        (
            "last_seen",
            record
                .last_seen
                .as_ref()
                .map(|stamp| Value::from(stamp.as_str()))
                .unwrap_or(Value::Null),
        ),
        (
            "transcoding_id",
            Value::from(record.transcoding_id.as_str()),
        ),
        ("max_bit_rate", Value::Integer(record.max_bit_rate)),
        ("report_real_path", Value::Bool(record.report_real_path)),
        ("scrobble_enabled", Value::Bool(record.scrobble_enabled)),
    ];
    if let Some(hash) = hash {
        pairs.push(("api_key_hash", Value::from(hash)));
    }
    pairs
}

/// `Save`'s INSERT (`player_repository.go:148-155`): the `toSQLArgs` payload
/// with `id` replaced by the generated id and `api_key_hash` added, in ONE
/// statement. Go's comment on why they share a statement (`:152`): `Save` only
/// creates, so the key hash goes in the same INSERT and the unique index settles
/// races. `record` must already carry the resolved owner (Go stamps `t.UserId`
/// in place at `:136`).
#[must_use]
pub fn save_insert_sql(record: &PlayerRecord, id: &str, hash: &str) -> Sql {
    render_insert_map(PLAYER_TABLE, &row_write_pairs(record, id, Some(hash)))
}

/// [`put_sql`]'s output: the two legs of the base `put` upsert
/// (`sql_base_repository.go:618-642`).
#[derive(Debug, Clone, PartialEq)]
pub struct PutSql {
    /// The id the row lands under: the record's, or a fresh
    /// [`nd_id::new_random`] when it came in empty (`:636-639`).
    pub id: String,
    /// The UPDATE leg; Go skips it entirely when the incoming id is empty.
    pub update: Option<Sql>,
    /// The INSERT leg.
    pub insert: Sql,
}

/// `Put` (`player_repository.go:42-45`) through `put`: UPDATE against the row id
/// with the full column set (`filterUpdateValues` forces `id` into the SET list,
/// `:611-614`), then the INSERT when the UPDATE matched no row or the id came in
/// empty.
///
/// Unlike [`update_owned_sql`], `put` neither drops `user_id` nor applies an
/// ownership predicate — Go's `Put` is the service write path
/// (`core/players.go:83`) and the schema's foreign key is what guards ownership.
#[must_use]
pub fn put_sql(record: &PlayerRecord) -> PutSql {
    let pairs = row_write_pairs(record, &record.id, None);
    let id = if record.id.is_empty() {
        nd_id::new_random()
    } else {
        record.id.clone()
    };
    let update = if record.id.is_empty() {
        None
    } else {
        let (set, mut args) = render_set_map(&pairs);
        args.push(Value::from(record.id.as_str()));
        Some((
            format!("UPDATE {PLAYER_TABLE} SET {set} WHERE id = ?"),
            args,
        ))
    };
    // The INSERT carries the id Go writes into `values` only when the incoming
    // record had none (`:636-639`); a predefined id rides inside the payload.
    let insert_pairs = row_write_pairs(record, &id, None);
    PutSql {
        id,
        update,
        insert: render_insert_map(PLAYER_TABLE, &insert_pairs),
    }
}

/// `Update`'s column leg (`player_repository.go:169,176`) through `updateOwned`
/// (`sql_base_repository.go:506-515`): `selectUpdateColumns` keeps the requested
/// columns (all of them when none were requested, `:596-604`),
/// `filterUpdateValues` forces `id` afterwards (`:612-616`, so `id` is written
/// even when the client did not ask for it), and `user_id` is deleted (`:512`) —
/// ownership is immutable on update, so no caller (admin included) can reassign
/// a row.
///
/// `cols_to_update` arrives as the client's camelCase names; Go snake-cases them
/// (`selectUpdateColumns`, `:599-601`) with the same helper this port reuses
/// ([`crate::sql_restful::to_snake_case`]).
#[must_use]
pub fn update_owned_sql(
    row_id: &str,
    record: &PlayerRecord,
    cols_to_update: &[&str],
    access: WriteAccess,
    user: &LoggedUser,
) -> Sql {
    let wanted: Vec<String> = cols_to_update.iter().map(|c| to_snake_case(c)).collect();
    let all = row_write_pairs(record, row_id, None);
    let mut pairs: Vec<SetPair> = all
        .into_iter()
        .filter(|(column, _)| {
            *column == "id"
                || ((*column != "user_id" && *column != "created_at")
                    && (wanted.is_empty() || wanted.iter().any(|c| c.as_str() == *column)))
        })
        .collect();
    pairs.sort_by_key(|pair| pair.0);
    let (set, mut args) = render_set_map(&pairs);
    let where_sql = owned_row(row_id, access, &mut args, user);
    (
        format!("UPDATE {PLAYER_TABLE} SET {set} WHERE {where_sql}"),
        args,
    )
}

/// `SetAPIKey` (`player_repository.go:211-223`): one SET of `api_key_hash`
/// through `updateOwnedRow` (`sql_base_repository.go:517-519`), `NULL` for a
/// revoke, restricted by `access`.
///
/// The access level is this method's whole security property: a revoke is
/// `ownerOrAdmin` (`:213`) but setting a key is `ownerOnly` (`:218`), so even an
/// admin cannot mint a login for someone else while an admin can still retire a
/// key. Unlike [`update_owned_sql`] this SET carries no `id` column — Go builds
/// the `map[string]any` by hand, so only the hash is written. The revoke arm
/// binds `Value::Null`, the way squirrel renders `map[string]any{…: nil}`
/// (`squirrel@v1.5.4/expr.go:172-183`).
#[must_use]
pub fn set_api_key_sql(
    player_id: &str,
    hash: Option<&str>,
    access: WriteAccess,
    user: &LoggedUser,
) -> Sql {
    let value = hash.map(Value::from).unwrap_or(Value::Null);
    let mut args = vec![value];
    let where_sql = owned_row(player_id, access, &mut args, user);
    (
        format!("UPDATE {PLAYER_TABLE} SET api_key_hash = ? WHERE {where_sql}"),
        args,
    )
}

/// `Delete` (`player_repository.go:190-192`) through `deleteOwnedAll`
/// (`sql_base_repository.go:542-549` → `deleteOwned`, `:526-528`): one
/// ownership-restricted (`ownerOrAdmin`) DELETE per id, in order.
#[must_use]
pub fn delete_sql(ids: &[&str], user: &LoggedUser) -> Vec<Sql> {
    ids.iter()
        .map(|id| {
            let mut args = Vec::new();
            let where_sql = owned_row(id, WriteAccess::OwnerOrAdmin, &mut args, user);
            (
                format!("DELETE FROM {PLAYER_TABLE} WHERE {where_sql}"),
                args,
            )
        })
        .collect()
}

/// `exists` (`sql_base_repository.go:488-493`), the lookup
/// `classifyOwnedWriteMiss` (`:554-563`) runs on the failure path only: does the
/// id exist at all, ignoring ownership? `WHERE (id = ?)` is how squirrel renders
/// the bare `Eq{"id": id}` inside `Where`.
#[must_use]
pub fn exists_sql(id: &str) -> Sql {
    (
        format!("SELECT count(*) as exist FROM {PLAYER_TABLE} WHERE (id = ?)"),
        vec![Value::from(id)],
    )
}

/// `exists`' boolean (`sql_base_repository.go:492`): `res.Exist > 0`.
///
/// # Errors
/// [`PlayerError::Sqlite`] when the probe statement fails.
fn exists(conn: &Connection, id: &str) -> Result<bool, PlayerError> {
    Ok(
        execution::query_optional(conn, &exists_sql(id), |row| row.get::<_, i64>(0))?
            .unwrap_or_default()
            > 0,
    )
}

/// `classifyOwnedWriteMiss` (`sql_base_repository.go:554-563`): explain why an
/// ownership-filtered write matched no row — permission-denied when the row
/// exists but belongs to someone else, not-found otherwise. Handing back the
/// error value (rather than a `Result<(), _>`) mirrors Go's `error`-only return;
/// callers wrap it in `Err`.
///
/// # Errors
/// [`PlayerError::Sqlite`] when the existence probe itself fails.
fn classify_owned_write_miss(conn: &Connection, id: &str) -> Result<PlayerError, PlayerError> {
    if exists(conn, id)? {
        Ok(PlayerError::PermissionDenied)
    } else {
        Ok(PlayerError::NotFound)
    }
}

/// `dbx` row mapping for the player select's columns. `api_key_hash` arrives via
/// `player.*` but has no field in the record, so Go scans it into a throwaway
/// `sql.NullString` (`dbx@v1.12.0/rows.go:80-86`) and this mapper ignores it too.
fn row_to_player(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlayerRecord> {
    Ok(PlayerRecord {
        username: row.get("username")?,
        id: row.get("id")?,
        name: row.get("name")?,
        user_agent: row.get("user_agent")?,
        user_id: row.get("user_id")?,
        client: row.get("client")?,
        ip: row.get("ip")?,
        last_seen: row.get("last_seen")?,
        transcoding_id: row.get("transcoding_id")?,
        max_bit_rate: row.get("max_bit_rate")?,
        report_real_path: row.get("report_real_path")?,
        scrobble_enabled: row.get("scrobble_enabled")?,
        has_api_key: row.get::<_, i64>("has_api_key")? != 0,
        api_key: None,
    })
}

/// `playerRepository` (`player_repository.go:18-20`) bound to one pooled
/// connection. Go binds a `dbx.Builder` and reaches the DB through the ambient
/// ctx; the crate has no ambient context, so the caller threads a lease from
/// [`SQLStore::with_connection_sync`] (or a transaction, which derefs to a
/// `&Connection`) and every statement runs through [`crate::execution`].
#[derive(Debug)]
pub struct PlayerRepo<'a> {
    conn: &'a Connection,
}

impl<'a> PlayerRepo<'a> {
    /// `NewPlayerRepository` (`player_repository.go:22-33`): bind to `conn`.
    #[must_use]
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`player_repository.go:54-59`): the joined row for one id, with no
    /// ownership restriction (see [`get_sql`]).
    ///
    /// Go returns a non-nil `*Player` alongside the error; the port returns
    /// `Result`, the same information without the pointer.
    ///
    /// # Errors
    /// [`PlayerError::NotFound`] when no row matches (`player_repository_test.go:84-86`);
    /// [`PlayerError::Sqlite`] on a statement or mapping failure.
    pub fn get(&self, id: &str) -> Result<PlayerRecord, PlayerError> {
        self.read_one(&get_sql(id))
    }

    /// `Read` (`player_repository.go:106-111`): the ownership-restricted read of
    /// one id; a foreign row reads as [`PlayerError::NotFound`]
    /// (`player_repository_test.go:160-167`).
    ///
    /// # Errors
    /// Same as [`Self::get`].
    pub fn read(&self, id: &str, user: &LoggedUser) -> Result<PlayerRecord, PlayerError> {
        self.read_one(&read_sql(id, user))
    }

    /// `FindMatch` (`player_repository.go:61-70`): the `(user_id, client,
    /// user_agent)` triple, unrestricted.
    ///
    /// # Errors
    /// [`PlayerError::NotFound`] when the triple matches nothing (Go's
    /// `model.ErrNotFound`, `player_repository_test.go:66-69`);
    /// [`PlayerError::Sqlite`] on a statement or mapping failure.
    pub fn find_match(
        &self,
        user_id: &str,
        client: &str,
        user_agent: &str,
    ) -> Result<PlayerRecord, PlayerError> {
        self.read_one(&find_match_sql(user_id, client, user_agent))
    }

    /// `FindByAPIKey` (`player_repository.go:200-209`): look a player up by the
    /// hash of a presented key. The plaintext never reaches the statement.
    ///
    /// # Errors
    /// [`PlayerError::NotFound`] for an unknown or empty key
    /// (`player_repository_test.go:378-383`); [`PlayerError::Sqlite`] on a
    /// statement or mapping failure.
    pub fn find_by_api_key(&self, key: &str) -> Result<PlayerRecord, PlayerError> {
        self.read_one(&find_by_api_key_sql(key))
    }

    /// `ReadAll` (`player_repository.go:113-118`): the restricted page under the
    /// caller's options. Go's `queryAll` yields an empty slice for no rows, not
    /// `ErrNotFound` (`sql_base_repository.go:441-459`), so the empty `Vec` is
    /// the faithful port.
    ///
    /// # Errors
    /// [`PlayerError::Sqlite`] when the statement or a row mapping fails.
    pub fn read_all(
        &self,
        options: Option<&Options>,
        user: &LoggedUser,
    ) -> Result<Vec<PlayerRecord>, PlayerError> {
        let statement = read_all_sql(options, user);
        Ok(execution::query_all(self.conn, &statement, row_to_player)?)
    }

    /// `CountAll` (`player_repository.go:98-100`) / `Count` (`:102-104`): the
    /// restricted row count under the caller's options.
    ///
    /// # Errors
    /// [`PlayerError::Sqlite`] when the statement fails.
    pub fn count_all(
        &self,
        options: Option<&Options>,
        user: &LoggedUser,
    ) -> Result<i64, PlayerError> {
        let statement = count_all_sql(options, user);
        Ok(
            execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `CountByClient` (`player_repository.go:77-96`): the client census, keyed
    /// by the CASE-substituted player label, unrestricted.
    ///
    /// # Errors
    /// [`PlayerError::Sqlite`] when the statement or a row mapping fails.
    pub fn count_by_client(
        &self,
        options: Option<&Options>,
    ) -> Result<HashMap<String, i64>, PlayerError> {
        let rows = execution::query_all(self.conn, &count_by_client_sql(options), |row| {
            Ok((row.get::<_, String>("player")?, row.get::<_, i64>("count")?))
        })?;
        Ok(rows.into_iter().collect())
    }

    /// `Put` (`player_repository.go:42-45`): UPDATE, then INSERT when it matched
    /// no row or the id came in empty. Returns the id the row landed under, like
    /// Go's `put` (`sql_base_repository.go:618-642`).
    ///
    /// # Errors
    /// [`PlayerError::Sqlite`] when either statement fails (an unknown owner is
    /// a foreign-key failure, exactly as in Go).
    pub fn put(&self, record: &PlayerRecord) -> Result<String, PlayerError> {
        let statements = put_sql(record);
        if let Some(update) = &statements.update {
            if execution::execute(self.conn, update)? > 0 {
                return Ok(statements.id);
            }
        }
        execution::execute(self.conn, &statements.insert)?;
        Ok(statements.id)
    }

    /// `Save` (`player_repository.go:133-163`): create a keyed, hand-made player
    /// owned by the logged user, and return the new id.
    ///
    /// `logged_id` is Go's `loggedUser(ctx).ID`, passed explicitly instead of
    /// read from a context: an empty `record.user_id` takes it (`:135-137`), and
    /// any other value is permission-denied (`:138-140`). The headless id is
    /// `INVALID_USER_ID`, and Go's `u.ID != invalidUserId` guard leaves it out of
    /// the stamp, so a headless save is denied rather than stamped `-1`.
    ///
    /// Hand-made players are only reachable through a key, so one is required
    /// (`:141-144`); the id is generated and the hash written in the SAME INSERT
    /// so the unique index settles races (`:152-155`), and a collision surfaces
    /// as the unique validation error rather than a raw driver failure
    /// (`:156-158`).
    ///
    /// Go mutates `t.UserId` in place so its caller sees the stamped owner; the
    /// port takes `&PlayerRecord`, so a caller that saved with an empty owner
    /// re-reads the returned id. A spoofed owner is rejected before the INSERT,
    /// so a Save can never overwrite an existing row (`:138-140`,
    /// `player_repository_test.go:548-566`).
    ///
    /// # Errors
    /// [`PlayerError::PermissionDenied`] when the owner is not the caller;
    /// [`PlayerError::ApiKeyValidation`] for a missing/empty key
    /// (`ra.validation.required`), a malformed key
    /// (`resources.player.validation.apiKeyFormat`), or a key already in use
    /// (`ra.validation.unique`); [`PlayerError::Sqlite`] for any other failure.
    pub fn save(&self, record: &PlayerRecord, logged_id: &str) -> Result<String, PlayerError> {
        // Arrange: the owner resolution of `:135-140`.
        let user_id = if record.user_id.is_empty() && logged_id != INVALID_USER_ID {
            logged_id.to_string()
        } else {
            record.user_id.clone()
        };
        if user_id != logged_id {
            return Err(PlayerError::PermissionDenied);
        }
        // Assert: a key is required and must have the exact shape (`:141-147`).
        let Some(key) = record.api_key.as_deref() else {
            return Err(PlayerError::ApiKeyValidation(
                ApiKeyValidationError::required(),
            ));
        };
        if key.is_empty() {
            return Err(PlayerError::ApiKeyValidation(
                ApiKeyValidationError::required(),
            ));
        }
        validate_api_key(key)?;
        // Act: the minted id, the stamped owner and the hash in the one INSERT
        // (`:136,152-155`).
        let id = nd_id::new_random();
        let stamped = PlayerRecord {
            user_id,
            ..record.clone()
        };
        let statement = save_insert_sql(&stamped, &id, &hash_api_key(key));
        match execution::execute(self.conn, &statement) {
            Ok(_rows) => Ok(id),
            Err(err) if is_unique_violation(&err) => Err(PlayerError::ApiKeyValidation(
                ApiKeyValidationError::unique(),
            )),
            Err(err) => Err(PlayerError::Sqlite(err)),
        }
    }

    /// `SetAPIKey` (`player_repository.go:211-223`): store the key's hash, or
    /// revoke it when `key` is empty.
    ///
    /// Setting is owner-only, even for admins (`:218`); revoking is
    /// owner-or-admin (`:213`). The distinction is deliberate — nobody can mint a
    /// login for someone else, while an admin can still retire a key.
    ///
    /// # Errors
    /// [`PlayerError::ApiKeyValidation`] for a malformed key or a hash already
    /// taken by another player; [`PlayerError::PermissionDenied`] when the row
    /// exists but the caller may not write it under the arm's access level;
    /// [`PlayerError::NotFound`] when the id matches no row;
    /// [`PlayerError::Sqlite`] for any other failure.
    pub fn set_api_key(
        &self,
        player_id: &str,
        key: &str,
        user: &LoggedUser,
    ) -> Result<(), PlayerError> {
        if key.is_empty() {
            return self.run_row_write(
                player_id,
                &set_api_key_sql(player_id, None, WriteAccess::OwnerOrAdmin, user),
            );
        }
        validate_api_key(key)?;
        // `updateOwnedRow` + `isUniqueViolation` (`:217-222`): Go classifies
        // only the unique violation specially, and passes a zero-row count to
        // `runRowWrite`'s classifier like any other write.
        let statement = set_api_key_sql(
            player_id,
            Some(&hash_api_key(key)),
            WriteAccess::OwnerOnly,
            user,
        );
        match execution::execute(self.conn, &statement) {
            Ok(0) => Err(classify_owned_write_miss(self.conn, player_id)?),
            Ok(_) => Ok(()),
            Err(err) if is_unique_violation(&err) => Err(PlayerError::ApiKeyValidation(
                ApiKeyValidationError::unique(),
            )),
            Err(err) => Err(PlayerError::Sqlite(err)),
        }
    }

    /// `updateOwned` (`sql_base_repository.go:506-515`): the ownership-restricted
    /// UPDATE of the record's columns. `user_id` is never written, so ownership
    /// cannot be reassigned by any caller
    /// (`player_repository_test.go:578-604`).
    ///
    /// # Errors
    /// [`PlayerError::PermissionDenied`] / [`PlayerError::NotFound`] per
    /// `classifyOwnedWriteMiss`; [`PlayerError::Sqlite`] otherwise.
    pub fn update_owned(
        &self,
        id: &str,
        record: &PlayerRecord,
        cols_to_update: &[&str],
        user: &LoggedUser,
    ) -> Result<(), PlayerError> {
        self.run_row_write(
            id,
            &update_owned_sql(id, record, cols_to_update, WriteAccess::OwnerOrAdmin, user),
        )
    }

    /// `Update`'s two-write arm (`player_repository.go:171-177`): the key write
    /// then the column write. Run it on a repository bound to a transaction so
    /// both commit together or neither does — [`update`] does that dispatch.
    ///
    /// # Errors
    /// Whatever either leg reports.
    pub fn update_with_key(
        &self,
        id: &str,
        record: &PlayerRecord,
        cols_to_update: &[&str],
        user: &LoggedUser,
    ) -> Result<(), PlayerError> {
        let key = record.api_key.as_deref().unwrap_or_default();
        self.set_api_key(id, key, user)?;
        self.update_owned(id, record, cols_to_update, user)
    }

    /// `Delete` (`player_repository.go:190-192`) through `deleteOwnedAll`: one id
    /// at a time, stopping at the first failure (`:543-547`), so a denied id
    /// leaves every later one untouched.
    ///
    /// # Errors
    /// [`PlayerError::PermissionDenied`] when the row belongs to someone else
    /// (`player_repository_test.go:130-143`), [`PlayerError::NotFound`] when it
    /// does not exist (`:145-148`); [`PlayerError::Sqlite`] otherwise.
    pub fn delete(&self, ids: &[&str], user: &LoggedUser) -> Result<(), PlayerError> {
        for (id, statement) in ids.iter().zip(delete_sql(ids, user)) {
            if execution::execute(self.conn, &statement)? == 0 {
                return Err(classify_owned_write_miss(self.conn, id)?);
            }
        }
        Ok(())
    }

    /// `runRowWrite` (`sql_base_repository.go:531-540`): execute the
    /// ownership-filtered write; a driver error propagates untouched and only a
    /// zero-row count consults `classifyOwnedWriteMiss`, which runs after no
    /// write happened, so there is no TOCTOU.
    ///
    /// # Errors
    /// The statement's own failure, or the classification of a miss.
    fn run_row_write(&self, row_id: &str, statement: &Sql) -> Result<(), PlayerError> {
        match execution::execute(self.conn, statement)? {
            0 => Err(classify_owned_write_miss(self.conn, row_id)?),
            _ => Ok(()),
        }
    }

    /// `queryOne` over the player select (`sql_base_repository.go:375-388`): one
    /// row, with `sql.ErrNoRows` mapped to [`PlayerError::NotFound`].
    fn read_one(&self, statement: &Sql) -> Result<PlayerRecord, PlayerError> {
        execution::query_optional(self.conn, statement, row_to_player)?.ok_or(PlayerError::NotFound)
    }
}

/// `Update` (`player_repository.go:165-178`) plus `inTx` (`:180-188`) through the
/// store's transaction verb:
///
/// - `record.api_key == None` → the single ownership-restricted UPDATE, no
///   transaction (`:168-170`).
/// - otherwise → [`PlayerRepo::update_with_key`] inside
///   [`SQLStore::with_tx_sync`], so the key write and the column write commit
///   together or roll back together (`:171-177`). Go's `inTx` wraps a `*dbx.DB`
///   in `conn.Transactional` and reuses the receiver when it is already a tx
///   (`:180-188`); rusqlite's default `DropBehavior::Rollback` plus
///   [`crate::store::with_tx`]'s explicit finish make the rollback certain.
///
/// # Errors
/// Whatever the repository legs report; a commit failure arrives as
/// [`PlayerError::Store`].
pub fn update(
    store: &SQLStore,
    id: &str,
    record: &PlayerRecord,
    cols_to_update: &[&str],
    user: &LoggedUser,
) -> Result<(), PlayerError> {
    let record = record.clone();
    let cols = cols_to_update.to_vec();
    let user = user.clone();
    let id = id.to_string();
    if record.api_key.is_none() {
        return store.with_connection_sync(move |conn| {
            Ok(PlayerRepo::new(conn).update_owned(&id, &record, &cols, &user))
        })?;
    }
    store.with_tx_sync(move |tx| {
        Ok(PlayerRepo::new(tx).update_with_key(&id, &record, &cols, &user))
    })?
}

/// `parseRestOptions` for this repository (`sql_restful.go:55-67`), spelled out
/// for callers that build [`Options`] directly from a client request.
#[must_use]
pub fn parse_player_rest_options(options: Option<&RestQueryOptions>) -> Options {
    Options::from_rest(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const PLAYER_ID: &str = "player-1";
    const OWNER: &str = "user-1";
    const KEY: &str = "nds_0123456789abcdefghijkl";

    fn admin() -> LoggedUser {
        LoggedUser {
            id: OWNER.to_string(),
            is_admin: true,
        }
    }

    fn regular(id: &str) -> LoggedUser {
        LoggedUser {
            id: id.to_string(),
            is_admin: false,
        }
    }

    fn record() -> PlayerRecord {
        PlayerRecord {
            username: "jdoe".to_string(),
            id: PLAYER_ID.to_string(),
            name: "NavidromeUI [Firefox/Linux]".to_string(),
            user_agent: "Firefox/Linux".to_string(),
            user_id: OWNER.to_string(),
            client: "NavidromeUI".to_string(),
            ip: "127.0.0.1".to_string(),
            last_seen: Some("2024-01-02 03:04:05".to_string()),
            transcoding_id: String::new(),
            max_bit_rate: 128,
            report_real_path: true,
            scrobble_enabled: false,
            has_api_key: false,
            api_key: Some(KEY.to_string()),
        }
    }

    fn options(pairs: &[(&str, &str)]) -> RestQueryOptions {
        RestQueryOptions {
            filters: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<String, String>>(),
            ..RestQueryOptions::default()
        }
    }

    #[test]
    fn select_player_pins_columns_join_and_restriction() {
        // Arrange/Act: selectPlayer (:47-51) and newRestSelect's
        // addRestriction (:72-75).
        let (admin_sql, admin_args) = new_rest_select_sql(None, &admin());
        let (user_sql, user_args) = new_rest_select_sql(None, &regular("other"));

        // Assert: Go's column order (two Columns calls extend), the owner JOIN,
        // and squirrel's empty-And `(1=1)` for admin/headless.
        assert_eq!(
            admin_sql,
            "SELECT player.*, player.api_key_hash is not null as has_api_key, \
             user.user_name username FROM player INNER JOIN user ON player.user_id = user.id \
             WHERE (1=1)"
        );
        assert!(admin_args.is_empty());

        // Assert: a regular user's ownerFilter rides inside the And, unqualified
        // (sql_base_repository.go:67-72) and bound.
        assert!(user_sql.ends_with("WHERE (user_id = ?)"));
        assert_eq!(user_args, vec![Value::from("other")]);
    }

    #[test]
    fn get_is_unrestricted_and_read_is_restricted() {
        // Arrange/Act: Get (:54-59) selects on selectPlayer — no restriction —
        // while Read (:106-111) goes through newRestSelect.
        let (get, get_args) = get_sql(PLAYER_ID);
        let (read, read_args) = read_sql(PLAYER_ID, &regular(OWNER));
        let (admin_read, admin_read_args) = read_sql(PLAYER_ID, &admin());
        // Assert:
        assert_eq!(
            get,
            "SELECT player.*, player.api_key_hash is not null as has_api_key, \
             user.user_name username FROM player INNER JOIN user ON player.user_id = user.id \
             WHERE player.id = ?"
        );
        assert_eq!(get_args, vec![Value::from(PLAYER_ID)]);
        assert!(read.ends_with("WHERE (user_id = ?) AND player.id = ?"));
        assert_eq!(read_args, vec![Value::from(OWNER), Value::from(PLAYER_ID)]);
        assert!(admin_read.ends_with("WHERE (1=1) AND player.id = ?"));
        assert_eq!(admin_read_args, vec![Value::from(PLAYER_ID)]);
    }

    #[test]
    fn find_match_binds_its_triple_unrestricted() {
        // Arrange/Act: FindMatch (:61-70); squirrel sorts the Eq keys inside the
        // And (squirrel@v1.5.4/expr.go:161 for the sorted Eq keys,
        // :384-386 for the parenthesized conjunction).
        let (sql, args) = find_match_sql(OWNER, "NavidromeUI", "Firefox/Linux");
        // Assert: no restriction clause — the API-key path resolves across users.
        assert!(sql.starts_with(
            "SELECT player.*, player.api_key_hash is not null as has_api_key, \
             user.user_name username FROM player INNER JOIN user ON player.user_id = user.id \
             WHERE (client = ? AND user_agent = ? AND user_id = ?)"
        ));
        assert!(!sql.contains("(1=1)"));
        assert_eq!(
            args,
            vec![
                Value::from("NavidromeUI"),
                Value::from("Firefox/Linux"),
                Value::from(OWNER),
            ]
        );
    }

    #[test]
    fn find_by_api_key_binds_the_hash_never_the_key() {
        // Arrange/Act: FindByAPIKey (:200-209).
        let (sql, args) = find_by_api_key_sql(KEY);
        // Assert:
        assert!(sql.ends_with("WHERE player.api_key_hash = ?"));
        assert_eq!(args, vec![Value::from(hash_api_key(KEY))]);
        assert!(!sql.contains(KEY));
    }

    #[test]
    fn count_all_keeps_the_restriction_and_appends_the_id_order() {
        // Arrange/Act: CountAll (:98-100) via count
        // (sql_base_repository.go:565-575).
        let (sql, args) = count_all_sql(None, &regular(OWNER));
        // Assert: columns replaced, LIMIT/OFFSET dropped, ORDER BY player.id
        // appended, ownership kept.
        assert_eq!(
            sql,
            "SELECT count(distinct player.id) as count FROM player INNER JOIN user \
             ON player.user_id = user.id WHERE (user_id = ?) ORDER BY player.id"
        );
        assert_eq!(args, vec![Value::from(OWNER)]);
    }

    #[test]
    fn count_all_appends_its_filters_after_the_restriction() {
        // Arrange: CountAll hands its options to count() (:99), which applies
        // them through applyFilters AFTER the builder was passed in (:572) — the
        // opposite position from ReadAll (:114).
        let parsed = parse_player_rest_options(Some(&options(&[("hasapikey", "true")])));
        // Act:
        let (sql, args) = count_all_sql(Some(&parsed), &admin());
        // Assert:
        assert!(sql.ends_with("WHERE (1=1) AND player.api_key_hash IS NOT NULL ORDER BY player.id"));
        assert!(args.is_empty(), "the IS NOT NULL clause binds nothing");
    }

    #[test]
    fn count_all_binds_its_filter_values() {
        // Arrange: the insights census shape (`last_seen > ?`), where the value
        // lives inside QueryOptions.Filters.
        let parsed = Options {
            filters: Some("last_seen > ?".to_string()),
            filter_args: vec![Value::from("2026-01-01 00:00:00")],
            ..Options::default()
        };
        // Act:
        let (sql, args) = count_all_sql(Some(&parsed), &admin());
        // Assert: the value rides the statement instead of being dropped.
        assert!(sql.ends_with("AND last_seen > ? ORDER BY player.id"));
        assert_eq!(args, vec![Value::from("2026-01-01 00:00:00")]);
    }

    #[test]
    fn read_all_orders_by_the_resolved_sort() {
        // Arrange: ReadAll (:113-118) with a mapped sort and a page bound.
        let parsed = Options {
            sort: "user_name".to_string(),
            order: "desc".to_string(),
            max: 10,
            offset: 5,
            ..Options::default()
        };
        // Act:
        let (sql, args) = read_all_sql(Some(&parsed), &admin());
        // Assert: the mapping resolves to the joined username alias, and the
        // page bounds render as decimal literals contributing no args — that is
        // squirrel's own behaviour: `Limit`/`Offset` store `fmt.Sprintf("%d")`
        // (select.go:376-388) and `ToSql` writes them into the SQL without
        // adding args (select.go:140-150).
        assert!(sql.contains("ORDER BY username desc"));
        assert!(sql.contains("LIMIT 10"));
        assert!(sql.contains("OFFSET 5"));
        assert_eq!(args.len(), 0, "pagination contributes no args");
    }

    #[test]
    fn count_by_client_pins_the_case_group_and_no_order() {
        // Arrange/Act: CountByClient (:77-96) with no options.
        let (sql, args) = count_by_client_sql(None);
        // Assert: no WHERE, no ORDER BY; GROUP BY sits after the columns.
        assert_eq!(
            sql,
            "SELECT case when client = 'NavidromeUI' then name else client end as player, \
             count(*) as count FROM player GROUP BY client"
        );
        assert!(args.is_empty());
        assert!(!sql.contains("ORDER BY"));
        assert!(!sql.contains("WHERE"));
    }

    #[test]
    fn count_by_client_carries_the_caller_filters_and_their_values() {
        // Arrange: core/metrics/insights.go:305-307 passes
        // squirrel.Gt{"last_seen": now-7d} — the value must bind.
        let parsed = Options {
            filters: Some("last_seen > ?".to_string()),
            filter_args: vec![Value::from("2026-01-01 00:00:00")],
            ..Options::default()
        };
        // Act:
        let (sql, args) = count_by_client_sql(Some(&parsed));
        // Assert: WHERE sits between FROM and GROUP BY, as squirrel renders it.
        assert_eq!(
            sql,
            "SELECT case when client = 'NavidromeUI' then name else client end as player, \
             count(*) as count FROM player WHERE last_seen > ? GROUP BY client"
        );
        assert_eq!(args, vec![Value::from("2026-01-01 00:00:00")]);
    }

    #[test]
    fn has_api_key_filter_folds_the_value_case() {
        // Arrange/Act: hasAPIKeyFilter (:35-40).
        // Assert: EqualFold semantics — every case of "true" is NOT NULL.
        for value in ["true", "TRUE", "True", "tRuE"] {
            assert_eq!(
                has_api_key_filter("hasapikey", value),
                "player.api_key_hash IS NOT NULL"
            );
        }
        // Assert: everything else is IS NULL, including "false" and junk.
        for value in ["false", "", "1", "yes"] {
            assert_eq!(
                has_api_key_filter("hasapikey", value),
                "player.api_key_hash IS NULL"
            );
        }
    }

    #[test]
    fn api_key_format_is_the_exact_go_pattern() {
        // Arrange/Act: apiKeyFormat (:120), tested with the exact strings Go's
        // suite uses (player_repository_test.go:14, :250-262, :411-415).
        // Assert:
        assert!(api_key_format_matches(KEY));
        assert!(api_key_format_matches("nds_ABCDEFGHIJKLMNOPQRSTUV"));
        assert!(
            !api_key_format_matches("0123456789abcdefghijklmn"),
            "no prefix"
        );
        assert!(!api_key_format_matches("nds_short"), "too short");
        assert!(!api_key_format_matches(&format!("{KEY}x")), "too long");
        assert!(
            !api_key_format_matches("nds_0123456789abcdefghij-!"),
            "bad chars"
        );
        assert!(!api_key_format_matches(""), "empty");
        assert!(validate_api_key(KEY).is_ok());
        assert!(matches!(
            validate_api_key("nds_bad"),
            Err(PlayerError::ApiKeyValidation(ApiKeyValidationError {
                field: "apiKey",
                message_key: "resources.player.validation.apiKeyFormat",
            }))
        ));
    }

    #[test]
    fn hash_api_key_is_lowercase_sha256_hex() {
        // Arrange/Act: hashAPIKey (:195-198), pinned against SHA-256 vectors
        // computed for Go's own test keys.
        // Assert:
        assert_eq!(
            hash_api_key(KEY),
            "67e1908fcd2cbb6592cca1b0f24e935cdc300526de424a3f09cfff91dcf97e0b"
        );
        assert_eq!(
            hash_api_key("nds_ABCDEFGHIJKLMNOPQRSTUV"),
            "58d416303860d0df2ae22ddb9764ab7d916189404cdcb6c03f249b8859653d22"
        );
        assert_eq!(
            hash_api_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_api_key("").len(), 64);
    }

    #[test]
    fn save_insert_sorts_columns_and_carries_id_plus_hash_in_one_statement() {
        // Arrange/Act: Save's INSERT (:148-155) via SetMap (squirrel@v1.5.4/insert.go:271-287).
        let (sql, args) = save_insert_sql(&record(), "generated-id", &hash_api_key(KEY));
        // Assert: the sorted column list — api_key_hash first — and no
        // username/has_api_key/api_key column (model/player.go:11,24-25).
        assert_eq!(
            sql,
            "INSERT INTO player (api_key_hash,client,id,ip,last_seen,max_bit_rate,name,\
             report_real_path,scrobble_enabled,transcoding_id,user_agent,user_id) \
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"
        );
        assert_eq!(args[0], Value::from(hash_api_key(KEY)));
        assert!(args.iter().any(|a| *a == Value::from("generated-id")));
        assert!(!sql.contains("has_api_key"));
        assert!(!sql.contains(KEY), "the plaintext key is never a column");
    }

    #[test]
    fn update_owned_forces_id_drops_user_id_and_sorts_the_set() {
        // Arrange/Act: Update with no key (:169) via updateOwned
        // (sql_base_repository.go:506-515).
        let (sql, args) = update_owned_sql(
            PLAYER_ID,
            &record(),
            &["ip", "maxBitRate"],
            WriteAccess::OwnerOrAdmin,
            &regular(OWNER),
        );
        // Assert: the requested snake_case columns plus the forced id
        // (filterUpdateValues, :611-614), sorted; user_id never written (:512).
        assert_eq!(
            sql,
            "UPDATE player SET id = ?, ip = ?, max_bit_rate = ? WHERE (id = ? AND user_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::from(PLAYER_ID),
                Value::from("127.0.0.1"),
                Value::Integer(128),
                Value::from(PLAYER_ID),
                Value::from(OWNER),
            ]
        );
    }

    #[test]
    fn update_owned_drops_a_spoofed_user_id_even_when_requested() {
        // Arrange: the spoof-attack column list from
        // player_repository_test.go:548-560.
        let moved = record();
        // Act:
        let (sql, _) = update_owned_sql(
            PLAYER_ID,
            &PlayerRecord {
                user_id: "attacker".to_string(),
                ..moved
            },
            &["name", "user_id", "max_bit_rate"],
            WriteAccess::OwnerOrAdmin,
            &regular("attacker"),
        );
        // Assert: no user_id in the SET list at all (the ownership predicate in
        // the WHERE clause is not a write).
        let set_clause = sql
            .split(" SET ")
            .nth(1)
            .expect("an UPDATE")
            .split(" WHERE ")
            .next()
            .expect("a WHERE follows the SET");
        assert!(!set_clause.contains("user_id = ?"), "{set_clause}");
        assert!(set_clause.contains("name = ?"));
    }

    #[test]
    fn update_owned_renders_the_admin_and_headless_shapes() {
        // Arrange/Act: ownedRow's ownerOrAdmin arm (:100-101) for callers whose
        // ownerFilter is nil.
        let (admin_sql, admin_args) = update_owned_sql(
            PLAYER_ID,
            &record(),
            &["ip"],
            WriteAccess::OwnerOrAdmin,
            &admin(),
        );
        let (headless_sql, headless_args) = update_owned_sql(
            PLAYER_ID,
            &record(),
            &["ip"],
            WriteAccess::OwnerOrAdmin,
            &LoggedUser::headless(),
        );
        // Assert: admins and headless contexts are restricted to the row id; the
        // SET list still carries the forced id, so it binds twice.
        assert!(admin_sql.ends_with("WHERE (id = ?)"));
        assert_eq!(
            admin_args,
            vec![
                Value::from(PLAYER_ID),
                Value::from("127.0.0.1"),
                Value::from(PLAYER_ID)
            ]
        );
        assert!(headless_sql.ends_with("WHERE (id = ?)"));
        assert_eq!(
            headless_args,
            vec![
                Value::from(PLAYER_ID),
                Value::from("127.0.0.1"),
                Value::from(PLAYER_ID)
            ]
        );
    }

    #[test]
    fn set_api_key_owner_only_even_for_admins() {
        // Arrange/Act: SetAPIKey's two arms (:212-218).
        let (set_sql, set_args) = set_api_key_sql(
            PLAYER_ID,
            Some(&hash_api_key(KEY)),
            WriteAccess::OwnerOnly,
            &admin(),
        );
        let (revoke_sql, revoke_args) =
            set_api_key_sql(PLAYER_ID, None, WriteAccess::OwnerOrAdmin, &admin());
        // Assert: setting always pins user_id, admins included, and writes only
        // the hash (Go's hand-built map has no id column).
        assert_eq!(
            set_sql,
            "UPDATE player SET api_key_hash = ? WHERE (id = ? AND user_id = ?)"
        );
        assert_eq!(
            set_args,
            vec![
                Value::from(hash_api_key(KEY)),
                Value::from(PLAYER_ID),
                Value::from(OWNER),
            ]
        );
        // Assert: revoking is ownerOrAdmin — an admin's clause has no user_id,
        // and NULL rides as a bound parameter.
        assert_eq!(
            revoke_sql,
            "UPDATE player SET api_key_hash = ? WHERE (id = ?)"
        );
        assert_eq!(revoke_args, vec![Value::Null, Value::from(PLAYER_ID)]);
    }

    #[test]
    fn delete_pins_one_restricted_statement_per_id() {
        // Arrange/Act: Delete (:190-192) → deleteOwnedAll (:542-549).
        let statements = delete_sql(&["a", "b"], &regular(OWNER));
        // Assert:
        assert_eq!(statements.len(), 2);
        for (index, statement) in statements.iter().enumerate() {
            let want = if index == 0 { "a" } else { "b" };
            assert_eq!(
                statement.0,
                "DELETE FROM player WHERE (id = ? AND user_id = ?)"
            );
            assert_eq!(statement.1, vec![Value::from(want), Value::from(OWNER)]);
        }
    }

    #[test]
    fn exists_pins_the_unowned_probe() {
        // Arrange/Act: exists (sql_base_repository.go:488-493).
        let (sql, args) = exists_sql(PLAYER_ID);
        // Assert: squirrel renders the bare Eq inside parens, no restriction.
        assert_eq!(sql, "SELECT count(*) as exist FROM player WHERE (id = ?)");
        assert_eq!(args, vec![Value::from(PLAYER_ID)]);
    }

    #[test]
    fn sort_mapping_renames_user_name_to_username() {
        // Arrange/Act: setSortMappings (:29-31).
        // Assert:
        assert_eq!(sort_mapping("user_name"), Some("username"));
        assert_eq!(sort_mapping("name"), None);
        // sanitizeSort validates but does not map (:71-74): a mapped field comes
        // back snake-cased even though it is not whitelisted.
        assert_eq!(
            sanitize_sort("userName", "desc"),
            ("user_name".to_string(), "desc".to_string())
        );
        assert_eq!(
            sanitize_sort("userName", ""),
            ("user_name".to_string(), String::new())
        );
        // A whitelisted plain column is kept as-is.
        assert_eq!(
            sanitize_sort("name", "asc"),
            ("name".to_string(), "asc".to_string())
        );
        assert_eq!(
            sanitize_sort("maxBitRate", "asc"),
            ("max_bit_rate".to_string(), "asc".to_string())
        );
        // Unmapped and unwhitelisted is dropped: `username` is neither (only
        // `user_name` is a mapping key, and structs:"-" keeps it out of the
        // whitelist).
        assert_eq!(
            sanitize_sort("username", "asc"),
            (String::new(), "asc".to_string())
        );
        assert_eq!(
            sanitize_sort("artist", "asc"),
            (String::new(), "asc".to_string())
        );
        // buildSortOrder resolves the mapping and defaults to asc. A part that
        // spells its own direction is not a mapping key, so Go leaves the raw
        // snake_case column in place (`sql_base_repository.go:172-181`) — the
        // mapping only ever applies to a bare name.
        assert_eq!(build_sort_order("user_name", ""), "username asc");
        assert_eq!(build_sort_order("user_name", "DESC"), "username desc");
        assert_eq!(build_sort_order("user_name desc", "asc"), "user_name desc");
        assert_eq!(build_sort_order("user_name asc", "desc"), "user_name desc");
        assert_eq!(
            build_sort_order("name, user_name", "asc"),
            "name asc, username asc"
        );
        assert_eq!(mapped_sort("user_name, name"), "username, name");
        assert_eq!(mapped_sort("user_name desc"), "user_name desc");
    }

    #[test]
    fn rest_repo_registers_the_go_filters_and_snake_case_whitelist() {
        // Arrange/Act: registerModel (:25-28).
        let repo = rest_repo();
        let request = options(&[
            ("hasapikey", "true"),
            ("name", "jazz"),
            ("username", "x"),
            ("id", "p1"),
        ]);
        // Act:
        let parsed = repo.parse_rest_options(Some(&request));
        // Assert: both custom mappings win over the generic field path
        // (sql_restful.go:27-31); `username` is not whitelisted (structs:"-",
        // model/player.go:11) so it is dropped, and `id` matches the snake-cased
        // whitelist entry as a bare exact-match column. Go walks a map (random
        // order); the port's BTreeMap gives sorted field order.
        let (sql, args) = parsed.where_clause().expect("clauses");
        assert_eq!(
            sql,
            "player.api_key_hash IS NOT NULL AND id = ? AND player.name LIKE ?"
        );
        assert_eq!(args, vec![Value::from("p1"), Value::from("%jazz%")]);
    }

    #[test]
    fn options_from_rest_carries_filter_values() {
        // Arrange/Act: parseRestOptions (:55-67) with a mapped sort, a page and a
        // value-bearing filter.
        let request = RestQueryOptions {
            sort: "userName".to_string(),
            order: "DESC".to_string(),
            max: 5,
            offset: 2,
            filters: BTreeMap::from([("name".to_string(), "jazz".to_string())]),
        };
        let parsed = parse_player_rest_options(Some(&request));
        // Assert:
        assert_eq!(parsed.sort, "user_name");
        assert_eq!(parsed.order, "desc");
        assert_eq!(parsed.max, 5);
        assert_eq!(parsed.offset, 2);
        assert_eq!(parsed.filters.as_deref(), Some("player.name LIKE ?"));
        assert_eq!(parsed.filter_args, vec![Value::from("%jazz%")]);
        // Assert: no options at all is Go's zero-value QueryOptions (:56-57).
        assert_eq!(parse_player_rest_options(None), Options::default());
    }

    #[test]
    fn put_sql_updates_then_inserts_the_full_row() {
        // Arrange/Act: Put (:42-45) via put (sql_base_repository.go:613-641).
        let statements = put_sql(&record());
        let update = statements.update.expect("an id was given");
        // Assert: the UPDATE sets the whole row (user_id included — put does not
        // drop ownership) against the id, and the INSERT is the same payload.
        assert!(update
            .0
            .starts_with("UPDATE player SET client = ?, id = ?, ip = ?, "));
        assert!(update.0.contains("user_id = ?"));
        assert!(update.0.ends_with(" WHERE id = ?"));
        assert!(statements.insert.0.contains("user_agent,user_id) VALUES"));
        assert!(
            !statements.insert.0.contains("api_key_hash"),
            "Put never writes the hash (toSQLArgs has no such field)"
        );
        assert_eq!(statements.id, PLAYER_ID);
    }

    #[test]
    fn put_sql_mints_an_id_for_a_new_row() {
        // Arrange: an empty id (sql_base_repository.go:636-639).
        let fresh = PlayerRecord {
            id: String::new(),
            ..record()
        };
        // Act:
        let statements = put_sql(&fresh);
        // Assert: a generated id, no UPDATE leg, and the id inside the INSERT.
        assert_eq!(statements.id.len(), nd_id::ID_LENGTH);
        assert!(statements.update.is_none());
        assert!(statements
            .insert
            .1
            .iter()
            .any(|a| *a == Value::from(statements.id.as_str())));
    }

    #[test]
    fn validation_errors_carry_the_go_message_keys() {
        // Arrange/Act: apiKeyValidationError (:122-124) and its three call sites
        // (:143, :128, :157/:220).
        // Assert:
        assert_eq!(
            ApiKeyValidationError::required().message_key,
            "ra.validation.required"
        );
        assert_eq!(
            ApiKeyValidationError::format().message_key,
            "resources.player.validation.apiKeyFormat"
        );
        assert_eq!(
            ApiKeyValidationError::unique().message_key,
            "ra.validation.unique"
        );
        assert_eq!(ApiKeyValidationError::unique().field, "apiKey");
        assert_eq!(
            ApiKeyValidationError::unique().errors(),
            HashMap::from([("apiKey".to_string(), "ra.validation.unique".to_string())])
        );
        assert_eq!(
            PlayerError::ApiKeyValidation(ApiKeyValidationError::required()).to_string(),
            "apiKey: ra.validation.required"
        );
    }

    #[test]
    fn unique_violation_matches_the_message_text_like_go() {
        // Arrange: a real constraint failure, produced the way SQLite reports the
        // api-key collision (isUniqueViolation, :225-227, matches the message
        // text rather than an error code).
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE player (id text primary key, api_key_hash text);
             CREATE UNIQUE INDEX player_api_key_hash on player(api_key_hash);
             INSERT INTO player(id, api_key_hash) VALUES('p1','h');",
        )
        .expect("fixture");
        // Act:
        let err = conn
            .execute("INSERT INTO player(id, api_key_hash) VALUES('p2','h')", [])
            .expect_err("the unique index rejects the duplicate hash");
        // Assert:
        assert!(is_unique_violation(&err));
        assert!(err.to_string().contains("player.api_key_hash"));
        assert!(!is_unique_violation(&rusqlite::Error::QueryReturnedNoRows));
    }

    #[test]
    fn owner_filter_is_nil_for_admin_and_headless() {
        // Arrange/Act: ownerFilter (sql_base_repository.go:67-72).
        // Assert:
        assert_eq!(owner_filter(&admin()), None);
        assert_eq!(owner_filter(&LoggedUser::headless()), None);
        assert_eq!(owner_filter(&regular(OWNER)), Some(OWNER));
    }
}
