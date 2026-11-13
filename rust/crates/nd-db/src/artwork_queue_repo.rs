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
//! reproduced verbatim: dropping `ArtworkSourceFailed` from the requested
//! sources decides between `source IN (…)`, the absent-state arm
//! (`hash = '' AND last_failure != ''`), or an OR of both.

use chrono::{DateTime, Utc};

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

/// `EnqueueIfMissing` (`:70-93`): chunked CTE inserts —
/// `WITH new_items(...) AS (VALUES (?,?,?,?)…) SELECT n.… , 0, ?, ?
/// FROM new_items n WHERE NOT EXISTS (SELECT 1 FROM item_artwork ia
/// WHERE ia.item_kind = n.item_kind AND …) skipIfQueued` (`:81-87`).
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
            let select = concat!(
                ") SELECT n.item_kind, n.item_id, n.image_type, n.priority, 0, ?, ? ",
                "FROM new_items n WHERE NOT EXISTS (SELECT 1 FROM item_artwork ia ",
                "WHERE ia.item_kind = n.item_kind AND ia.item_id = n.item_id ",
                "AND ia.image_type = n.image_type)",
            );
            (
                format!("WITH new_items(item_kind, item_id, image_type, priority) AS (VALUES {rows} {select}{SKIP_IF_QUEUED}"),
                args_all,
            )
        })
        .collect::<Vec<_>>()
}

/// `artworkSourceFilter` (`:107-121`): kind Eq plus, when sources were given,
/// the OR of the stored-source IN and the absent/gave-up arm. Returned as
/// `(sql, args)` fragments for the count/queue paths below.
#[must_use]
pub fn source_filter_sql(kind: &str, sources: &[String]) -> Sql {
    if sources.is_empty() {
        return (
            "item_kind = ?".to_string(),
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
        arms.push(format!("source IN ({placeholders})"));
        args.extend(stored.iter().map(|s| Value::Text(s.clone())));
    }
    if stored.len() != sources.len() {
        arms.push("(hash = '' AND last_failure != '')".to_string());
    }
    let or_body = if arms.len() == 1 {
        arms[0].to_string()
    } else {
        format!("({}) OR ({})", arms[0], arms[1])
    };
    args.insert(0, Value::Text(kind.to_string()));
    (format!("item_kind = ? AND {or_body}"), args)
}

/// `CountBySource` (`:123-128`): `Select("count(*) as count")` over
/// `item_artwork` with the source filter.
#[must_use]
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
            item_kind: "album".to_string(),
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
        // Act: EnqueueAllMissing for albums (:58-68).
        let (sql, args) = enqueue_all_missing_sql("album", "album", 3, &at(2024, 1, 2, 3, 4, 5));
        // Assert: the NOT IN resolved-state guard, six bound values
        // (kind, primary, priority, now, now, kind, :66-68).
        assert_eq!(args.len(), 6);
        assert!(
            sql.contains("WHERE id NOT IN (SELECT item_id FROM item_artwork WHERE item_kind = ?)")
        );
    }

    #[test]
    fn dequeue_batch_orders_priority_desc_then_enqueued_asc() {
        // Act: DequeueBatch(n=5, kinds album) (:166-176).
        let (sql, args) = dequeue_batch_sql(5, &at(2024, 1, 2, 3, 4, 5), &["album".to_string()]);
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
        assert_eq!(args.last(), Some(&Value::Text("album".to_string())));
    }

    #[test]
    fn failed_pseudo_source_is_the_absent_gave_up_arm() {
        // Arrange: CountBySource asking only the failed pseudo-source
        // (artworkSourceFilter :112-120).
        let sources = vec![SOURCE_FAILED.to_string()];
        // Act:
        let (sql, args) = source_filter_sql("album", &sources);
        // Assert: stored list is empty so no IN arm, absent arm present, and
        // asking absent+failed collapses to one arm.
        assert_eq!(sql, "item_kind = ? AND (hash = '' AND last_failure != '')");
        assert_eq!(args, vec![Value::Text("album".to_string())]);
    }

    #[test]
    fn count_and_purge_share_one_filter() {
        // Arrange/Act: kinds+priorities filter (artworkQueueFilter :198-207).
        let kinds = vec!["album".to_string()];
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
