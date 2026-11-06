//! Port of `persistence/playqueue_repository.go` (176L) — the per-user play
//! queue. Shape conventions match the sibling repositories: methods return
//! materialized `(sql, args)` pairs, Go's `loggedUser(ctx)` logging stays with
//! the caller.
//!
//! Ported: `Retrieve`/`RetrieveWithMediaFiles`' selects (`:78-92`),
//! `clearPlayQueue`/`Clear` (`:168-174`), `Store`'s decision sequence
//! (`:38-76` — retrieve first, keep an existing id, clear-then-put when no
//! column names were given, skip the put for an empty queue), `fromModel`/`toModel`
//! items (de)serialisation (`:94-129` — comma-joined ids), and `loadTracks`
//! (`:133-166` — 500-id chunks of `media_file.id IN (…)`).

use crate::sqlrepo::{Repository, SqlError, Value};

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The `playqueue` table (`:23`).
pub const TABLE: &str = "playqueue";

/// `loadTracks`' chunk size (`:145`): keeps a multi-row `IN` under SQLite's
/// bind-variable limit.
pub const TRACK_CHUNK_SIZE: usize = 500;

/// `playQueue` struct (`:27-36`): the row shape. `items` holds comma-joined
/// media-file ids (`fromModel`, `:108`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayQueueRow {
    /// Queue row id.
    pub id: String,
    /// Owning user.
    pub user_id: String,
    /// Cursor index into items.
    pub current: i64,
    /// Play offset in seconds.
    pub position: i64,
    /// Which client last moved the cursor ("play"/"user"/"" — `:100`).
    pub changed_by: String,
    /// Comma-joined media-file ids in queue order (`:33`).
    pub items: String,
    /// Epoch seconds; Go stamps `created_at` only for a brand-new row
    /// (`:66-68`) and always refreshes `updated_at` (`:69`).
    pub created_at: i64,
    /// Epoch seconds, refreshed on every Store (Store :69).
    pub updated_at: i64,
}

/// `Retrieve` / `RetrieveWithMediaFiles`' select (`:79-80`, `:88-89`):
/// `newSelect().Columns("*").Where(Eq{"user_id": userId})`.
#[must_use]
pub fn retrieve_sql(user_id: &str) -> Sql {
    Repository::new(TABLE)
        .select(vec!["*"])
        .where_eq("user_id", user_id)
        .to_sql()
        .unwrap()
}

/// `clearPlayQueue` / `Clear` (`:168-174`): the base `delete` over
/// `Eq{"user_id": userId}` (`sql_base_repository.go:649-660`).
#[must_use]
pub fn clear_sql(user_id: &str) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE user_id = ?"),
        vec![Value::Text(user_id.to_string())],
    )
}

/// `fromModel` (`:94-110`): the queue's items as the comma-joined id string
/// the `items` column stores.
#[must_use]
pub fn items_to_column(items: &[String]) -> String {
    items.join(",")
}

/// `toModel` (`:113-129`): split the column back into ids, blank/whitespace
/// entries dropped like Go's trim guard (`:122-127`).
#[must_use]
pub fn column_to_items(column: String) -> Vec<String> {
    if column.trim().is_empty() {
        return Vec::new();
    }
    column
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
}

/// `Store` (`:38-76`): returns the statements in Go's execution order.
/// `existing_found` is the retrieve outcome and `existing_id` the row's id
/// (Go keeps the provided id only when nothing was found, `:49-51`); an
/// empty `cols_to_update` means "replace the whole queue" — clear first, and
/// when the queue is empty the clear is all Store does (`:54-63`).
/// The caller stamps `created_at` only when the effective id is new
/// (`:66-68`) and `updated_at` always (`:69`) — both arrive as arguments.
pub fn store_sql(
    q: PlayQueueRow,
    existing_found: bool,
    existing_id: String,
    cols_to_update: &[&str],
    now: i64,
) -> Vec<Sql> {
    let mut stmts = Vec::new();
    let id = if existing_found && !existing_id.is_empty() {
        existing_id
    } else {
        q.id
    };
    if cols_to_update.is_empty() {
        stmts.push(clear_sql(&q.user_id));
        if q.items.trim().is_empty() {
            return stmts;
        }
    }
    // The base `put` upsert (sql_base_repository.go:613-635): UPDATE the
    // requested columns matched by id, INSERT when nothing matched.
    let columns = [
        "id",
        "user_id",
        "current",
        "position",
        "changed_by",
        "items",
        "created_at",
        "updated_at",
    ];
    let values = vec![
        Value::Text(id.clone()),
        Value::Text(q.user_id.clone()),
        Value::Integer(q.current),
        Value::Integer(q.position),
        Value::Text(q.changed_by),
        Value::Text(q.items),
        Value::Integer(q.created_at),
        Value::Integer(now),
    ];
    let mut set_cols: Vec<String> = Vec::new();
    let mut set_args: Vec<Value> = Vec::new();
    for (col, value) in columns.iter().zip(values.iter()) {
        if !cols_to_update.is_empty() && !cols_to_update.iter().any(|c| c == col) {
            continue;
        }
        set_cols.push((*col).to_string());
        set_args.push(value.clone());
    }
    if !id.is_empty() && !set_cols.is_empty() {
        let assignments = set_cols
            .iter()
            .map(|c| format!("{c} = ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut update_args = set_args;
        update_args.push(Value::Text(id.clone()));
        stmts.push((
            format!("UPDATE {} SET {} WHERE id = ?", TABLE, assignments),
            update_args,
        ));
    }
    let insert_sql = Repository::new(TABLE)
        .to_sql()
        .unwrap()
        .0
        .replace(" FROM", " INSERT INTO,");
    let _ = insert_sql;
    let placeholders = columns.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    stmts.push((
        format!(
            "INSERT INTO {TABLE} ({}) VALUES ({})",
            columns.join(", "),
            placeholders
        ),
        values,
    ));
    stmts
}

/// `loadTracks`' per-chunk query (`:141-155`): the media-file `IN` filter
//. Go runs through `mfRepo.GetAll` with `Eq{"media_file.id": chunk}`; the
// result re-hydration preserves the input order and drops ids gone from the
// DB (`:157-165`) — that reordering is caller-side work over these rows.
pub fn load_tracks_sql(ids: &[String]) -> Result<Vec<Sql>, SqlError> {
    let mut stmts = Vec::new();
    for chunk in ids.chunks(TRACK_CHUNK_SIZE) {
        let (sql, args) = Repository::new("media_file")
            .where_in(
                "media_file.id",
                chunk
                    .iter()
                    .map(|id| Value::Text(id.clone()))
                    .collect::<Vec<_>>(),
            )
            .to_sql()?;
        stmts.push((sql, args));
    }
    Ok(stmts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_replaces_the_whole_queue_after_clearing_it() {
        // Arrange: an existing row was found, Store called with no column
        // names (Store :54 — the clear-the-whole-queue path).
        let q2 = PlayQueueRow {
            user_id: "usr-1".to_string(),
            items: "mf-1,mf-2".to_string(),
            ..Default::default()
        };
        // Act:
        let stmts = store_sql(q2, true, "pq-old".to_string(), &[], 99);
        // Assert: the DELETE clears first, then UPDATE (kept id) then INSERT
        // legs of the base put (:55-75).
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0].0, "DELETE FROM playqueue WHERE user_id = ?");
        assert_eq!(stmts[0].1, vec![Value::Text("usr-1".to_string())]);
        assert!(stmts[1].0.starts_with("UPDATE playqueue SET"));
        assert!(stmts[1].0.ends_with("WHERE id = ?"));
        assert_eq!(
            stmts[1].1.first(),
            Some(Value::Text("pq-old".to_string())).as_ref()
        );
        assert_eq!(
            stmts[1].1.last(),
            Some(Value::Text("pq-old".to_string())).as_ref()
        );
        assert_eq!(stmts[2].0, "INSERT INTO playqueue (id, user_id, current, position, changed_by, items, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)");
        assert_eq!(stmts[2].1.len(), 8);
    }

    #[test]
    fn store_of_an_empty_queue_only_clears() {
        // Arrange: no items and no column names (Store :60-62 — clear then
        // return before the put).
        let q = PlayQueueRow::default();
        // Act:
        let stmts = store_sql(q, false, "".to_string(), &[], 42);
        // Assert:
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].0.starts_with("DELETE FROM playqueue"));
    }

    #[test]
    fn items_column_round_trips_through_the_comma_form() {
        // Arrange/Act: fromModel's join (:108) and toModel's split (:122-127).
        let items = vec!["mf-1".to_string(), "mf-2".to_string()];
        // Assert:
        assert_eq!(items_to_column(&items), "mf-1,mf-2");
        assert_eq!(column_to_items("mf-1,mf-2".to_string()), items);
        assert!(column_to_items("   ".to_string()).is_empty());
    }

    #[test]
    fn load_tracks_chunks_at_the_500_id_limit() {
        // Arrange: 501 ids (loadTracks :144-145).
        let ids = (1..=501).map(|i| format!("mf-{i}")).collect::<Vec<_>>();
        // Act:
        let stmts = load_tracks_sql(&ids).unwrap();
        // Assert: two IN statements, and Go's IN renders via squirrel IN
        // (`WHERE media_file.id IN (?, …)`).
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].0.contains("WHERE media_file.id IN ("));
        assert_eq!(stmts[0].1.len(), 500);
        assert_eq!(stmts[1].1.len(), 1);
    }
}
