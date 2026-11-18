//! Port of `persistence/radio_repository.go` (143L) — the internet-radio
//! station table.
//!
//! Scope: the row select shapes (`Get`/`GetAll`, `:58-71`), `CountAll` and
//! `Exists` (`:43-56`), the permission-gated delete (`:51-57`, one
//! `deleteByID` per id — a zero-row delete is Go's `model.ErrNotFound`),
//! the `Put` upsert (`:90-116` via `put`, `sql_base_repository.go:613-641`:
//! UPDATE first, INSERT when no row matched or the id was empty) and the
//! artwork-resolution enqueue that never fails the save (`:107-113`).
//!
//! `Save`/`Update` (`:118-141`) are `Put` wrappers: `Update` forces the
//! entity's `ID` and passes the client's column list; `Save` is a bare `Put`.
//! They are the caller composing [`put_sql`], not separate SQL shapes.
//!
//! Ownership: Go gates writes on `isPermitted` (`:27-30` — admin only,
//! `rest.ErrPermissionDenied`); the crate has no ambient context (the
//! `sqlrepo.rs` DI rule), so the caller checks admin before calling —
//! nothing in the SQL layer can stand in for the gate. Reads are unfiltered:
//! `radioRepository` defines NO `userFilter` at all (`grep -n "userFilter"
//! persistence/radio_repository.go` is empty; the playlist repository's is at
//! `playlist_repository.go:83-92`) and `newSelect` adds no clause
//! (`sql_base_repository.go:139-147`), so `Get`/`GetAll` carry no ownership or
//! library filter. Go's own comment saying radios are visible to every user sits on
//! `Exists` (`:38`), not on the reads. `registerModel`'s only filter mapping is
//! `containsFilter("name")` (`:20-22`).
//!
//! Timestamps: `radio.created_at`/`updated_at` are the non-optional
//! `time.Time` of `model/radio.go:19-20`, stored as TEXT via [`crate::time`]
//! (rusqlite's `DateTime<Utc>` shape — see `time.rs` for why epoch integers
//! silently break range predicates and ordering). The caller owns the clock —
//! `Put`'s `UpdatedAt = time.Now()` / fresh-row `CreatedAt` / `id.NewRandom()`
//! minting (`:96-100`) are reproduced by [`put_sql`] taking `now` and minting
//! through [`nd_id::new_random`].
//!
//! Not ported: nothing of the read side remains — `hydrateArtwork`
//! (`:79-83`) landed as
//! [`crate::artwork_hydration::hydrate_radio_artwork`] (the shared batched
//! `item_artwork` lookup of `artwork_hydration.go:58-90`) and runs inside
//! [`RadioRepo::get`]/[`RadioRepo::get_all`]. `uploaded_image` is a
//! migration-added column in Go's schema; `nd-dbschema`'s fresh DDL must
//! carry it for the INSERT to bind (residual dependency on that lane).

use chrono::{DateTime, Utc};

use crate::marshal::to_snake_case;
use crate::sql_restful::{contains_filter, RestRepository};
use crate::sqlrepo::Value;
use crate::time;
use nd_id::new_random;

/// `r.tableName` (`radio_repository.go:22` via `registerModel(&model.Radio{})`,
/// `sql_base_repository.go:104-111`).
pub const TABLE: &str = "radio";

/// The artwork queue `Put` enqueues into (`:107-113`,
/// `artwork_queue_repository.go:47-50`).
pub const ARTWORK_QUEUE_TABLE: &str = "artwork_queue";

/// `model.KindRadioArtwork.Prefix()` (`model/artwork_id.go:33`).
pub const RADIO_ARTWORK_KIND: &str = "ra";

/// `model.ImageTypePrimary` (`model/artwork.go:22`).
pub const IMAGE_TYPE_PRIMARY: &str = "primary";

/// `model.ArtworkPriorityBump` (`model/artwork.go:117`): the enqueue priority
/// so a new radio's cover resolves proactively (`:108-109`).
pub const ARTWORK_PRIORITY_BUMP: i64 = 100;

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// One `SET column = value` pair of the `put` upsert (`:96-101` via
/// `toSQLArgs`/`PostMapArgs`): identifiers are `model.Radio`'s fixed
/// snake_case `structs:` tags, values are bound.
#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    /// The column name — never user text.
    pub column: String,
    /// Its bound value.
    pub value: Value,
}

impl Col {
    /// A text SET pair.
    #[must_use]
    pub fn text(column: &str, value: &str) -> Self {
        Self {
            column: column.to_string(),
            value: Value::Text(value.to_string()),
        }
    }

    /// An integer SET pair.
    #[must_use]
    pub fn integer(column: &str, value: i64) -> Self {
        Self {
            column: column.to_string(),
            value: Value::Integer(value),
        }
    }

    /// A timestamp SET pair: bound with [`crate::time`]'s TEXT shape, never
    /// an epoch integer.
    #[must_use]
    pub fn timestamp(column: &str, value: &DateTime<Utc>) -> Self {
        Self {
            column: column.to_string(),
            value: time::value(value),
        }
    }

    /// A raw bound-value SET pair, for columns this repository does not model
    /// as a chrono timestamp (the artwork queue's `retry_at`/`enqueued_at`,
    /// see [`enqueue_artwork_sql`]).
    #[must_use]
    pub fn value(column: &str, value: Value) -> Self {
        Self {
            column: column.to_string(),
            value,
        }
    }
}

/// `model.Radio` minus the `structs:"-"` embeds (`model/radio.go:12-21`):
/// exactly the columns `toSQLArgs` yields, in struct order. `created_at`/
/// `updated_at` are the non-optional `time.Time` of `model/radio.go:19-20`,
/// stored via [`crate::time`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RadioRecord {
    /// `id` — empty for a fresh row; [`put_sql`] mints it.
    pub id: String,
    /// `stream_url`.
    pub stream_url: String,
    /// `name` (unique, NOCASE).
    pub name: String,
    /// `home_page_url`.
    pub home_page_url: String,
    /// `uploaded_image` (migration-added column; see module header).
    pub uploaded_image: String,
    /// `created_at`.
    pub created_at: DateTime<Utc>,
    /// `updated_at`.
    pub updated_at: DateTime<Utc>,
    /// The promoted `ItemImage` embed (`model/radio.go:12`, `structs:"-"`): the
    /// radio's own artwork state, **read-path only**. Filled by the SECOND layer
    /// ([`crate::artwork_hydration::hydrate_radio_artwork`],
    /// `radio_repository.go:80-83`). The read side scans straight into
    /// `model.Radio`: Go has no `dbRadio` wrapper, no `PostScan` and no shim column
    /// (`grep -n "dbRadio\|PostScan" persistence/radio_repository.go` returns
    /// nothing), so there is NO phase 2 for radio and this field stays zero
    /// (= UNRESOLVED) until the artwork layer runs. Zero is NOT known-absent; see
    /// [`crate::artwork_hydration::ItemImage`]. Never emitted by [`put_cols`],
    /// which stays at exactly the 7 persisted columns.
    pub image: crate::artwork_hydration::ItemImage,
}

/// `toSQLArgs(&model.Radio{})` (`helpers.go:18-37`): every column, struct
/// order, as SET pairs.
#[must_use]
pub fn put_cols(radio: &RadioRecord) -> Vec<Col> {
    vec![
        Col::text("id", &radio.id),
        Col::text("stream_url", &radio.stream_url),
        Col::text("name", &radio.name),
        Col::text("home_page_url", &radio.home_page_url),
        Col::text("uploaded_image", &radio.uploaded_image),
        Col::timestamp("created_at", &radio.created_at),
        Col::timestamp("updated_at", &radio.updated_at),
    ]
}

/// The columns `selectUpdateColumns` always drops on update
/// (`sql_base_repository.go:605-608`); `birth_time` does not exist on radio.
pub const NEVER_UPDATE_COLS: [&str; 1] = ["created_at"];

/// `registerModel`'s rest-facing half (`:16-22`): the `radio` table with its
/// one filter mapping (`containsFilter("name")`) and the field whitelist
/// (`registerModelWhiteList(&model.Radio{})` + annotation fields,
/// `sql_restful.go:142-163`).
#[must_use]
pub fn rest_repo() -> RestRepository {
    RestRepository::new(TABLE)
        .with_filter_mappings([("name", contains_filter("name"))])
        .with_field_white_list([
            "ID",
            "StreamUrl",
            "Name",
            "HomePageUrl",
            "UploadedImage",
            "CreatedAt",
            "UpdatedAt",
        ])
}

/// `sanitizeSort` for this repository (`sql_restful.go:69-86` via
/// [`RestRepository::sanitize_sort`] plus the per-repository `lookupSortMapping` arm
/// Go validates against at `:74`). The mapping arm is vacuous here — `radioRepository`
/// never calls `setSortMappings` (`grep -n setSortMappings
/// persistence/radio_repository.go` is empty), so `sortMapping` falls through to its
/// snake_case fallback (`sql_base_repository.go:194-196`) and a sort field survives only
/// when it is a whitelisted field name of `model.Radio`.
///
/// Kept as an explicit function even though this repository's renderer has no ORDER BY
/// leg (see [`get_all_sql`]'s note): it is the whitelist check Go runs in
/// `sanitizeSort`, and it is what [`parse_rest_options`] hands to the count and read
/// renderers, so a hostile sort is provably dropped rather than merely unrendered.
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
/// [`crate::sql_restful::QueryOptions`] that [`get_all_sql`] and [`count_all_sql`]
/// consume — Go's `ReadAll` (`radio_repository.go:121`) and `Count` (`:113`) option
/// parsing in full, including `sanitizeSort` (`:57`) and the `"seed"` lift-out
/// (`:60-63`).
///
/// This is the ONLY sanctioned way to build the options handed to [`get_all_sql`] and
/// [`count_all_sql`]: it is what makes the sort whitelist-checked and every filter
/// either mapped (`name` → `containsFilter("name")`, `:22-24`) or whitelist-enforced
/// before it reaches the statement.
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

/// `sort_mapping` (`lookupSortMapping`, `sql_base_repository.go:187-197`) over this
/// repository's map, which is EMPTY (`radioRepository` calls no `setSortMappings`,
/// `radio_repository.go:19-26`), so every lookup misses and Go's `sortMapping`
/// fallback (`:194-196`) snake-cases the field. Recorded as a constant so the empty map
/// is a pinned fact rather than an accident, matching `library_repo::SORT_MAPPINGS`.
pub const SORT_MAPPINGS: [(&str, &str); 0] = [];

/// [`SORT_MAPPINGS`] lookup, mirroring `library_repo::sort_mapping` and
/// `tag_repo::sort_mapping`.
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort)
        .map(|(_, value)| *value)
}

/// `Get`/`Read` (`:56-66,116-118`): `SELECT * … WHERE id = ?` — radios are
/// visible to every user: `radioRepository` defines no `userFilter` and `newSelect`
/// adds no clause (`sql_base_repository.go:139-147`).
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    (
        format!("SELECT * FROM {TABLE} WHERE id = ?"),
        vec![Value::Text(id.to_string())],
    )
}

/// `GetAll`/`ReadAll` (`:68-71,120-122`): `newSelect(ctx, options...).Columns("*")`
/// — the caller's filters and pagination applied.
///
/// WARNING: `options` here must ALREADY be the output of [`parse_rest_options`]
/// (`sql_restful.go:54-67`) — this renderer is a pure statement builder and does NOT
/// sanitize `options.sort` or its filter clauses itself, so a hand-built
/// [`crate::sql_restful::QueryOptions`] reaches the statement verbatim (injection
/// through the sort field or a `Filter::Raw` clause, exactly as in
/// `transcoding_repo.rs:161-167`). Go has no such gap because `ReadAll` calls
/// `parseRestOptions` itself (`radio_repository.go:121`) and `sanitizeSort`
/// (`sql_restful.go:69-86`) plus the `parseRestFilters` whitelist arm (`:39-42`)
/// therefore always run first; [`RadioRepo::get_all`] mirrors that by parsing its own
/// rest options, which is why the executed path is safe. The residual risk is confined
/// to callers of this public renderer: the checks live in [`sanitize_sort`] and
/// [`rest_repo`], not here. Go's renderer likewise accepts whatever `applyOptions` and
/// `applyFilters` are handed (`sql_base_repository.go:157-158`, `:247-252`), so this
/// renderer is deliberately NOT tightened.
///
/// NOTE (unchanged residual): this renderer emits no ORDER BY leg at all —
/// [`crate::sql_search::apply_pagination`] carries only LIMIT/OFFSET — so
/// `options.sort`/`options.order` affect nothing here. That divergence from Go's
/// `applyOptions` (`sql_base_repository.go:157-158`) is documented in
/// `docs/rust-port/hydrate-contract.md` §S-D3f and stays out of this slice's scope;
/// [`RadioRepo::get_all`] parses its options regardless, so the gate is in place for
/// whichever lane eventually renders the leg.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize (it
/// always can here — the table is fixed).
pub fn get_all_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = crate::sqlrepo::Repository::new(TABLE).select(["*"]);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
        sq = crate::sql_search::apply_pagination(sq, options);
    }
    sq.to_sql()
}

/// `CountAll`/`Count` (`:33-36,112-114` via `count`,
/// `sql_base_repository.go:565-575`): `count(distinct radio.id) as count` with the
/// caller's filters kept.
///
/// `count` always resets the sort to `OrderBy(r.tableName+".id")` before running the
/// statement (`sql_base_repository.go:569`, "To remove any ORDER BY clause that
/// could slow down the query"), so Go's traced text is
/// `SELECT count(distinct radio.id) as count FROM radio ORDER BY radio.id` (executed,
/// oracle `sql_shapes.txt`). The leg contributes no argument and cannot change a
/// count, but `playlist_repo::count_all_sql` emits its equivalent and the two files
/// in this lane should not disagree, so it is rendered here too.
///
/// WARNING: `options` here must ALREADY be the output of [`parse_rest_options`]
/// (`sql_restful.go:54-67`), for the same filter-channel reason as [`get_all_sql`]:
/// [`crate::sql_search::apply_filters`] takes the parsed clauses, so a hand-built
/// [`crate::sql_restful::QueryOptions`] with a raw clause reaches WHERE verbatim. A
/// caller sort cannot reach this statement — `count` installs `radio.id`
/// (`sql_base_repository.go:569`) — so the count is invariant under
/// `options.sort`/`options.order` and there is no ordering channel here. Go's REST
/// `Count` parses rest options itself (`radio_repository.go:113`), mirrored by
/// [`RadioRepo::count`]. This renderer is deliberately NOT tightened.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] when the builder cannot materialize.
pub fn count_all_sql(
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let mut sq = crate::sqlrepo::Repository::new(TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    // `count` removes any caller ORDER BY and installs `table.id`
    // (`sql_base_repository.go:566-570`); `Repository::count` already drops the
    // sort legs, so this restores the reset one Go always applies.
    sq = sq.count(TABLE).order_by([format!("{TABLE}.id")]);
    sq.to_sql()
}

/// `Exists` (`:48-50` via `exists`, `sql_base_repository.go:488-493`):
/// `count(*) as exist` over the id — no ownership filter, per the Go comment.
#[must_use]
pub fn exists_sql(id: &str) -> Sql {
    (
        format!("SELECT count(*) as exist FROM {TABLE} WHERE id = ?"),
        vec![Value::Text(id.to_string())],
    )
}

/// `Delete` (`:43-54` via `deleteByID`, `sql_base_repository.go:650-658`):
/// one DELETE per id, in order; a zero-row delete is the caller's
/// not-found. The admin gate is the caller's (see module header).
#[must_use]
pub fn delete_sql(ids: &[String]) -> Vec<Sql> {
    ids.iter()
        .map(|id| {
            (
                format!("DELETE FROM {TABLE} WHERE id = ?"),
                vec![Value::Text(id.clone())],
            )
        })
        .collect()
}

/// [`put_sql`]'s output: the upsert pair plus the follow-ups Go's `Put` runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PutSql {
    /// The id the row lands under — [`nd_id::new_random`] when the record
    /// came in empty (`:98-99`), else the record's.
    pub id: String,
    /// `put`'s UPDATE leg (`sql_base_repository.go:619-627`); execute first,
    /// and only run [`Self::insert`] when it matched no row.
    pub update: Sql,
    /// `put`'s INSERT leg (`:629-635`).
    pub insert: Sql,
    /// The artwork-resolution enqueue (`:107-113`); never fails the save.
    pub enqueue_artwork: Sql,
}

/// `Put` (`:90-116`): stamp `updated_at` (always) and `created_at` (fresh
/// rows), mint the id when empty, then the `put` upsert — an UPDATE against
/// the id whose SET list is [`select_update_cols`]'s output (the requested
/// columns plus the appended `updated_at`, `:101-103`, always minus
/// `created_at`, `sql_base_repository.go:601-604`), followed by the full-row
/// INSERT — plus the Bump-priority artwork enqueue.
///
/// `cols_to_update` are column NAMES as Go's callers pass them, not literal
/// SQL identifiers: Go's `Put` appends `"UpdatedAt"` in camelCase
/// (`radio_repository.go:95-97`) and `selectUpdateColumns` runs every requested
/// name through `toSnakeCase` before matching the write map
/// (`sql_base_repository.go:596-598`). So camelCase names such as
/// `"StreamUrl"`/`"HomePageUrl"`/`"Name"` — exactly what the real Go callers pass
/// (`server/subsonic/radio.go:122`, `server/nativeapi/radios.go:48` pass
/// `"UploadedImage"`) — behave like Go's. The port normalizes through
/// [`crate::marshal::to_snake_case`], the same seam `playlist_repo` uses; matching
/// verbatim would silently drop the caller's columns and update only `updated_at`.
#[must_use]
pub fn put_sql(radio: &RadioRecord, now: DateTime<Utc>, cols_to_update: &[&str]) -> PutSql {
    let id = if radio.id.is_empty() {
        new_random()
    } else {
        radio.id.clone()
    };
    let created_at = if radio.id.is_empty() {
        now
    } else {
        radio.created_at
    };
    let stamped = RadioRecord {
        id: id.clone(),
        created_at,
        updated_at: now,
        ..radio.clone()
    };
    let all_cols = put_cols(&stamped);
    let update_cols = select_update_cols(&all_cols, cols_to_update);
    let set = update_cols
        .iter()
        .map(|c| format!("{} = ?", c.column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut update_args: Vec<Value> = update_cols.iter().map(|c| c.value.clone()).collect();
    update_args.push(Value::Text(id.clone()));
    let update = (
        format!("UPDATE {TABLE} SET {set} WHERE id = ?"),
        update_args,
    );
    // squirrel's Insert().SetMap renders a column list plus one VALUES tuple
    // (`insert.go:92-105,269-287`), not MySQL's `SET col = ?` — SQLite rejects
    // the latter, so the INSERT leg has to be the VALUES form to execute.
    let insert_cols = all_cols
        .iter()
        .map(|c| c.column.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let insert_placeholders = all_cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let insert_args: Vec<Value> = all_cols.iter().map(|c| c.value.clone()).collect();
    let insert = (
        format!("INSERT INTO {TABLE} ({insert_cols}) VALUES ({insert_placeholders})"),
        insert_args,
    );
    PutSql {
        id: id.clone(),
        update,
        insert,
        enqueue_artwork: enqueue_artwork_sql(&id, &now),
    }
}

/// `selectUpdateColumns` + the `UpdatedAt` append (`:95-97`,
/// `sql_base_repository.go:591-609`): all columns when none requested, else the
/// `toSnakeCase`-normalized requested names plus `updated_at`, always minus
/// `created_at`. Normalization happens BEFORE the append-matching, exactly as Go
/// does it (`c2upd := slice.ToMap(colsToUpdate, toSnakeCase)`, `:596-598`), so a
/// caller passing `"UpdatedAt"` is not duplicated and a camelCase name still matches
/// its snake_case column.
#[must_use]
fn select_update_cols(all_cols: &[Col], cols_to_update: &[&str]) -> Vec<Col> {
    // Go appends the camelCase name (`radio_repository.go:96`) and snake-cases the
    // whole slice (`sql_base_repository.go:596-598`); appending first and
    // normalizing after is the same result.
    let mut wanted: Vec<String> = cols_to_update.iter().map(|c| c.to_string()).collect();
    if !wanted.is_empty() {
        // Go appends unconditionally when the slice is non-empty
        // (`radio_repository.go:95-97`); a duplicate collapses because the names are
        // matched through a map (`sql_base_repository.go:596-598`).
        wanted.push("UpdatedAt".to_string());
    }
    let wanted: Vec<String> = wanted.iter().map(|c| to_snake_case(c)).collect();
    all_cols
        .iter()
        .filter(|c| wanted.is_empty() || wanted.iter().any(|w| w == &c.column))
        .filter(|c| !NEVER_UPDATE_COLS.contains(&c.column.as_str()))
        .cloned()
        .collect()
}

/// The artwork enqueue of `Put` (`:107-113` via `Enqueue`,
/// `artwork_queue_repository.go:47-50`): the Bump-priority upsert that starts
/// a fresh lifecycle on conflict. `retry_at`/`enqueued_at` bind the same
/// caller-owned `now` through [`crate::time`] — Go binds `time.Now()`
/// (`artwork_queue_repository.go:63`), so they are datetime TEXT here too.
#[must_use]
pub fn enqueue_artwork_sql(radio_id: &str, now: &DateTime<Utc>) -> Sql {
    (
        format!(
            "INSERT INTO {ARTWORK_QUEUE_TABLE} \
             (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        ),
        vec![
            Value::Text(RADIO_ARTWORK_KIND.to_string()),
            Value::Text(radio_id.to_string()),
            Value::Text(IMAGE_TYPE_PRIMARY.to_string()),
            Value::Integer(ARTWORK_PRIORITY_BUMP),
            time::value(now),
            time::value(now),
        ],
    )
}

/// Failures of the executed radio reads ([`RadioRepo`]).
///
/// The variant set follows the sibling `ArtistRepo::ArtistError`
/// (`artist_repo.rs:1498-1521`) — a statement/scan failure and the not-found case —
/// plus the one extra arm this repository needs because its statement builder
/// returns a `Result`: [`RadioError::Builder`]. `PlayerError`
/// (`player_repo.rs:243-272`) and `ShareError` (`share_repo.rs:161-180`) set the
/// precedent for more than two arms when Go has more error paths.
#[derive(Debug, thiserror::Error)]
pub enum RadioError {
    /// The SQLite statement failed, or a row could not be scanned. Unlike the
    /// playlist and artist reads there is no `PostScan` hook to fail here —
    /// `grep -n "dbRadio\|PostScan" persistence/radio_repository.go` is empty —
    /// so this variant is a pure statement/scan failure. The one scan failure that
    /// IS reachable is a NULL `created_at`/`updated_at` scanned into Go's
    /// non-pointer `time.Time` (`model/radio.go:19-20`): executed against the
    /// real repository, `Get` on such a row returns
    /// `sql: Scan error on column index 4, name "created_at": unsupported Scan,
    /// storing driver.Value type <nil> into type *time.Time` beside a NON-nil
    /// pointer to a zero `Radio`. `GetAll` returns the same error, but the slice
    /// beside it holds the rows scanned BEFORE the failing one
    /// (`radio_repository.go:71-73` returns `res` with `err`; dbx appends each row
    /// after a successful scan and aborts at `dbx@v1.12.0/rows.go:169-171`, so the
    /// append loop at `:159-193` keeps everything up to the failure). Executed
    /// against the real repository over {ra-ok1, ra-ok2, ra-null}: `len == 2` with
    /// the error; with the NULL row first, `len == 0` (which is the shape the
    /// earlier oracle saw, and why this bullet used to claim "always empty").
    /// The port has no value-beside-error, so it reports only the error — see
    /// [`RadioRepo::get_all`]. The fresh DDL declares both columns as bare
    /// `datetime` with no DEFAULT, so NULL is reachable. The port reports it here —
    /// NOT as [`RadioError::NotFound`] and NOT as a zero timestamp.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The statement could not be materialized — Go's `toSQL` failure
    /// (`sql_base_repository.go:358-372`), which `queryAll` returns unchanged
    /// (`:446-448`) and `GetAll` hands back beside its empty `res`
    /// (`radio_repository.go:71-73`). For this table the builder's only failure
    /// mode is `SqlError::NoTable`, which the fixed `radio` table makes
    /// unreachable; the arm exists so the port does not have to invent a
    /// `rusqlite::Error` for a builder failure.
    #[error(transparent)]
    Builder(#[from] crate::sqlrepo::SqlError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`).
    ///
    /// Radio is the one repository in this slice whose NotFound comes from
    /// `queryOne`: Go's `Get` (`radio_repository.go:56-66`) is a `queryOne`, which
    /// maps `sql.ErrNoRows` to `model.ErrNotFound`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`).
    ///
    /// **Go quirk, ported as information rather than as a value:** Go returns
    /// `(&res, err)` at `:61`, so the pointer beside the error is NON-nil and points
    /// at a zero `Radio` (`ID == ""`, `Name == ""`, zero `ItemImage`) — executed:
    /// `Get("ra-nobody")` gives `err="data not found"` with `ptrNil=false`. The
    /// port's `Err(NotFound)` carries the same information minus that dead pointer,
    /// which no Go caller can legitimately read.
    ///
    /// [`RadioRepo::get_all`] NEVER reports this variant: `GetAll` is `queryAll`, and
    /// dbx's `All` loops `Next` then `Close` and makes an empty NON-nil slice for a
    /// zero-row result (`dbx@v1.12.0/query.go:278-291`, `rows.go:122-125`), so
    /// `queryAll`'s ErrNoRows→ErrNotFound arm (`sql_base_repository.go:451-454`) is
    /// dead code on that path.
    #[error("data not found")]
    NotFound,
}

/// `radioRepository` (`radio_repository.go:15-17`) bound to one pooled connection —
/// the executed READ side of the radio repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate
/// has no ambient context (the `sqlrepo.rs` DI freeze), so the caller threads a
/// lease from [`crate::store::SQLStore::with_connection_sync`] (a transaction
/// derefs to a `&Connection`) and every statement runs through
/// [`crate::execution`] — the port of `executeSQL`/`queryOne`/`queryAll`
/// (`sql_base_repository.go:336-355,375-388,441-457`). Rows hydrate through
/// [`crate::hydrate::row_to_radio`] — a SINGLE phase, because Go has no `dbRadio`
/// wrapper struct and no `PostScan` hook — and artwork hydration is the second
/// layer, [`crate::artwork_hydration::hydrate_radio_artwork`]
/// (`hydrateArtwork`, `radio_repository.go:80-83`).
///
/// No read takes a user: radios are visible to every user, because `Get`/`GetAll`
/// build their statement with `newSelect` (`:57`, `:69`), which adds only the table,
/// caller filters and pagination (`sql_base_repository.go:139-147`), and this
/// repository defines no `userFilter` at all. Go's own comment saying radios are
/// unfiltered sits on `Exists` (`:38`); the reads are unfiltered for the same reason,
/// so neither [`get_sql`] nor [`get_all_sql`] carries an ownership or library clause.
/// The admin gate (`isPermitted`, `:28-31`) governs WRITES only and stays the
/// caller's (see module header).
#[derive(Debug)]
pub struct RadioRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> RadioRepo<'a> {
    /// `NewRadioRepository` (`radio_repository.go:19-26`): bind to `conn` (the
    /// pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get`/`Read` (`:56-66`, `:116-118`): [`get_sql`] over the id, hydrated
    /// through both layers.
    ///
    /// NotFound is `queryOne`'s no-rows condition (see [`RadioError::NotFound`]),
    /// and as in Go it is decided BEFORE hydration — Go returns at `:60-62`, so a
    /// missing id issues no `item_artwork` lookup at all. Executed: `Get` of an
    /// unknown id traced only `SELECT * FROM radio WHERE id = ?`.
    ///
    /// An artwork-lookup failure is NOT an error here: Go's `hydrateItemImages` logs and
    /// returns an EMPTY map (`artwork_hydration.go:56-68`), leaving the radio with a
    /// zero (unresolved) [`crate::artwork_hydration::ItemImage`]. Executed with
    /// `item_artwork` dropped: `Get` returned `err=""` with
    /// `ItemImage{Hash:"", Absent:false, …}`. That failure is deliberately dropped,
    /// because Go's `Get` has nowhere to report it — use [`RadioRepo::get_all`] to
    /// have it surfaced.
    ///
    /// # Errors
    /// [`RadioError::NotFound`] when the id matches nothing;
    /// [`RadioError::Sqlite`] on a statement or scan failure (see that variant for
    /// the NULL-timestamp case); [`RadioError::Builder`] only if the fixed-table
    /// builder ever fails to materialize. An OFFSET-without-LIMIT page does NOT fail
    /// here — see [`RadioRepo::get_all`] for that documented divergence.
    pub fn get(&self, id: &str) -> Result<RadioRecord, RadioError> {
        let statement = get_sql(id);
        // Go's queryOne (`:59`) → dbx `Rows.one` (`dbx@v1.12.0/rows.go:237-244`);
        // `query_optional`'s `None` is exactly that no-rows condition, which Go
        // maps to model.ErrNotFound.
        let mut record =
            crate::execution::query_optional(self.conn, &statement, crate::hydrate::row_to_radio)?
                .ok_or(RadioError::NotFound)?;
        // Go wraps the single row in `model.Radios{res}` (`:63`) and hydrates that
        // one-element slice — `from_mut` is the same 1-element slice.
        let mut artwork_error = None;
        crate::artwork_hydration::hydrate_radio_artwork(
            self.conn,
            std::slice::from_mut(&mut record),
            &mut artwork_error,
        );
        // Go's swallow-and-log path (`artwork_hydration.go:62-66`): Get reports nothing.
        let _ = artwork_error;
        Ok(record)
    }

    /// `GetAll`/`ReadAll` (`:68-77`, `:120-122`): [`get_all_sql`] under the caller's
    /// REST options — which this method PARSES ITSELF, exactly as Go's `ReadAll` calls
    /// `parseRestOptions` inside the method (`radio_repository.go:121`) so that
    /// `sanitizeSort` (`sql_restful.go:69-86`) and the `parseRestFilters` whitelist arm
    /// (`:38-42`) always run before the statement is built. Here that is
    /// [`parse_rest_options`] feeding [`get_all_sql`]: a sort outside [`SORT_MAPPINGS`]
    /// or [`rest_repo`]'s whitelist is dropped, and a filter that is neither a
    /// registered mapping nor a whitelisted `model.Radio` field is dropped outright
    /// rather than reaching WHERE — which also closes the `Filter::Raw` channel a
    /// hand-built [`crate::sql_restful::QueryOptions`] used to reach this method
    /// verbatim. Then artwork hydration.
    ///
    /// Executed against the real repository,
    /// `Sort: "name", Order: "desc", Max: 2, Offset: 1` traces
    /// `SELECT * FROM radio ORDER BY name desc LIMIT 2 OFFSET 1`, and an
    /// `Offset`-only option (`Max == 0`) traces `SELECT * FROM radio OFFSET 1` —
    /// which SQLite rejects with `near "1": syntax error` in Go (executed).
    /// **Divergence:** the port does NOT fail there. Its builder emits
    /// `LIMIT -1 OFFSET n` ahead of a bare offset (`sqlrepo.rs:313-319`, a
    /// deliberate cited deviation), so the offset-only page EXECUTES and skips rows
    /// — pinned by `offset_without_limit_diverges_the_way_the_builder_documents`.
    /// **Residual:** [`get_all_sql`] renders LIMIT/OFFSET and the filter but NOT the
    /// ORDER BY leg (`apply_pagination`/`apply_filters` have no sort leg), so
    /// `options.sort`/`options.order` change nothing and rows come back in SQLite's
    /// order — the same documented divergence as
    /// `artist_repo::ArtistRepo::get_all`. Reported, not changed: the builder shape
    /// is pinned by `get_all_applies_parsed_filters_and_pagination`.
    ///
    /// Go returns its partially-filled `res` beside a statement error (`:71-73`), so
    /// rows scanned before a failing row survive (see [`RadioError::Sqlite`]).
    /// `Result` has no value-beside-error, so the port returns `Err` and drops them;
    /// no Go caller reads that partial slice on this path either way, and
    /// `playlist_repository.go:200-201` (playlist `GetAll`) returns `nil, err` — the
    /// playlist read has no divergence here.
    ///
    /// An empty table is an empty `Vec` with `artwork_error == None`, never
    /// [`RadioError::NotFound`] (see that variant's dbx proof). An empty slice also
    /// issues no artwork query at all — `hydrateItemImages` returns early on no ids
    /// (`artwork_hydration.go:59-61`).
    ///
    /// `artwork_error` carries Go's swallowed `log.Error`
    /// (`artwork_hydration.go:62-66`): the radios come back either way, each with a
    /// zero (unresolved) image — executed with `item_artwork` dropped, where
    /// `GetAll` returned all 4 rows with zero images.
    ///
    /// `options == None` is Go's empty variadic list: `parseRestOptions` returns the
    /// zero `model.QueryOptions` (`sql_restful.go:55-56,66`), which is
    /// [`crate::sql_restful::QueryOptions::default`] — no filter, no page, so the
    /// statement is `SELECT * FROM radio` and row order is SQLite's.
    ///
    /// # Errors
    /// [`RadioError::Sqlite`] when the statement fails or a row will not scan
    /// (including a NULL timestamp — NOT an OFFSET-without-LIMIT failure, which the
    /// builder's `LIMIT -1` deviation makes executable here); [`RadioError::Builder`]
    /// if the builder fails to materialize. Never [`RadioError::NotFound`].
    pub fn get_all(
        &self,
        options: Option<&crate::sql_restful::RestQueryOptions>,
    ) -> Result<crate::artwork_hydration::ReadOutcome<RadioRecord>, RadioError> {
        let statement = get_all_sql(Some(&parse_rest_options(options)))?;
        let mut items =
            crate::execution::query_all(self.conn, &statement, crate::hydrate::row_to_radio)?;
        let mut artwork_error = None;
        crate::artwork_hydration::hydrate_radio_artwork(self.conn, &mut items, &mut artwork_error);
        Ok(crate::artwork_hydration::ReadOutcome {
            items,
            artwork_error,
        })
    }

    /// `CountAll` (`:33-36`) under the caller's already-built
    /// [`crate::sql_restful::QueryOptions`]: the DISTINCT aggregate.
    ///
    /// It deliberately KEEPS the raw option type that [`Self::get_all`] abandoned. Go's
    /// `CountAll` builds its select with `newSelect(ctx)` and NO options
    /// (`radio_repository.go:34`), so a caller sort never even reaches the builder, and
    /// `count` additionally drops the page (`sql_base_repository.go:568`) and installs
    /// `ORDER BY radio.id` (`:569`). It is not an ordering channel and carries no
    /// [`sanitize_sort`]-style leak — the same justification as
    /// [`crate::transcoding_repo::TranscodingRepo::count_all`]. Filters DO apply
    /// (`:571`), which is why they stay in the signature.
    ///
    /// # Errors
    /// [`RadioError::Sqlite`] when the statement fails; [`RadioError::Builder`] if the
    /// builder fails to materialize. Go's `count` returns `res.Count` BESIDE a
    /// `queryOne` error (`sql_base_repository.go:572-574`); the port reports the error
    /// alone and the count then defaults to 0.
    pub fn count_all(
        &self,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<i64, RadioError> {
        let statement = count_all_sql(options)?;
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `Count` (`:112-114`): [`Self::count_all`] driven by rest options through
    /// [`parse_rest_options`] — Go's `CountAll(ctx, parseRestOptions(…))` (`:113`) in
    /// full. The whitelist check happens here even though the aggregate cannot be
    /// reordered by a caller sort; what this entry point really transports is the
    /// `name` contains-filter (`:22-24`).
    ///
    /// # Errors
    /// Same as [`Self::count_all`].
    pub fn count(
        &self,
        options: Option<&crate::sql_restful::RestQueryOptions>,
    ) -> Result<i64, RadioError> {
        self.count_all(Some(&parse_rest_options(options)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_restful::{RestQueryOptions, ANNOTATION_FIELDS};
    use chrono::{NaiveDate, TimeZone};
    use std::collections::BTreeMap;

    /// Deterministic clock: the caller-owned `time.Now()` of
    /// `radio_repository.go:90`, and an earlier stamp for `created_at` (`:92`).
    fn at(year: u32, month: u32, day: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(i32::try_from(year).unwrap(), month, day)
                .unwrap()
                .and_hms_opt(h, mi, s)
                .unwrap(),
        )
    }

    const NOW_STAMP: &str = "2024-01-02 03:04:05+00:00";

    fn now() -> DateTime<Utc> {
        at(2024, 1, 2, 3, 4, 5)
    }

    fn radio(id: &str) -> RadioRecord {
        RadioRecord {
            id: id.to_string(),
            stream_url: "https://example.com:4533/app".to_string(),
            name: "Radio One".to_string(),
            home_page_url: "https://example.com".to_string(),
            uploaded_image: "cover.png".to_string(),
            created_at: at(2024, 1, 1, 2, 4, 5),
            updated_at: at(2024, 1, 1, 2, 4, 5),
            // Second-layer artwork (`structs:"-"`): never a put_cols leg.
            image: crate::artwork_hydration::ItemImage::default(),
        }
    }

    /// Go's real callers pass camelCase names (`server/subsonic/radio.go:122` passes
    /// `"StreamUrl", "HomePageUrl", "Name"`; `server/nativeapi/radios.go:48` passes
    /// `"UploadedImage"`), and `selectUpdateColumns` snake-cases every requested name
    /// (`sql_base_repository.go:596-598`) before matching the write map. Matching them
    /// verbatim would drop all three and update only `updated_at`, so the SET list must
    /// still name every requested column.
    #[test]
    fn put_with_camel_case_cols_to_update_matches_go_column_names() {
        let record = radio("ra-1");
        // Act: the exact call shape of `server/subsonic/radio.go:122`.
        let put = put_sql(&record, now(), &["StreamUrl", "HomePageUrl", "Name"]);
        let (sql, args) = &put.update;
        // Assert: squirrel renders the SET pairs in write-map (struct) order —
        // `radio_repository.go:96` appends `UpdatedAt` last in Go's slice, but the
        // filtered map is iterated over `values`, i.e. the marshaled column order.
        assert_eq!(
            sql.as_str(),
            "UPDATE radio SET stream_url = ?, name = ?, home_page_url = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args.as_slice(),
            [
                Value::Text("https://example.com:4533/app".to_string()),
                Value::Text("Radio One".to_string()),
                Value::Text("https://example.com".to_string()),
                Value::Text(NOW_STAMP.to_string()),
                Value::Text("ra-1".to_string()),
            ]
        );
    }

    /// A camelCase `"UpdatedAt"` from the caller must not produce a second `updated_at`
    /// leg: Go appends its own and the names collapse through
    /// `slice.ToMap` (`sql_base_repository.go:596-598`), so a duplicate is harmless.
    #[test]
    fn cols_to_update_naming_updated_at_camel_case_does_not_duplicate_the_leg() {
        let put = put_sql(&radio("ra-1"), now(), &["UpdatedAt"]);
        assert_eq!(
            put.update.0.as_str(),
            "UPDATE radio SET updated_at = ? WHERE id = ?"
        );
    }

    #[test]
    fn get_pins_the_unfiltered_row_select() {
        // Arrange: Get (radio_repository.go:58-66) — every user sees radios, because
        // there is no userFilter (see the module header).
        // Act:
        let (sql, args) = get_sql("ra-1");
        // Assert:
        assert_eq!(sql, "SELECT * FROM radio WHERE id = ?");
        assert_eq!(args, vec![Value::Text("ra-1".to_string())]);
    }

    #[test]
    fn get_all_applies_parsed_filters_and_pagination() {
        // Arrange: GetAll (:68-71) over a contains-filtered page, the shape
        // rest_repo's "name" mapping produces (containsFilter, :21).
        let mut filters = BTreeMap::new();
        filters.insert("name".to_string(), "one".to_string());
        let rest = RestQueryOptions {
            sort: String::new(),
            order: String::new(),
            max: 10,
            offset: 20,
            filters,
        };
        let options = rest_repo().parse_rest_options(Some(&rest));
        // Act:
        let (sql, args) = get_all_sql(Some(&options)).expect("the radio select materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM radio WHERE name LIKE ? LIMIT 10 OFFSET 20"
        );
        // Assert: the bounds are squirrel literals (`select.go:139-147`), so
        // only the parsed filter binds.
        assert_eq!(args, vec![Value::Text("%one%".to_string())]);
    }

    #[test]
    fn count_all_and_exists_pin_the_count_shapes() {
        // Arrange: CountAll (:43-45) via count() and Exists (:48-50) via exists().
        // Act:
        let (count_sql, count_args) = count_all_sql(None).expect("count materializes");
        let (exist_sql, exist_args) = exists_sql("ra-2");
        // Assert:
        assert_eq!(
            count_sql,
            "SELECT count(distinct radio.id) as count FROM radio ORDER BY radio.id"
        );
        assert!(count_args.is_empty());
        assert_eq!(
            exist_sql,
            "SELECT count(*) as exist FROM radio WHERE id = ?"
        );
        assert_eq!(exist_args, vec![Value::Text("ra-2".to_string())]);
    }

    #[test]
    fn delete_emits_one_statement_per_id_in_order() {
        // Arrange: Delete (:51-57) via deleteByID — the admin gate is the
        // caller's (see module header).
        // Act:
        let stmts = delete_sql(&["ra-1".to_string(), "ra-2".to_string()]);
        // Assert:
        assert_eq!(stmts.len(), 2);
        for (i, (sql, args)) in stmts.iter().enumerate() {
            assert_eq!(sql, "DELETE FROM radio WHERE id = ?");
            assert_eq!(args.as_slice(), [Value::Text(format!("ra-{}", i + 1))]);
        }
    }

    #[test]
    fn put_update_leg_sets_every_column_but_created_at() {
        // Arrange: Put with no colsToUpdate (:90-103) — selectUpdateColumns
        // keeps all, drops created_at (:601-604).
        let record = radio("ra-1");
        // Act:
        let put = put_sql(&record, now(), &[]);
        let (sql, args) = &put.update;
        // Assert:
        assert_eq!(
            sql.as_str(),
            "UPDATE radio SET id = ?, stream_url = ?, name = ?, home_page_url = ?, \
             uploaded_image = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args.as_slice(),
            vec![
                Value::Text("ra-1".to_string()),
                Value::Text("https://example.com:4533/app".to_string()),
                Value::Text("Radio One".to_string()),
                Value::Text("https://example.com".to_string()),
                Value::Text("cover.png".to_string()),
                Value::Text(NOW_STAMP.to_string()),
                Value::Text("ra-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_with_cols_to_update_appends_updated_at_and_keeps_only_requested() {
        // Arrange: Update's column list (:101-103, :135-141): the client's
        // "name" plus the appended UpdatedAt — created_at never overwritten.
        let record = radio("ra-1");
        // Act:
        let put = put_sql(&record, now(), &["name"]);
        let (sql, args) = &put.update;
        // Assert:
        assert_eq!(
            sql.as_str(),
            "UPDATE radio SET name = ?, updated_at = ? WHERE id = ?"
        );
        assert_eq!(
            args.as_slice(),
            [
                Value::Text("Radio One".to_string()),
                Value::Text(NOW_STAMP.to_string()),
                Value::Text("ra-1".to_string()),
            ]
        );
    }

    #[test]
    fn put_fresh_row_mints_the_id_and_stamps_created_at() {
        // Arrange: Put with an empty id (:96-99) — NewRandom + CreatedAt=now.
        // A fresh row's CreatedAt is overwritten by put_sql, so whatever the
        // caller carries is irrelevant (:96-100).
        let mut record = radio("");
        record.created_at = at(2020, 1, 1, 0, 0, 0);
        // Act:
        let put = put_sql(&record, now(), &[]);
        let (sql, args) = &put.insert;
        // Assert:
        assert_eq!(put.id.len(), 22, "id2.NewRandom() is 22 base62 chars");
        assert!(sql.starts_with("INSERT INTO radio (id, stream_url, "));
        assert_eq!(args[0], Value::Text(put.id.clone()));
        assert_eq!(
            args[5],
            Value::Text(NOW_STAMP.to_string()),
            "created_at = now"
        );
        assert_eq!(
            args[6],
            Value::Text(NOW_STAMP.to_string()),
            "updated_at = now"
        );
        // The UPDATE leg still targets the minted id (Go's put always tries it).
        assert!(put.update.0.ends_with("WHERE id = ?"));
        assert_eq!(put.update.1.last(), Some(&Value::Text(put.id.clone())));
    }

    #[test]
    fn put_enqueues_artwork_resolution_at_bump_priority() {
        // Arrange: the never-failing enqueue (:107-113), KindRadioArtwork "ra"
        // and ArtworkPriorityBump 100.
        // Act:
        let (sql, args) = enqueue_artwork_sql("ra-9", &now());
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO artwork_queue (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) \
             ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET \
             priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, \
             attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("ra".to_string()),
                Value::Text("ra-9".to_string()),
                Value::Text("primary".to_string()),
                Value::Integer(100),
                Value::Text(NOW_STAMP.to_string()),
                Value::Text(NOW_STAMP.to_string()),
            ]
        );
    }

    #[test]
    fn rest_repo_whitelists_radio_fields_plus_annotations() {
        // Arrange: registerModel's whitelist (:22, sql_restful.go:142-163).
        let repo = rest_repo();
        // Act:
        let (kept, _) = repo.sanitize_sort("Name", "asc");
        let (dropped, _) = repo.sanitize_sort("Bogus", "asc");
        // Assert:
        assert_eq!(kept, "name".to_string());
        assert_eq!(dropped, String::new());
        for field in ANNOTATION_FIELDS {
            let (sort, _) = repo.sanitize_sort(field, "");
            assert_ne!(sort, String::new(), "{field} must be whitelisted");
        }
    }

    #[test]
    fn rest_repo_name_filter_is_a_contains_match() {
        // Arrange: containsFilter("name") (:21).
        let repo = rest_repo();
        let mut filters = BTreeMap::new();
        filters.insert("name".to_string(), "jazz".to_string());
        // Act:
        let options = repo.parse_rest_options(Some(&RestQueryOptions {
            filters,
            ..RestQueryOptions::default()
        }));
        // Assert:
        let (sql, args) = options.where_clause().expect("the filter renders");
        assert_eq!(sql, "name LIKE ?");
        assert_eq!(args, vec![Value::Text("%jazz%".to_string())]);
    }
}
