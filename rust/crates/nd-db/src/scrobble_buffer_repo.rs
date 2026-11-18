//! Port of `persistence/scrobble_buffer_repository.go` (103L) — the pending
//! scrobbles queue drained by the scrobbler agents.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `queryAllSlice`/`queryOne`/`executeSQL`/`delete`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with `store.rs`/the caller, so the port stops at the same
//! `(sql, args)` pairs (see [`Sql`]).
//!
//! S-D3e landed the READ side: [`ScrobbleBufferRow`] (Go's `dbScrobbleBuffer`
//! phase 1), [`ScrobbleEntryRecord`] (its hydrated model, which carries the
//! joined `media_file`) and the executed [`ScrobbleBufferRepo::next`], all on the
//! shared seam ([`crate::hydrate::row_to_scrobble_buffer`] →
//! [`crate::hydrate::scrobble_buffer_post_scan`]). The statements stay byte-identical
//! to Go's, including the `m.*, s.*` column order Next depends on (`:66-67`).
//!
//! Ported here: `UserIDs`' grouped select (`:44-54`), `Enqueue`'s SetMap
//! insert with `id.NewRandom()` (`:56-67`), `Next`'s LEFT-JOIN head-of-queue
//! select (`:69-89`), `Dequeue`/`Discard`'s deletes (`:91-93`, `:95-97`) and
//! `Length`'s base `count()` (`:99-100`).
//!
//! **`getParticipants` is NOT ported** (`Next`'s follow-up at `:84`,
//! `sql_participations.go:127-146`), so [`ScrobbleBufferRepo::next`] stops at the
//! delegated `dbMediaFile.PostScan`. That is NOT an empty-participants result:
//! `model.ScrobbleEntry` has no participants field of its own
//! (`model/scrobble_buffer.go:8-16` embeds `MediaFile`), so Go's
//! `ScrobbleEntry.Participants` IS `MediaFile.Participants` — executed as
//! `&e.Participants == &e.MediaFile.Participants == true` on every entry `Next`
//! returns — and that alias holds the JSON-decoded column.
//! [`ScrobbleBufferRepo::next`] therefore exposes exactly that decoded map, which
//! is what the seam hydrates, and does NOT
//! apply Go's re-read. The re-read replaces each `Participant.Artist` WHOLESALE —
//! including `SortArtistName`/`OrderArtistName`/`MBID`/the stats counts, none of
//! which the `participants` column stores — for every id its
//! `artist.id IN (…)` select returns, and it returns nothing for ids with no
//! `artist` row or no `library_artist` junction row for the logged user (both
//! joins INNER, `artist_repository.go:195-202`). Executed against the real
//! repository with JSON names `JSON-A1/A2/A3` and an `artist` row only for `a1`
//! and `a3` (`library_artist` only for `a1`):
//! ```text
//!   Next(lim) id=a1 name="DB-A1"   sort="SortA1" order="db-a1" mbid="mbid-a1"
//!   Next(lim) id=a2 name="JSON-A2" sort="" order="" mbid=""   // no artist row
//!   Next(lim) id=a3 name="JSON-A3" sort="" order="" mbid=""   // no library_artist row
//!   Get(lim)  all three keep their JSON names and empty sort/order/mbid
//! ```
//! The port deliberately does not emulate a partial version of that: this crate's
//! [`crate::sql_participations::Participant`] carries only `{id, name, subRole}`
//! — Go's stored shape (`sql_participations.go:13-17`) — so it has nowhere to put
//! the re-read artist's extra fields, and half-applying it would report values Go
//! does not produce. Callers needing Go's `Next` semantics compose the re-read
//! themselves (see `docs/rust-port/hydrate-contract.md`, which assigns
//! `getParticipants`/`GetWithParticipants` to the artist-hydration lane).

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};

use crate::mediafile_repo::MediaFileRecord;
use crate::sql_participations::Participants;
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

/// `model.ScrobbleEntry` (`model/scrobble_buffer.go:8-16`) in its **hydrated**
/// shape: Go's `Next` returns `*ScrobbleEntry`, whose `MediaFile` embed is filled
/// by the delegated `dbMediaFile.PostScan`, and whose promoted `Participants`
/// field Go OVERWRITES afterwards with the unported `getParticipants` re-fetch
/// (see the module note).
///
/// Distinct from this module's [`ScrobbleEntry`] write DTO (the record `Enqueue`
/// takes), which has no media file. `hydrate-scan-contract.md:162` names this
/// shape `ScrobbleEntryRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrobbleEntryRecord {
    /// Go `ID` — the scrobble row's id, NOT the media file's. It is the value dbx
    /// binds for the duplicated name `id`, which `SELECT m.*, s.*` makes `s.id` by
    /// putting `s.*` last (`scrobble_buffer_repository.go:66-67`).
    pub id: String,
    /// Go `Service`.
    pub service: String,
    /// Go `UserID`.
    pub user_id: String,
    /// Go `PlayTime` — the non-pointer `time.Time` read from `s.play_time`.
    pub play_time: DateTime<Utc>,
    /// Go `EnqueueTime` — `Enqueue`'s `time.Now()` (`:64`).
    pub enqueue_time: DateTime<Utc>,
    /// Go `MediaFileID`.
    pub media_file_id: String,
    /// Go's promoted `MediaFile` embed: the joined `m.*` row, hydrated by the
    /// delegated `dbMediaFile.PostScan` and then id-overwritten with
    /// [`ScrobbleEntryRecord::media_file_id`]
    /// (`scrobble_buffer_repository.go:28`).
    pub media: MediaFileRecord,
    /// Go `Participants` — the media file's decoded `participants` column, because
    /// in Go the entry has NO participants field of its own:
    /// `model.ScrobbleEntry` (`model/scrobble_buffer.go:8-16`) embeds `MediaFile`,
    /// so `ScrobbleEntry.Participants` IS `MediaFile.Participants` (executed:
    /// `&e.Participants == &e.MediaFile.Participants` for every entry `Next`
    /// returns). Go's `Next` then OVERWRITES that alias with `getParticipants`'s
    /// re-read (`:84`, `sql_participations.go:127-146`), which this port does not
    /// perform — see the module note. [`ScrobbleEntryRecord::media`] carries the
    /// same map, since it is the same field in Go.
    pub participants: Participants,
}

impl ScrobbleEntryRecord {
    /// `Dequeue` (`:91-93`) keys on the entry id Go returned, so the port's delete
    /// takes exactly this field — not `media_file_id`, and not `media.id`, which
    /// the hook overwrote.
    #[must_use]
    pub fn entry_id(&self) -> &str {
        &self.id
    }
}

/// `dbScrobbleBuffer` (`scrobble_buffer_repository.go:18-21`) — phase 1 of the
/// scan: the joined `media_file` columns with the JSON shims still unparsed
/// (Go inherits them from `dbMediaFile`), plus the entry's own columns.
///
/// Like `dbPlaylistTrack`, this struct owns NO `structs:"-"` shim of its own, so
/// phase 2 ([`crate::hydrate::scrobble_buffer_post_scan`]) is a pure delegation
/// plus the id overwrite.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrobbleBufferRow {
    /// `dbMediaFile`'s record, shims still empty.
    pub record: MediaFileRecord,
    /// The raw `participants` TEXT column, unparsed (the inherited
    /// `dbMediaFile.Participants`, `mediafile_repository.go:31`). Decoded
    /// UNGUARDED, and a failure keeps Go's **media-file** prefix
    /// (`parsing media_file from db: …`), because the buffer hook adds none of its
    /// own (`scrobble_buffer_repository.go:23-26`).
    pub participants: String,
    /// The raw `tags` TEXT column, unparsed (the inherited `dbMediaFile.Tags`,
    /// `:32`).
    pub tags: String,
    /// `scrobble_buffer.id`. Read as the field dbx binds for the duplicated name
    /// `id` (see [`crate::hydrate::id_field_from_row`]).
    pub entry_id: String,
    /// `scrobble_buffer.service`.
    pub service: String,
    /// `scrobble_buffer.user_id`.
    pub user_id: String,
    /// `scrobble_buffer.play_time`, Go's non-pointer `time.Time`.
    pub play_time: DateTime<Utc>,
    /// `scrobble_buffer.enqueue_time`, Go's non-pointer `time.Time`.
    pub enqueue_time: DateTime<Utc>,
    /// `scrobble_buffer.media_file_id` — the FK and the source of the hydrated
    /// media file's `id`.
    pub media_file_id: String,
}

impl ScrobbleBufferRow {
    /// `dbScrobbleBuffer.PostScan` (`scrobble_buffer_repository.go:23-30`): thin
    /// delegation onto [`crate::hydrate::scrobble_buffer_post_scan`], matching
    /// [`crate::mediafile_repo::MediaFileRow::into_record`]'s shape.
    ///
    /// # Errors
    /// [`crate::hydrate::HydrateError::Decoded`] for a bad `participants`/`tags`
    /// cell (carrying Go's media-file prefix), or a transparent SQLite error.
    pub fn into_record(self) -> Result<ScrobbleEntryRecord, crate::hydrate::HydrateError> {
        crate::hydrate::scrobble_buffer_post_scan(self)
    }
}

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

/// Failures of the executed scrobble-buffer read side.
#[derive(Debug, thiserror::Error)]
pub enum ScrobbleBufferError {
    /// The statement failed, or a row's hydration failed. `Next` is `queryOne`, so a
    /// `PostScan` failure surfaces from dbx's `ScanStruct` hook block
    /// (`dbx@v1.12.0/rows.go:92-102`, returning at `:97-99` after the `Scan` at
    /// `:88-90`); a decode failure arrives here too — via
    /// [`crate::hydrate::HydrateError::into_sqlite`].
    ///
    /// This is also how Go reports a queue entry whose `media_file` is GONE:
    /// `Next`'s LEFT JOIN yields NULL `m.*`, and `database/sql` aborts the scan on
    /// the first NULL cell (`m.id`, index 0), so the caller gets
    /// `sql: Scan error on column index 0, name "id": converting NULL to string is
    /// unsupported` (`database/sql/convert.go:535`) — executed against the real
    /// repository. It is NOT `NotFound`, so an entry with a missing media file is
    /// NOT treated as an empty queue.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// `scrobbleBufferRepository` (`scrobble_buffer_repository.go:11-13`) bound to one
/// pooled connection — the executed READ side. Go reaches the DB through the
/// ambient ctx; this crate threads the lease instead (the `sqlrepo.rs` DI freeze).
pub struct ScrobbleBufferRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> ScrobbleBufferRepo<'a> {
    /// `NewScrobbleBufferRepository` (`:32-38`).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Next` (`:65-90`): the head of one user's queue for one service, hydrated
    /// through both phases of the seam.
    ///
    /// `None` is Go's `(nil, nil)`: an EMPTY queue is not an error
    /// (`:78-80`, `errors.Is(err, model.ErrNotFound) ⇒ return nil, nil`), which
    /// Go reaches because `queryOne` maps `sql.ErrNoRows` to `model.ErrNotFound`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`). Executed:
    /// after draining the queue `Next` returns `err == nil` with a nil entry, and
    /// `errors.Is(err, model.ErrNotFound)` is false.
    ///
    /// Ordering is the statement's, not the port's: `ORDER BY play_time, s.rowid
    /// LIMIT 1`. Go stores timestamps as text, so equal `play_time` cells are
    /// broken by `s.rowid` ASCENDING — FIFO by insert order (executed: two rows at
    /// the same `play_time`, `rowid` 1 then 2; the first `Next` yields the rowid-1
    /// entry, and only after `Dequeue` does the rowid-2 entry surface).
    ///
    /// # Errors
    /// [`ScrobbleBufferError::Sqlite`] for a statement failure or a hydration
    /// failure — including Go's NULL-into-`string` scan failure for an entry whose
    /// `media_file` row is gone (see that variant). An entry with a missing media
    /// file is therefore an ERROR, never `None`.
    ///
    /// Go's follow-up `getParticipants` call (`:84`) is NOT performed here, so
    /// [`ScrobbleEntryRecord::participants`] carries the JSON-decoded map the seam
    /// hydrated — Go's alias value BEFORE its re-read, not Go's final value. See
    /// the module note for the executed replace/keep behavior of that re-read.
    pub fn next(
        &self,
        service: &str,
        user_id: &str,
    ) -> Result<Option<ScrobbleEntryRecord>, ScrobbleBufferError> {
        let statement = next_sql(service, user_id);
        let row = crate::execution::query_optional(
            self.conn,
            &statement,
            crate::hydrate::row_to_scrobble_buffer,
        )?;
        // Go's `nil, nil` becomes `Ok(None)`; a `PostScan` failure becomes a
        // column-conversion error, exactly as dbx wraps it.
        match row {
            None => Ok(None),
            Some(raw) => Ok(Some(
                raw.into_record()
                    .map_err(crate::hydrate::HydrateError::into_sqlite)?,
            )),
        }
    }

    /// `Dequeue` (`:91-93`): delete the entry `next` returned, keyed by its id.
    ///
    /// # Errors
    /// SQLite failures. Like Go's `delete`, a missing row is NOT an error
    /// (`sql_base_repository.go:645-648` — only `deleteByID` reports one).
    pub fn dequeue(&self, entry_id: &str) -> Result<usize, ScrobbleBufferError> {
        Ok(crate::execution::execute(
            self.conn,
            &dequeue_sql(entry_id),
        )?)
    }

    /// `Length` (`:99-100`) via the base `count()`
    /// (`sql_base_repository.go:566-576`): every buffered row, joined to nothing,
    /// so an entry whose `media_file` is gone is still counted (Go's `count` has no
    /// join to drop it).
    ///
    /// # Errors
    /// SQLite failures.
    pub fn length(&self) -> Result<i64, ScrobbleBufferError> {
        let statement = length_sql();
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| {
                row.get::<usize, i64>(0)
            })?
            .unwrap_or(0),
        )
    }
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
