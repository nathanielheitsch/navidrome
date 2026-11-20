//! Port of `persistence/artwork_queue_repository.go` (235L) — the pending-work
//! queue (`artwork_queue`) for artwork resolution.
//!
//! Conventions as elsewhere: statements return `(sql, args)`. Go's shared
//! column vector (`enqueueColumns`, `:22`) and chunk cap (`enqueueChunkSize`,
//! `:18`) become [`ENQUEUE_COLUMNS`]/[`CHUNK_SIZE`]; the conflict suffixes
//! (Go's raw strings) are [`CONFLICT_ENQUEUE`], [`CONFLICT_PRESERVE_BACKOFF`]
//! and [`SKIP_IF_QUEUED`] data.
//!
//! Time: Go stamps `retry_at`/`enqueued_at` with `time.Now()` and reads them
//! back with `LtOrEq{"retry_at": time.Now()}` (`:167`), so the port takes
//! [`chrono::DateTime<Utc>]` from the caller and binds it through
//! [`crate::time`]. Epoch integers would break exactly that read: INTEGER rows
//! never satisfy a TEXT-bound `retry_at <= ?` and sort first under
//! `ORDER BY enqueued_at ASC` (see `time.rs`).
//!
//! The source-filter semantics of `artworkSourceFilter` (`:107-121`) are
//! reproduced: dropping `ArtworkSourceFailed` from the requested sources
//! decides between `source IN (…)`, the absent-state arm, or an OR of both.
//! Squirrel renders that arm as `(hash = ? AND last_failure <> ?)` with both
//! values bound (`equalOpr = "<>"`, `squirrel@v1.5.4/expr.go:155`) — not as a
//! literal `!=`.
//!
//! Kind spelling (S-D3c): every `item_kind` value in this table is Go's
//! `Kind.Prefix()` — `Get` binds it (`:38`), `EnqueueAllMissing` binds it twice
//! (`:66-67`), `artworkSourceFilter` binds it (`:108`), and the queue filter uses
//! `model.KindPrefixes(kinds)` (`:201`, `model/artwork_id.go:44-46`). The kind
//! NAME (`"album"`, `"media_file"`) is never stored, so callers pass
//! `nd_id::ARTWORK_KIND_*_PREFIX` and the fixtures below use `"al"`/`"mf"`.

use chrono::{DateTime, Utc};

use crate::runtime_error::DbOperationError;
use crate::sqlrepo::{Repository, Value};
use crate::time;

/// A materialized statement: `(sql, args)`, the `toSQL()` output shape.
pub type Sql = (String, Vec<Value>);

/// The queue table (`:31`).
pub const TABLE: &str = "artwork_queue";

/// `itemArtworkTable` (`artwork_repository.go:15`).
pub const ITEMS_TABLE: &str = "item_artwork";

/// `enqueueChunkSize` (`:18`): rows per multi-row insert, keeping each insert
/// under SQLite's bind-variable limit (Go's comment: at most 7 vars per row).
pub const CHUNK_SIZE: usize = 100;

/// `enqueueColumns` (`:22`) — the columns every insert writes, in this order;
/// the INSERT…SELECT forms project them to match, and `DequeueBatch` selects
/// exactly these.
pub const ENQUEUE_COLUMNS: [&str; 7] = [
    "item_kind",
    "item_id",
    "image_type",
    "priority",
    "attempts",
    "retry_at",
    "enqueued_at",
];

/// `model.ImageTypePrimary` (`model/artwork.go`).
pub const IMAGE_TYPE_PRIMARY: &str = "primary";

/// `model.ArtworkSourceFailed` (`model/artwork.go`): the pseudo-source for
/// absent states that exhausted the retry budget.
pub const SOURCE_FAILED: &str = "!failed";

/// `Enqueue`'s conflict suffix (`:48-51`): keep the higher priority, reset the
/// lifecycle (`attempts = 0`, fresh `enqueued_at`, empty trace).
pub const CONFLICT_ENQUEUE: &str =
    "ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET priority = MAX(priority, excluded.priority), retry_at = excluded.retry_at, attempts = 0, enqueued_at = excluded.enqueued_at, trace = '[]'";

/// `EnqueuePreservingBackoff`'s suffix (`:54-56`): an already-queued row keeps
/// its `retry_at`, so a read-through never resets a failed backoff.
pub const CONFLICT_PRESERVE_BACKOFF: &str =
    "ON CONFLICT (item_kind, item_id, image_type) DO UPDATE SET priority = MAX(priority, excluded.priority)";

/// `skipIfQueued` (`:96`): DO NOTHING is deliberate — a recheck must not bump
/// priority or retry_at of an already-queued item (Go's comment `:95`).
pub const SKIP_IF_QUEUED: &str = " ON CONFLICT (item_kind, item_id, image_type) DO NOTHING";

/// `model.ArtworkQueueItem` (`model/artwork.go:19+`) as the row shape the
/// queue writes; `image_type` empty means [`IMAGE_TYPE_PRIMARY`] (`cmp.Or`,
/// `:77`, `:155`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtworkQueueItem {
    /// `Kind.Prefix()` typed-domain text.
    /// `Kind.Prefix()` typed-domain text.
    pub item_kind: String,
    /// The entity's id.
    pub item_id: String,
    /// Empty means [`IMAGE_TYPE_PRIMARY`] (Go's `cmp.Or`, `:77`/`:155`).
    pub image_type: String,
    /// Queue priority.
    pub priority: i64,
    /// Always 0 on enqueue (`:155`); the row's attempt counter.
    pub attempts: i64,
    /// Eligibility bound (`retry_at <= now`), stored as datetime TEXT.
    pub retry_at: Option<DateTime<Utc>>,
    /// Lifecycle start stamp, Go's `time.Now()`, stored as datetime TEXT.
    pub enqueued_at: Option<DateTime<Utc>>,
    /// JSON walk trace (`Enqueue` resets it to `'[]'`, `:50`).
    pub trace: String,
}

/// `Get` (`:35-43`): `Select("*").Where(Eq{item_kind, item_id, image_type})`.
#[must_use]
pub fn get_sql(kind: &str, id: &str, image_type: &str) -> Sql {
    Repository::new(TABLE)
        .select(vec!["*"])
        .where_eq("item_kind", kind)
        .where_eq("item_id", id)
        .where_eq("image_type", image_type)
        .to_sql()
        .unwrap()
}

/// `enqueue` (`:150-163`) with either suffix: one chunked multi-row insert
/// per [`CHUNK_SIZE`] items, values `(kind, id, image_type, priority, 0,
/// now, now)` (`:155`).
#[must_use]
pub fn enqueue_sql(
    items: &[ArtworkQueueItem],
    conflict: &'static str,
    now: &DateTime<Utc>,
) -> Vec<Sql> {
    items
        .chunks(CHUNK_SIZE)
        .map(|chunk| {
            let rows = chunk
                .iter()
                .map(|_| "(?, ?, ?, ?, ?, ?, ?)")
                .collect::<Vec<_>>()
                .join(", ");
            let args = chunk
                .iter()
                .flat_map(|it| {
                    let image_type = if it.image_type.is_empty() {
                        IMAGE_TYPE_PRIMARY.to_string()
                    } else {
                        it.image_type.clone()
                    };
                    vec![
                        Value::Text(it.item_kind.clone()),
                        Value::Text(it.item_id.clone()),
                        Value::Text(image_type.clone()),
                        Value::Integer(it.priority),
                        Value::Integer(0),
                        time::value(now),
                        time::value(now),
                    ]
                })
                .collect::<Vec<_>>();
            let cols = ENQUEUE_COLUMNS
                .iter()
                .map(|c| (*c).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("INSERT INTO {TABLE} ({cols}) VALUES {rows} {conflict}"),
                args,
            )
        })
        .collect::<Vec<_>>()
}

/// `Enqueue` (`:47-52`).
#[must_use]
pub fn enqueue_sql_reset(items: &[ArtworkQueueItem], now: &DateTime<Utc>) -> Vec<Sql> {
    enqueue_sql(items, CONFLICT_ENQUEUE, now)
}

/// `EnqueuePreservingBackoff` (`:53-57`).
#[must_use]
pub fn enqueue_sql_preserving_backoff(items: &[ArtworkQueueItem], now: &DateTime<Utc>) -> Vec<Sql> {
    enqueue_sql(items, CONFLICT_PRESERVE_BACKOFF, now)
}

/// `EnqueueAllMissing` (`:58-68`): the entity table comes from the caller's
/// `artworkOwnerTables` lookup (fixed schema identifier, Go errors for an
/// unknown kind — the port takes the resolved name, `Option` left to the
/// caller's table). `SELECT ?, id, ?, ?, 0, ?, ? FROM <entity> WHERE id NOT
/// IN (SELECT item_id FROM item_artwork WHERE item_kind = ?)` — args in
/// dbx order (`:66-68`).
#[must_use]
pub fn enqueue_all_missing_sql(
    kind: &str,
    entity_table: &str,
    priority: i64,
    now: &DateTime<Utc>,
) -> Sql {
    let cols = ENQUEUE_COLUMNS
        .iter()
        .map(|c| (*c).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let stmt = format!(
        "INSERT INTO {TABLE} ({cols}) SELECT ?, id, ?, ?, 0, ?, ? FROM {entity_table} WHERE id NOT IN (SELECT item_id FROM {ITEMS_TABLE} WHERE item_kind = ?){SKIP_IF_QUEUED}"
    );
    (
        stmt,
        vec![
            Value::Text(kind.to_string()),
            Value::Text(IMAGE_TYPE_PRIMARY.to_string()),
            Value::Integer(priority),
            time::value(now),
            time::value(now),
            Value::Text(kind.to_string()),
        ],
    )
}

/// `EnqueueIfMissing` (`:70-93`): chunked CTE inserts. Go composes the statement in
/// `insertIfNotQueued` (`:99-102`), NOT in the loop:
/// `Expr(with + "INSERT INTO " + r.tableName + " (" + join(enqueueColumns, ", ") + ") "
/// + sql + skipIfQueued, args...)`, where `with` is the CTE prefix (`:81`) and `sql` is
/// the SELECT (`:82-87`). The `INSERT INTO artwork_queue (<enqueueColumns>)` leg therefore
/// sits BETWEEN the CTE and the SELECT. This renderer previously omitted it, producing a
/// `WITH … SELECT … ON CONFLICT` statement that SQLite rejects outright at PREPARE
/// (`near "ON": syntax error`), so the executed wrapper could never insert a row.
///
/// Render of that exact composite through pinned squirrel v1.5.4 (`Expr` passes the string
/// through verbatim, `expr.go:53-66`; `/tmp/sqprobe/main.go` mirrors `:99-102`):
/// ```text
/// WITH new_items(item_kind, item_id, image_type, priority) AS (VALUES (?,?,?,?),(?,?,?,?))
/// INSERT INTO artwork_queue (item_kind, item_id, image_type, priority, attempts, retry_at, enqueued_at)
/// SELECT n.item_kind, n.item_id, n.image_type, n.priority, 0, ?, ?
/// FROM new_items n WHERE NOT EXISTS (SELECT 1 FROM item_artwork ia
///   WHERE ia.item_kind = n.item_kind AND ia.item_id = n.item_id AND ia.image_type = n.image_type)
/// ON CONFLICT (item_kind, item_id, image_type) DO NOTHING
/// ARGS: [al x primary 3 al y primary 3 now now]
/// ```
#[must_use]
pub fn enqueue_if_missing_sql(items: &[ArtworkQueueItem], now: &DateTime<Utc>) -> Vec<Sql> {
    items.chunks(CHUNK_SIZE)
        .map(|chunk| {
            let rows = chunk.iter().map(|_| "(?,?,?,?)")
                .collect::<Vec<_>>().join(", ");
            let args = chunk.iter()
                .flat_map(|it| {
                    let image_type =
                        if it.image_type.is_empty() {
                            IMAGE_TYPE_PRIMARY.to_string()
                        } else {
                            it.image_type.clone()
                        };
                    vec![
                        Value::Text(it.item_kind.clone()),
                        Value::Text(it.item_id.clone()),
                        Value::Text(image_type.clone()),
                        Value::Integer(it.priority),
                    ]
                })
                .collect::<Vec<_>>();
            let mut args_all = args;
            args_all.push(time::value(now));
            args_all.push(time::value(now));
            let cols = ENQUEUE_COLUMNS
                .iter()
                .map(|c| (*c).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let select = format!(
                "INSERT INTO {TABLE} ({cols}) \
                 SELECT n.item_kind, n.item_id, n.image_type, n.priority, 0, ?, ? \
                 FROM new_items n WHERE NOT EXISTS (SELECT 1 FROM item_artwork ia \
                 WHERE ia.item_kind = n.item_kind AND ia.item_id = n.item_id \
                 AND ia.image_type = n.image_type)"
            );
            (
                format!(
                    "WITH new_items(item_kind, item_id, image_type, priority) AS (VALUES {rows}) {select}{SKIP_IF_QUEUED}"
                ),
                args_all,
            )
        })
        .collect::<Vec<_>>()
}

/// `artworkSourceFilter` (`:107-121`): kind Eq plus, when sources were given,
/// the OR of the stored-source IN and the absent/gave-up arm. Returned as
/// `(sql, args)` fragments for the count/queue paths below.
///
/// **Fixed in S-D3g2 — a row-set defect, not a text one.** Go builds
/// `append(f, match)` over `And{Eq{item_kind}}` (`:108`, `:120`), so squirrel's
/// `conj.join` wraps the whole conjunction in one group and the `Or` member in a
/// second (`squirrel@v1.5.4/expr.go:366-386`; `conj.join` wraps even a
/// ONE-element conjunction, which is why the empty-sources arm is also
/// parenthesized in Go). Executed squirrel render (standalone program
/// `/tmp/sd3g2_render`, saved `/tmp/g2_source_filter_render.txt`):
/// ```text
/// sources=[]                     (item_kind = ?)
/// sources=[cover]                (item_kind = ? AND (source IN (?)))
/// sources=[!failed]              (item_kind = ? AND ((hash = ? AND last_failure <> ?)))
/// sources=[cover,!failed]        (item_kind = ? AND (source IN (?) OR (hash = ? AND last_failure <> ?)))
/// sources=[embedded,cover]       (item_kind = ? AND (source IN (?,?)))
/// ```
/// Args are `[kind, <stored…>, "", ""]`: `Eq{"hash": ""}` and
/// `NotEq{"last_failure": ""}` both BIND the empty string instead of inlining it.
/// The previous form emitted `item_kind = ? AND <or-body>`, and the seam joins
/// clauses with a bare `" AND "`, so SQL precedence gave
/// `(item_kind = ? AND source IN (…)) OR (hash = '' AND last_failure <> '')` — the
/// kind stopped restricting the absent/gave-up arm.
///
/// Executed before/after over ONE identical `item_artwork` fixture (5 rows across
/// kinds `al`/`mf`/`ra`: `al/a1` source=cover + failure, `al/a2` source=cover +
/// hash, `mf/m1` absent source + failure, `mf/m2` embedded + hash, `ra/r1` absent
/// source + failure); oracle `/tmp/sd3g2_paren`, saved
/// `/tmp/g2_paren_proof.txt`:
///
/// | predicate                             | al | mf | ra |
/// |---------------------------------------|----|----|----|
/// | Go's `CountBySource(kind,[cover,!failed])` | **2** | **1** | **1** |
/// | flat `kind AND (IN) OR (hash…)`       | 4  | 3  | 3  |
/// | wrapped `(kind AND ((IN) OR (hash…)))` | 2  | 1  | 1  |
///
/// The flat form returned `al: [a1 a2 m1 r1]`, `mf: [a1 m1 r1]`, `ra: [a1 m1 r1]`
/// — rows of OTHER kinds counted. With no sources there is no OR arm, so both
/// forms agree (`al: [a1 a2]`); only the multi-arm case changes.
///
/// `hash` is a `TEXT NOT NULL DEFAULT ''` column and `last_failure` a
/// `jsonb NOT NULL DEFAULT '[]'` one (`schema_ddl.rs:51`), so Go's `<> ''`
/// comparison runs against JSON text — `'[]'` (resolved) is unequal to `''` and
/// comparison runs against JSON text: `'[]'` (Go's empty JSON array) and
/// `'["boom"]'` both differ from `''`. This arm checks stored hash emptiness plus
/// last_failure's non-empty SQL text, NOT whether its decoded JSON list has items.
#[must_use]
pub fn source_filter_sql(kind: &str, sources: &[String]) -> Sql {
    if sources.is_empty() {
        // Go returns the one-element And, whose conj.join STILL wraps it:
        // `(item_kind = ?)` (executed render, /tmp/g2_source_filter_render.txt;
        // executed repository trace in /tmp/g2_folder_queue_trace.txt).
        return (
            "(item_kind = ?)".to_string(),
            vec![Value::Text(kind.to_string())],
        );
    }
    let stored = sources
        .iter()
        .filter(|s| **s != SOURCE_FAILED)
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();
    let mut arms: Vec<String> = Vec::new();
    let mut args = Vec::new();
    if !stored.is_empty() {
        let placeholders = stored.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        // squirrel Eq with a slice renders the bare `source IN (?, ?)` part; the
        // wrapping group comes from the Or's conj.join below.
        arms.push(format!("source IN ({placeholders})"));
        args.extend(stored.iter().map(|s| Value::Text(s.clone())));
    }
    if stored.len() != sources.len() {
        // Go's `And{Eq{"hash": ""}, NotEq{"last_failure": ""}}` inside the Or:
        // Eq renders `hash = ?` with a bound empty string, NotEq `last_failure
        // <> ''`, and the conjunction is wrapped.
        arms.push("(hash = ? AND last_failure <> ?)".to_string());
        args.push(Value::Text(String::new()));
        args.push(Value::Text(String::new()));
    }
    args.insert(0, Value::Text(kind.to_string()));
    // The And's conj.join wraps the whole conjunction once; its Or member is
    // itself a conj, so conj.join wraps it too (arms.len() > 1 renders
    // `(a OR b)` from the inner join; len == 1 renders `(a)` — Go's single-arm
    // Or is still a conj and still gets its group, which the
    // `[cover]`/`[!failed]` renders above confirm).
    let or_group = format!("({})", arms.join(" OR "));
    (format!("(item_kind = ? AND {or_group})"), args)
}

/// `CountBySource` (`:123-128`): `Select("count(*) as count")` over
/// `item_artwork` with the source filter, read through Go's `count` alias
/// (`:124-127`). Go returns `(res.Count, err)` and its caller reads the number
/// beside the error (`core/artwork/queue.go`); the port reports `0` beside `Err`,
/// which is Go's zero `res.Count`.
///
/// Executed Go (oracle `/tmp/sd3g2_paren`, `/tmp/g2_paren_proof.txt`): with
/// `[cover]` and `[cover, !failed]` both asking kind `al` it returned **2**, for
/// kind `mf` **1**, and for `[!failed]` alone **1**. See [`source_filter_sql`]
/// for the parenthesization proof that makes those numbers reachable.
pub fn count_by_source_sql(kind: &str, sources: &[String]) -> Sql {
    let (where_sql, args) = source_filter_sql(kind, sources);
    (
        format!("SELECT count(*) as count FROM {ITEMS_TABLE} WHERE {where_sql}"),
        args,
    )
}

/// `SourcesInUse` (`:130-138`): `Select("distinct source")` over
/// `item_artwork WHERE item_kind = ?`.
#[must_use]
pub fn sources_in_use_sql(kind: &str) -> Sql {
    (
        format!("SELECT distinct source FROM {ITEMS_TABLE} WHERE item_kind = ?"),
        vec![Value::Text(kind.to_string())],
    )
}

/// `EnqueueBySource` (`:142-148`): INSERT…SELECT over the source-filtered
/// items with literal `(?, priority, 0, ?, ?)` columns (`:145-146`);
/// deliberately leaves `item_artwork` alone (Go's comment `:140-141`).
#[must_use]
pub fn enqueue_by_source_sql(
    kind: &str,
    sources: &[String],
    priority: i64,
    now: &DateTime<Utc>,
) -> Sql {
    let cols = ENQUEUE_COLUMNS
        .iter()
        .map(|c| (*c).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let (where_sql, mut args) = source_filter_sql(kind, sources);
    let select = format!(
        "SELECT item_kind, item_id, image_type, ?, 0, ?, ? FROM {ITEMS_TABLE} WHERE {where_sql}"
    );
    // squirrel renders Insert().Columns().Select() with the select's own bound
    // args; the INSERT-side args (priority/now/now) precede WHERE's.
    args.insert(0, Value::Integer(priority));
    args.insert(1, time::value(now));
    args.insert(2, time::value(now));
    (
        format!("INSERT INTO {TABLE} ({cols}) {select}{SKIP_IF_QUEUED}"),
        args,
    )
}

/// `DequeueBatch` (`:166-176`): `Select(enqueueColumns).Where(LtOrEq{retry_at})`
/// (+ optional `Eq{item_kind: kinds}`) `.OrderBy("priority DESC",
/// "enqueued_at ASC").Limit(n)`. `now` is the eligibility bound Go binds as
/// `time.Now()` (`:167`), so it must be the same TEXT shape the enqueue wrote;
/// `kinds` are caller-supplied typed prefixes (`:170-172`).
#[must_use]
pub fn dequeue_batch_sql(n: i64, now: &DateTime<Utc>, kinds: &[String]) -> Sql {
    let cols = ENQUEUE_COLUMNS
        .iter()
        .map(|c| (*c).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let mut args = vec![time::value(now)];
    args.extend(kinds.iter().map(|k| Value::Text(k.clone())));
    let where_sql = if kinds.is_empty() {
        "retry_at <= ?".to_string()
    } else {
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        format!("retry_at <= ? AND item_kind IN ({placeholders})")
    };
    (
        format!(
            "SELECT {cols} FROM {TABLE} WHERE {where_sql} ORDER BY priority DESC, enqueued_at ASC LIMIT {n}"
        ),
        args,
    )
}

/// `MarkFailedIfUnchanged` (`:179-186`): the +1 attempts / retry_at / trace
/// UPDATE gated on `retry_at = seenRetryAt` — a concurrent re-enqueue keeps
/// its own eligibility (Go's comment `:177-178`).
#[must_use]
pub fn mark_failed_if_unchanged_sql(
    kind: &str,
    id: &str,
    image_type: &str,
    seen_retry_at: &DateTime<Utc>,
    retry_at: &DateTime<Utc>,
    trace: &str,
) -> Sql {
    (
        format!(
            "UPDATE {TABLE} SET attempts = attempts + 1, retry_at = ?, trace = ? WHERE item_kind = ? AND item_id = ? AND image_type = ? AND retry_at = ?"
        ),
        vec![
            time::value(retry_at),
            Value::Text(trace.to_string()),
            Value::Text(kind.to_string()), Value::Text(id.to_string()),
            Value::Text(image_type.to_string()),
            time::value(seen_retry_at),
        ],
    )
}

/// `DeleteIfUnchanged` (`:188-190`): the four-key delete, `retry_at` included.
#[must_use]
pub fn delete_if_unchanged_sql(
    kind: &str,
    id: &str,
    image_type: &str,
    retry_at: &DateTime<Utc>,
) -> Sql {
    (
        format!(
            "DELETE FROM {TABLE} WHERE item_kind = ? AND item_id = ? AND image_type = ? AND retry_at = ?"
        ),
        vec![
            Value::Text(kind.to_string()), Value::Text(id.to_string()),
            Value::Text(image_type.to_string()),
            time::value(retry_at),
        ],
    )
}

/// `artworkQueueFilter` (`:198-207`): shared by `CountQueued` and `PurgeQueued`
/// so a preview never counts rows the delete misses. Empty filter means every
/// row (`None`).
#[must_use]
pub fn artwork_queue_filter_sql(kinds: &[String], priorities: &[i64]) -> Option<Sql> {
    if kinds.is_empty() && priorities.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if !kinds.is_empty() {
        let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        parts.push(format!("item_kind IN ({placeholders})"));
        args.extend(kinds.iter().map(|k| Value::Text(k.clone())));
    }
    if !priorities.is_empty() {
        let placeholders = priorities
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("priority IN ({placeholders})"));
        args.extend(priorities.iter().map(|p| Value::Integer(*p)));
    }
    Some((parts.join(" AND "), args))
}

/// `CountQueued` (`:210-219`): grouped counts, `ORDER BY item_kind,
/// priority desc`; the filter sits between FROM and GROUP BY (squirrel order
/// WHERE→GROUP BY→ORDER, `sqlrepo.to_sql`).
#[must_use]
pub fn count_queued_sql(kinds: &[String], priorities: &[i64]) -> Sql {
    let sq = Repository::new(TABLE)
        .select(vec!["item_kind", "priority", "count(*) as count"])
        .group_by(vec!["item_kind", "priority"])
        .order_by(vec!["item_kind", "priority desc"]);
    let sq = match artwork_queue_filter_sql(kinds, priorities) {
        None => sq,
        Some((where_sql, args)) => sq.where_raw(where_sql, args),
    };
    let (sql, args) = sq.to_sql().unwrap();
    (sql, args)
}

/// `PurgeQueued` (`:221-227`): the same filter; no conditions keeps SQLite's
/// truncate path (Go's comment `:197`).
#[must_use]
pub fn purge_queued_sql(kinds: &[String], priorities: &[i64]) -> Sql {
    match artwork_queue_filter_sql(kinds, priorities) {
        None => (format!("DELETE FROM {TABLE}"), Vec::new()),
        Some((where_sql, args)) => (format!("DELETE FROM {TABLE} WHERE {where_sql}"), args),
    }
}

/// `purgeDangling` against the queue table (`:192-194`): rows whose entity is
/// gone, per owning table (same shape as the artwork slice's
/// `purge_dangling_sql`, table swapped).
#[must_use]
pub fn purge_dangling_sql(kind: &str, entity_table: &str) -> Sql {
    (
        format!(
            "DELETE FROM {TABLE} WHERE item_kind = ? AND item_id NOT IN (SELECT id FROM {entity_table})"
        ),
        vec![Value::Text(kind.to_string())],
    )
}

/// One row of `CountQueued` (`:214-218`): Go's `model.ArtworkQueueStat`
/// (`model/artwork.go:178-182`), read by field name through `ScanStruct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkQueueStat {
    /// `item_kind` — the stored `Kind.Prefix()`.
    pub item_kind: String,
    /// `priority` (Go `int`).
    pub priority: i64,
    /// `count(*) as count` (Go `int64`).
    pub count: i64,
}

/// Errors of the executed [`ArtworkQueueRepo`] reads.
#[derive(Debug, thiserror::Error)]
pub enum ArtworkQueueError {
    /// The statement failed or a row could not be scanned. `artwork_queue` has no
    /// `PostScan` hook, so this is a statement/scan failure. Go accepts non-TEXT
    /// storage classes for string destinations (`RowGet::get_text` converts INTEGER,
    /// REAL and BLOB); NULL still fails for non-pointer fields. The schema normally
    /// marks these columns NOT NULL, but malformed/custom SQLite rows and driver
    /// conversion failures can still exercise this arm.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`),
    /// which `queryOne` maps from `sql.ErrNoRows`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`).
    ///
    /// Only [`ArtworkQueueRepo::get`] reports it. Executed (oracle
    /// `/tmp/sd3g2_oracle9`, saved `/tmp/g2_folder_queue_out.txt`):
    /// `Get(al, "a2", primary)` on a row that exists only in `item_artwork` and
    /// `Get(al, "zzz", primary)` both returned `err="data not found"`.
    #[error("data not found")]
    NotFound,
}

/// `artworkQueueRepository`'s executed read side
/// (`persistence/artwork_queue_repository.go:35-45`, `:123-138`, `:210-231`):
/// statements from the builders above, run through [`crate::execution`].
///
/// Go's repository has no ctx-derived SQL clause here (no `applyLibraryFilter`,
/// no logged user), so no context-shaped argument appears: the kind is the
/// caller's `Kind.Prefix()` (`model/artwork_id.go:23-25`) and the priorities are
/// plain ints.
pub struct ArtworkQueueRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> ArtworkQueueRepo<'a> {
    /// `newArtworkQueueRepository` (`:29-33`): bind to `conn` (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`:35-43`): `Select("*")` over the three-part primary key.
    ///
    /// Go returns `nil, err` on a miss and `&res` on a hit (`:39-43`), so the
    /// port's `Err(NotFound)`/`Ok(item)` split matches. `retry_at`/`enqueued_at`
    /// are `NOT NULL` in the DDL, and Go reads them as non-pointer `time.Time`, so
    /// they hydrate through [`crate::hydrate::RowGet::get_dt`].
    ///
    /// # Errors
    /// [`ArtworkQueueError::NotFound`] when the triple matches nothing;
    /// [`ArtworkQueueError::Sqlite`] on a statement or scan failure.
    pub fn get(
        &self,
        kind: &str,
        id: &str,
        image_type: &str,
    ) -> Result<ArtworkQueueItem, ArtworkQueueError> {
        let statement = get_sql(kind, id, image_type);
        use crate::hydrate::RowGet;
        let row = crate::execution::query_optional(self.conn, &statement, |row| {
            Ok(ArtworkQueueItem {
                item_kind: row.get_text("item_kind")?,
                item_id: row.get_text("item_id")?,
                image_type: row.get_text("image_type")?,
                priority: row.get_i64("priority")?,
                attempts: row.get_i64("attempts")?,
                // Go's non-pointer `time.Time` (`model/artwork.go:104-105`): a
                // NULL cell is a SCAN failure, not an absent value, so the `?`
                // propagates and the Option only encodes "present".
                retry_at: Some(row.get_dt("retry_at")?),
                enqueued_at: Some(row.get_dt("enqueued_at")?),
                trace: row.get_text("trace")?,
            })
        })?;
        row.ok_or(ArtworkQueueError::NotFound)
    }

    /// `CountBySource` (`:123-128`): `count(*) as count` over `item_artwork` with
    /// the source filter, read by Go's `count` alias.
    ///
    /// Go returns `(res.Count, err)` — a caller may read the number beside the
    /// error (`cmd/artwork.go:191-198`); the port reports `0` beside `Err`, which
    /// is Go's zero `res.Count`. Executed Go values for the fixture in
    /// `tests/runtime_hydrate_folder_artwork_queue.rs`: `al` nil-sources 2,
    /// `al [cover]` 2, `al [!failed]` 1, `al [cover,!failed]` 2,
    /// `mf [cover,!failed]` 1, `al [embedded,cover]` 2
    /// (`/tmp/g2_paren_proof.txt`).
    ///
    /// # Errors
    /// [`ArtworkQueueError::Sqlite`] on a statement failure.
    pub fn count_by_source(
        &self,
        kind: &str,
        sources: &[String],
    ) -> Result<i64, ArtworkQueueError> {
        let statement = count_by_source_sql(kind, sources);
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| {
                row.get::<_, i64>("count")
            })?
            .unwrap_or_default(),
        )
    }

    /// `SourcesInUse` (`:130-138`): `distinct source` for the kind, in SQLite's
    /// returned order (Go adds no ORDER BY, so the order is not contractual).
    ///
    /// # Errors
    /// [`ArtworkQueueError::Sqlite`] on a statement or scan failure.
    pub fn sources_in_use(&self, kind: &str) -> Result<Vec<String>, ArtworkQueueError> {
        let statement = sources_in_use_sql(kind);
        Ok(crate::execution::query_all(self.conn, &statement, |row| {
            crate::hydrate::RowGet::get_text(row, "source")
        })?)
    }

    /// `CountQueued` (`:210-219`): grouped counts under [`artwork_queue_filter_sql`],
    /// ordered `item_kind, priority desc`. The filter is deliberately SHARED with
    /// [`Self::purge_queued`] (Go's comment `:209`), so a preview cannot count rows
    /// the delete misses.
    ///
    /// Go's `queryAll` leaves a mid-page scan failure as `(partial, err)`
    /// (`:217-218` returns `res, err`); the port drops the prefix on `Err`, the
    /// same decision as `folder_repo::FolderRepo::get_all`.
    ///
    /// # Errors
    /// [`ArtworkQueueError::Sqlite`] on a statement or scan failure. A zero-row
    /// result is an empty `Vec`, never `NotFound` (`dbx`'s `All` makes an empty
    /// non-nil slice, `dbx@v1.12.0/query.go:278-291`).
    pub fn count_queued(
        &self,
        kinds: &[String],
        priorities: &[i64],
    ) -> Result<Vec<ArtworkQueueStat>, ArtworkQueueError> {
        use crate::hydrate::RowGet;
        let statement = count_queued_sql(kinds, priorities);
        Ok(crate::execution::query_all(self.conn, &statement, |row| {
            Ok(ArtworkQueueStat {
                item_kind: row.get_text("item_kind")?,
                priority: row.get_i64("priority")?,
                count: row.get_i64("count")?,
            })
        })?)
    }

    /// `Count` (`:229-233`): `count(*) as count` over the whole queue.
    ///
    /// # Errors
    /// [`ArtworkQueueError::Sqlite`] on a statement failure.
    pub fn count(&self) -> Result<i64, ArtworkQueueError> {
        let statement = (format!("SELECT count(*) as count FROM {TABLE}"), Vec::new());
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| {
                row.get::<_, i64>("count")
            })?
            .unwrap_or_default(),
        )
    }

    /// `PurgeQueued` (`:221-227`): the DELETE sharing [`artwork_queue_filter_sql`],
    /// returning `executeSQL`'s `RowsAffected` (`sql_base_repository.go:336-354`).
    ///
    /// Included because `CountQueued`'s contract is the shared filter: the
    /// executed pair (count, then purge, then re-count) is the only way to prove
    /// a preview cannot over- or under-report the delete. An empty filter deletes
    /// everything, which is Go's intent (`:196-197`), and is asserted rather than
    /// assumed.
    ///
    /// # Errors
    /// [`ArtworkQueueError::Sqlite`] on a statement failure.
    pub fn purge_queued(
        &self,
        kinds: &[String],
        priorities: &[i64],
    ) -> Result<i64, ArtworkQueueError> {
        let statement = purge_queued_sql(kinds, priorities);
        Ok(crate::execution::execute(self.conn, &statement)? as i64)
    }
}

/// Which conflict arm an executed enqueue uses — the ONE behavioural difference
/// between Go's two upserting methods (`artwork_queue_repository.go:45-57`). The
/// variants carry the existing rendered suffixes verbatim; no third arm is invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueMode {
    /// `Enqueue` (`:47-52`): a FRESH lifecycle. On conflict the row keeps the higher
    /// priority but its lifecycle is reset — `attempts = 0`, `retry_at =` and
    /// `enqueued_at =` the incoming `now`, `trace = '[]'` — so a new request does not
    /// inherit a spent retry budget or a prior failure in explain (Go's comment
    /// `:45-46`). Writers: `core/artwork/housekeeping.go:169`,
    /// `scanner/image_changes.go:57`, `persistence/playlist_repository.go:368`,
    /// `persistence/radio_repository.go:106`.
    Reset,
    /// `EnqueuePreservingBackoff` (`:53-57`): ONLY `priority = MAX(priority,
    /// excluded.priority)`. An already-queued row keeps its `retry_at`, `attempts`,
    /// `enqueued_at` and `trace`, so a read-through never resets a failed backoff
    /// (Go's comment `:52-53`; the writer is `core/artwork/artwork.go:345`).
    PreserveBackoff,
}

impl EnqueueMode {
    /// The conflict suffix this mode executes: [`CONFLICT_ENQUEUE`] or
    /// [`CONFLICT_PRESERVE_BACKOFF`], never a new statement.
    #[must_use]
    pub const fn conflict(self) -> &'static str {
        match self {
            Self::Reset => CONFLICT_ENQUEUE,
            Self::PreserveBackoff => CONFLICT_PRESERVE_BACKOFF,
        }
    }
}

/// Callback error of the executed enqueues that have NO domain failure to carry:
/// Go's `Enqueue`/`EnqueuePreservingBackoff` return a bare `error` that can only be a
/// driver failure (`:47-52`, `:53-57`, `:158-160`) — no validation, no `NotFound`.
/// `Infallible` makes the `Domain` arm structurally uninhabited rather than reserved
/// (`library_repo::LibraryScanError`, `tag_repo::TagWriteError`), and keeps every
/// SQLite failure on [`DbOperationError::Sqlite`] so `store::is_busy` still classifies
/// `SQLITE_BUSY` for the executor's retry loop.
pub type EnqueueFailure = DbOperationError<std::convert::Infallible>;

/// Go's owner-table lookup failure (`:59-62`):
/// `fmt.Errorf("artwork queue: no entity table for kind %q", kind.Prefix())`, typed
/// here so it stays a `Domain` failure rather than a statement that matches nothing.
#[derive(Debug, thiserror::Error)]
#[error("artwork queue: no entity table for kind {kind:?}")]
pub struct UnsupportedKind {
    /// The rejected `item_kind` token (Go interpolates `kind.Prefix()`).
    pub kind: String,
}

/// Callback error of the executed [`enqueue_all_missing_in_tx`]: the owner-table
/// rejection on the `Domain` arm, every SQLite failure on the `Sqlite` arm.
pub type EnqueueAllMissingFailure = DbOperationError<UnsupportedKind>;

/// `Enqueue` / `EnqueuePreservingBackoff` (`:47-57`, both through the private
/// `enqueue` `:150-163`) EXECUTED on the CALLER's transaction: one multi-row INSERT
/// per [`CHUNK_SIZE`] items in Go's `slices.Chunk` order (`:152`), executing exactly
/// what [`enqueue_sql`] renders with [`EnqueueMode::conflict`] as the suffix. Empty
/// input runs NO statement at all (Go's chunk loop yields nothing for an empty slice).
///
/// `now` is Go's single `time.Now()` (`:151`), sampled ONCE before the loop and bound
/// as BOTH `retry_at` and `enqueued_at` of EVERY row (`:155`); stamping per chunk
/// would give later chunks a later lifecycle start than Go does. The caller owns the
/// clock — this function reads none.
///
/// `items` is caller-trusted typed-domain data: `item_kind` is `Kind.Prefix()`
/// (`nd_id::ARTWORK_KIND_*_PREFIX`), `priority` is Go's `int` as the renderer types it
/// (`Value::Integer`), and an empty `image_type` defaults to [`IMAGE_TYPE_PRIMARY`]
/// inside the renderer (`cmp.Or`, `:155`). None of it is HTTP-supplied, and this layer
/// validates nothing, exactly as Go does.
///
/// Go checks no affected-row count (`:158` tests `err` only) and keeps looping, so a
/// chunk that changes nothing is a successful NO-OP. There is no count to report.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact) from the chunk that failed, raw and unwrapped like Go (`:159`).
/// [`DbOperationError::Domain`] is structurally uninhabited ([`EnqueueFailure`]).
pub fn enqueue_in_tx(
    tx: &rusqlite::Transaction<'_>,
    items: &[ArtworkQueueItem],
    mode: EnqueueMode,
    now: &DateTime<Utc>,
) -> Result<(), EnqueueFailure> {
    for statement in enqueue_sql(items, mode.conflict(), now) {
        crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
    }
    Ok(())
}

/// `EnqueueAllMissing` (`:58-68`) EXECUTED on the CALLER's transaction: the single
/// INSERT…SELECT [`enqueue_all_missing_sql`] renders, on the caller's lease.
///
/// `kind` is resolved through [`crate::artwork_repo::owner_table`] — Go's
/// `artworkOwnerTables` lookup (`:59-62`): an unsupported token is the typed
/// [`UnsupportedKind`] DOMAIN failure and NO statement runs, matching Go's early
/// `return 0, fmt.Errorf(...)`. Because the resolved table name is INTERPOLATED into
/// the statement, that whitelist is the injection boundary: no caller text can become
/// an identifier.
///
/// `priority` is Go's `priority int` (`:58`), bound as the renderer's
/// `Value::Integer`. It comes from repository code (`model.ArtworkPriorityScan`/
/// `ArtworkPriorityRecheck`, `scanner/scanner.go:286`,
/// `core/artwork/housekeeping.go:99`), never from a request.
///
/// [`SKIP_IF_QUEUED`] is `DO NOTHING` (`:96`), so an entity ALREADY queued keeps its
/// existing row untouched — priority, attempts, retry_at and trace all survive. The
/// anti-join excludes every entity with ANY `item_artwork` row of this kind (`:66`),
/// whether resolved (`hash <> ''`) or settled-absent (`hash = ''`): Go does not
/// distinguish the two here, and this call never touches `item_artwork`.
///
/// Returns `executeSQL`'s affected-row count (`sql_base_repository.go:336-354`), which
/// is what Go hands its callers (`scanner/scanner.go:286` totals it for a log line
/// only). No policy rides on it here.
///
/// **This is not a candidate count.** For this `INSERT…SELECT … DO NOTHING`, `sqlite3_changes()`
/// equals the rows actually inserted; skipped conflicts and rows filtered out by the SELECT
/// contribute zero. Verified with `sqlite3` over the fresh DDL: an anti-join select whose sole
/// candidate is already queued reports `0`; one that inserts one row and skips one reports `1`.
/// Go inherits the same driver semantics through `database/sql`'s `RowsAffected`, so the number
/// is returned verbatim and no policy rides on it.
///
/// # Errors
/// [`DbOperationError::Domain`]·[`UnsupportedKind`] BEFORE any SQL for a kind with no
/// owner table; [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error`
/// from the statement, raw and unwrapped like Go (`:64-67`).
pub fn enqueue_all_missing_in_tx(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    priority: i64,
    now: &DateTime<Utc>,
) -> Result<i64, EnqueueAllMissingFailure> {
    let Some(entity_table) = crate::artwork_repo::owner_table(kind) else {
        return Err(DbOperationError::Domain(UnsupportedKind {
            kind: kind.to_string(),
        }));
    };
    let rows = crate::execution::execute(
        tx,
        &enqueue_all_missing_sql(kind, entity_table, priority, now),
    )
    .map_err(DbOperationError::sqlite)?;
    Ok(i64::try_from(rows).unwrap_or(i64::MAX))
}

/// `EnqueueIfMissing` (`:70-93`) EXECUTED on the CALLER's transaction: one CTE
/// INSERT per [`CHUNK_SIZE`] items in Go's `slices.Chunk` order (`:72`), executing
/// exactly what [`enqueue_if_missing_sql`] renders.
///
/// `now` is Go's single `time.Now()` per call (`:71`), sampled BEFORE the loop and bound
/// as both `retry_at` and `enqueued_at` of every row in the chunk (`:83`); this port takes
/// it from the caller and reads no clock.
///
/// Two filters make this mode different from [`enqueue_in_tx`], and both are tested:
/// an entity with ANY `item_artwork` row of this kind+image_type is excluded by the
/// `NOT EXISTS` anti-join (`:85-86`) — resolved or settled-absent alike, Go tests only
/// for existence — and an entity ALREADY queued keeps its existing row untouched under
/// [`SKIP_IF_QUEUED`], so a recheck cannot bump priority or retry_at (`:95`).
///
/// Go checks no affected-row count (`:89` tests `err` only) and keeps chunking, so there
/// is no count to report. Empty input runs NO statement at all.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended code
/// intact) from the chunk that failed, raw and unwrapped like Go (`:90`).
/// [`DbOperationError::Domain`] is structurally uninhabited ([`EnqueueFailure`]).
pub fn enqueue_if_missing_in_tx(
    tx: &rusqlite::Transaction<'_>,
    items: &[ArtworkQueueItem],
    now: &DateTime<Utc>,
) -> Result<(), EnqueueFailure> {
    for statement in enqueue_if_missing_sql(items, now) {
        crate::execution::execute(tx, &statement).map_err(DbOperationError::sqlite)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    /// Deterministic caller clock (Go's `time.Now()`,
    /// `artwork_queue_repository.go:63,71,143,151,167`).
    fn at(year: u32, month: u32, day: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(i32::try_from(year).unwrap(), month, day)
                .unwrap()
                .and_hms_opt(h, mi, s)
                .unwrap(),
        )
    }

    #[test]
    fn enqueue_resets_the_lifecycle_on_conflict() {
        // Arrange: one item (Enqueue :47-52).
        let item = ArtworkQueueItem {
            item_kind: nd_id::ARTWORK_KIND_ALBUM_PREFIX.to_string(),
            item_id: "al-1".to_string(),
            image_type: "".to_string(),
            priority: 0,
            attempts: 0,
            retry_at: None,
            enqueued_at: None,
            trace: "".to_string(),
        };
        // Act:
        let stmts = enqueue_sql_reset(&[item], &at(2024, 1, 2, 3, 4, 5));
        // Assert: 7-column insert, image_type defaulted, lifecycle reset in the
        // conflict clause (:49).
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].1.len(), 7);
        assert_eq!(stmts[0].1[2], Value::Text(IMAGE_TYPE_PRIMARY.to_string()));
        assert!(stmts[0]
            .0
            .contains("DO UPDATE SET priority = MAX(priority, excluded.priority)"));
        assert!(stmts[0].0.contains("attempts = 0"));
        assert!(stmts[0].0.contains("trace = '[]'"));
    }

    #[test]
    fn preserve_backoff_upsert_touches_only_priority() {
        // Act: EnqueuePreservingBackoff (:54-56).
        let stmts = enqueue_sql_preserving_backoff(
            &[ArtworkQueueItem::default()],
            &at(2024, 1, 2, 3, 4, 5),
        );
        // Assert: retry_at is NOT reset (Go's comment :52-53).
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0]
            .0
            .contains("DO UPDATE SET priority = MAX(priority, excluded.priority)"));
        assert!(!stmts[0].0.contains("retry_at = excluded.retry_at"));
    }

    #[test]
    fn enqueue_all_missing_skips_resolved_entities() {
        // Act: EnqueueAllMissing for albums (:58-68). Go binds `kind.Prefix()`
        // twice (`artwork_queue_repository.go:66-67`), so the item_kind args are
        // the 2-char prefix, while the entity table is the OWNER_TABLES value.
        let (sql, args) = enqueue_all_missing_sql(
            nd_id::ARTWORK_KIND_ALBUM_PREFIX,
            "album",
            3,
            &at(2024, 1, 2, 3, 4, 5),
        );
        // Assert: the NOT IN resolved-state guard, six bound values
        // (kind, primary, priority, now, now, kind, :66-68).
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], Value::Text("al".to_string()));
        assert_eq!(args[5], Value::Text("al".to_string()));
        assert!(
            sql.contains("WHERE id NOT IN (SELECT item_id FROM item_artwork WHERE item_kind = ?)")
        );
    }

    #[test]
    fn dequeue_batch_orders_priority_desc_then_enqueued_asc() {
        // Act: DequeueBatch(n=5, kinds album) (:166-176). `DequeueBatch` takes
        // already-prefix strings — Go's `KindPrefixes(kinds)` output
        // (`artwork_queue_repository.go:201`, `model/artwork_id.go:44-46`).
        let (sql, args) = dequeue_batch_sql(
            5,
            &at(2024, 1, 2, 3, 4, 5),
            &[nd_id::ARTWORK_KIND_ALBUM_PREFIX.to_string()],
        );
        // Assert: eligibility + kind IN filter, the ORDER BY pair, limit last.
        // The `LIMIT 5` bound is a squirrel literal (`select.go:139-142`), so
        // only the eligibility bound and the kind filter are args.
        assert!(sql.contains("WHERE retry_at <= ? AND item_kind IN (?)"));
        assert!(sql.contains("ORDER BY priority DESC, enqueued_at ASC LIMIT 5"));
        assert_eq!(args.len(), 2);
        assert_eq!(
            args[0],
            Value::Text("2024-01-02 03:04:05+00:00".to_string())
        );
        assert_eq!(args.last(), Some(&Value::Text("al".to_string())));
    }

    #[test]
    fn failed_pseudo_source_is_the_absent_gave_up_arm() {
        // Arrange: CountBySource asking only the failed pseudo-source
        // (artworkSourceFilter :112-120).
        let sources = vec![SOURCE_FAILED.to_string()];
        // Act:
        let (sql, args) = source_filter_sql(nd_id::ARTWORK_KIND_ALBUM_PREFIX, &sources);
        // Assert: stored list is empty so no IN arm, absent arm present, and
        // asking absent+failed collapses to one arm.
        // Go's render for the pseudo-source-only arm (executed squirrel,
        // /tmp/g2_source_filter_render.txt): both empty strings are BOUND.
        assert_eq!(
            sql,
            "(item_kind = ? AND ((hash = ? AND last_failure <> ?)))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("al".to_string()),
                Value::Text(String::new()),
                Value::Text(String::new()),
            ]
        );
    }

    #[test]
    fn source_filter_wraps_the_whole_conjunction_like_go() {
        // Arrange: every arm Go's artworkSourceFilter can produce (:108-120).
        // The expected texts are the executed squirrel render (standalone
        // program /tmp/sd3g2_render, saved /tmp/g2_source_filter_render.txt) and
        // the executed repository trace (/tmp/g2_folder_queue_trace.txt).
        let cases: &[(&[&str], &str)] = &[
            (&[], "(item_kind = ?)"),
            (&["cover"], "(item_kind = ? AND (source IN (?)))"),
            (
                &["!failed"],
                "(item_kind = ? AND ((hash = ? AND last_failure <> ?)))",
            ),
            (
                &["cover", "!failed"],
                "(item_kind = ? AND (source IN (?) OR (hash = ? AND last_failure <> ?)))",
            ),
            (
                &["embedded", "cover"],
                "(item_kind = ? AND (source IN (?, ?)))",
            ),
        ];
        for (sources, expected) in cases {
            let owned: Vec<String> = sources.iter().map(|s| (*s).to_string()).collect();
            // Act:
            let (sql, args) = source_filter_sql("al", &owned);
            // Assert: the And group is present, so OR cannot escape the kind.
            assert_eq!(sql, *expected, "sources {sources:?}");
            assert!(sql.starts_with("(item_kind"), "sources {sources:?}");
            // Empty strings are BOUND, never inlined (Go's Eq/NotEq both take ?).
            let inlined = args
                .iter()
                .filter(|v| matches!(v, Value::Text(t) if t.is_empty()))
                .count();
            let want = if sources.contains(&"!failed") { 2 } else { 0 };
            assert_eq!(inlined, want, "sources {sources:?}");
        }
    }

    #[test]
    fn count_and_purge_share_one_filter() {
        // Arrange/Act: kinds+priorities filter (artworkQueueFilter :198-207),
        // whose kinds are `model.KindPrefixes(kinds)` (`:201`) — prefixes.
        let kinds = vec![nd_id::ARTWORK_KIND_ALBUM_PREFIX.to_string()];
        let priorities = vec![2, 5];
        let (sql, args) = purge_queued_sql(&kinds, &priorities);
        // Assert: AND of both INs with three bound values, and the count
        // preview sees exactly that WHERE.
        assert!(sql.contains("WHERE item_kind IN (?) AND priority IN (?, ?)"));
        assert_eq!(args.len(), 3);
        let preview = count_queued_sql(&kinds, &priorities);
        assert!(preview.0.contains("GROUP BY") || preview.0.contains("item_kind, priority"));
    }
}
