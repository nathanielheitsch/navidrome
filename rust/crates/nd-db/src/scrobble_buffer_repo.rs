//! Port of `persistence/scrobble_buffer_repository.go` (103L) — the pending
//! scrobbles queue drained by the scrobbler agents.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `queryAllSlice`/`queryOne`/`executeSQL`/`delete`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with `store.rs`/the caller, so the port stops at the same
//! `(sql, args)` pairs (see [`Sql`]). Row hydration (Go scans into
//! `dbScrobbleBuffer`, a `media_file` join) is the caller's; the ports pin the
//! statements, including the `m.*, s.*` column order Next relies on (`:71`).
//!
//! Ported here: `UserIDs`' grouped select (`:44-54`), `Enqueue`'s SetMap
//! insert with `id.NewRandom()` (`:56-67`), `Next`'s LEFT-JOIN head-of-queue
//! select (`:69-89`), `Dequeue`/`Discard`'s deletes (`:91-93`, `:95-97`) and
//! `Length`'s base `count()` (`:99-100`).
//!
//! `Next`'s follow-up `getParticipants` call (`:84`) depends on a live
//! `media_file` row and the artist repository; its SQL is already ported in
//! `sql_participations.rs`, so the caller composes it.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::sqlrepo::{Repository, Value};

/// The `scrobble_buffer` table (`:37`).
pub const TABLE: &str = "scrobble_buffer";

/// `model.ScrobbleEntry` (`model/scrobble_buffer.go`): one queued play, the
/// joined `media_file` carried by `Next`'s select.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrobbleEntry {
    /// Go `ID`.
    pub id: String,
    /// Go `Service`.
    pub service: String,
    /// Go `UserID`.
    pub user_id: String,
    /// Unix seconds, Go's `time.Time` column value.
    pub play_time: i64,
    /// Unix seconds; `Enqueue` stamps `time.Now()` (`:64`).
    pub enqueue_time: i64,
    /// Go `MediaFileID`.
    pub media_file_id: String,
}

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `UserIDs` (`:44-54`): the distinct users with pending scrobbles for one
/// service, most-loaded first — `GROUP BY user_id ORDER BY count(*)`
/// (ascending; Go relies on SQLite's stable group order, kept as written).
#[must_use]
pub fn user_ids_sql(service: &str) -> Sql {
    let (mut sql, args) = Repository::new(TABLE)
        .select(["user_id"])
        .where_eq("service", service)
        .to_sql()
        .expect("the user-ids select materializes");
    sql.push_str(" GROUP BY user_id ORDER BY count(*)");
    (sql, args)
}

/// `Enqueue` (`:56-67`): the SetMap insert. `id` is Go's `id.NewRandom()`
/// (`:59`); `enqueue_time` is Go's `time.Now()` — both passed in so the
/// statements stay pure, values bound in the map literal's declaration order.
#[must_use]
pub fn enqueue_sql(
    id: &str,
    service: &str,
    user_id: &str,
    media_file_id: &str,
    play_time: i64,
    enqueue_time: i64,
) -> Sql {
    (
        format!(
            "INSERT INTO {TABLE} (id, user_id, service, media_file_id, play_time, enqueue_time) \
             VALUES (?, ?, ?, ?, ?, ?)"
        ),
        vec![
            Value::from(id),
            Value::from(user_id),
            Value::from(service),
            Value::from(media_file_id),
            Value::Integer(play_time),
            Value::Integer(enqueue_time),
        ],
    )
}

/// `Now` helper matching `Enqueue`'s `time.Now()` (`:64`): unix seconds.
#[must_use]
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

/// `Next` (`:69-89`): the head of the user's queue for one service, joined to
/// its `media_file` row. `m.*, s.*` keeps `s.id` authoritative (`:71` comment)
/// and `ORDER BY play_time, s.rowid` breaks ties FIFO; `LIMIT 1` pins the head.
/// Not-found (Go maps it to a `nil` entry, `:78-80`) is the caller's zero rows.
#[must_use]
pub fn next_sql(service: &str, user_id: &str) -> Sql {
    let (sql, args) = Repository::new("scrobble_buffer s")
        .select(["m.*, s.*"])
        .where_eq("s.service", service)
        .where_eq("s.user_id", user_id)
        .order_by(["play_time", "s.rowid"])
        .limit(1)
        .to_sql()
        .expect("the next select materializes");
    // Go LEFT-joins media_file (:72); the builder renders INNER only, so the
    // clause is appended at its squirrel position — after FROM, before WHERE.
    let from = "FROM scrobble_buffer s";
    let at = sql.find(from).expect("the from clause is present") + from.len();
    let mut sql = sql;
    sql.replace_range(at..at, " LEFT JOIN media_file m on m.id = s.media_file_id");
    (sql, args)
}

/// `Dequeue` (`:91-93`): delete the one buffered entry by its id — the row
/// `Next` returned.
#[must_use]
pub fn dequeue_sql(entry_id: &str) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE id = ?"),
        vec![Value::from(entry_id)],
    )
}

/// `Discard` (`:95-97`): drop every pending entry of one service.
#[must_use]
pub fn discard_sql(service: &str) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE service = ?"),
        vec![Value::from(service)],
    )
}

/// `Length` (`:99-100`) via the base `count()`
/// (`sql_base_repository.go:566-576`).
#[must_use]
pub fn length_sql() -> Sql {
    Repository::new(TABLE)
        .count(TABLE)
        .to_sql()
        .expect("the length count materializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids_groups_by_user_ordered_by_count() {
        // Arrange/Act: UserIDs' grouped select (:44-54).
        let (sql, args) = user_ids_sql("lastfm");
        // Assert:
        assert_eq!(
            sql,
            "SELECT user_id FROM scrobble_buffer WHERE service = ? GROUP BY user_id ORDER BY count(*)"
        );
        assert_eq!(args, vec![Value::Text("lastfm".to_string())]);
    }

    #[test]
    fn enqueue_binds_all_six_columns_in_declaration_order() {
        // Arrange: Enqueue's SetMap (:59-65) with the caller-supplied id and
        // time.Now() stamp (:59, :64).
        // Act:
        let (sql, args) = enqueue_sql("buf1", "lastfm", "user1", "mf1", 1_700, now_unix());
        // Assert:
        assert!(sql.starts_with(
            "INSERT INTO scrobble_buffer (id, user_id, service, media_file_id, play_time, enqueue_time) VALUES (?, ?, ?, ?, ?, ?)"
        ));
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], Value::Text("buf1".to_string()));
        assert_eq!(args[1], Value::Text("user1".to_string()));
        assert_eq!(args[2], Value::Text("lastfm".to_string()));
        assert_eq!(args[3], Value::Text("mf1".to_string()));
        assert_eq!(args[4], Value::Integer(1_700));
    }

    #[test]
    fn next_selects_the_fifo_head_with_the_media_file_left_join() {
        // Arrange/Act: Next's select (:70-76).
        let (sql, args) = next_sql("lastfm", "user1");
        // Assert: m.* first so s.id wins; LEFT JOIN sits after FROM, before WHERE.
        // `LIMIT 1` is a squirrel literal (`select.go:139-142`), so it is not an arg.
        assert_eq!(
            sql,
            "SELECT m.*, s.* FROM scrobble_buffer s \
             LEFT JOIN media_file m on m.id = s.media_file_id \
             WHERE s.service = ? AND s.user_id = ? \
             ORDER BY play_time, s.rowid LIMIT 1"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("lastfm".to_string()),
                Value::Text("user1".to_string()),
            ]
        );
    }

    #[test]
    fn dequeue_and_discard_delete_scoped_rows() {
        // Arrange/Act: Dequeue (:91-93) and Discard (:95-97).
        let dequeue = dequeue_sql("buf1");
        let discard = discard_sql("listenbrainz");
        // Assert:
        assert_eq!(dequeue.0, "DELETE FROM scrobble_buffer WHERE id = ?");
        assert_eq!(dequeue.1, vec![Value::Text("buf1".to_string())]);
        assert_eq!(discard.0, "DELETE FROM scrobble_buffer WHERE service = ?");
        assert_eq!(discard.1, vec![Value::Text("listenbrainz".to_string())]);
    }

    #[test]
    fn length_counts_distinct_ids() {
        // Arrange/Act: Length's base count() (:99-100).
        let (sql, args) = length_sql();
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct scrobble_buffer.id) as count FROM scrobble_buffer"
        );
        assert!(args.is_empty());
    }
}
