//! Port of `persistence/share_repository.go` (205L) and the share-service
//! visit/expiry path of `core/share.go:38-58`.
//!
//! Scope: the share token (`share.id` — the nanoid share token of
//! `core/share.go:63-79`, the repository has no separate token column),
//! the expiry check (`core/share.go:44-47`: an `expires_at` in the past is
//! `model.ErrExpired`, `model/errors.go:13`), the visit-count bookkeeping
//! (`core/share.go:48-51` updates `last_visited_at`/`visit_count` via
//! `Update(ctx, id, *share, "last_visited_at", "visit_count")`), and
//! delete-by-token (`share_repository.go:10-14`, `deleteOwnedAll`,
//! `sql_base_repository.go:542-552`).
//!
//! Deliberately absent (other lanes/slices own their tables and models):
//! `loadMedia` (`share_repository.go:78-133`) with its `ownerContext`
//! (`:135-145`), the album/media-file/playlist re-hydration, and
//! `sortByIdPosition`. They sit AFTER the row read in `Get`/`GetAll`
//! (`:27-33,37-53`) — the ported read stops at the materialized row.
//!
//! Ownership: Go derives the logged-in user from the ambient `ctx`
//! (`sql_base_repository.go:53-57` + `ownerFilter`/`addRestriction`
//! `:67-101`). The crate has no ambient context (see `sqlrepo.rs`/`store.rs`
//! DI rule), so the caller passes the effective restriction explicitly as
//! the owner argument: `None` is admin/headless (Go's `ownerFilter() == nil`),
//! `Some(user_id)` is a normal user; [`INVALID_USER_ID`] resolves to `None`.
//! `deleteOwnedAll` (`:542-552`) classifies a miss through
//! `classifyOwnedWriteMiss` (`:554-564`) into not-found/permission-denied;
//! that classifier needs the full `model.User` (admin bit), so the port
//! pins the miss as [`ShareError::NotFound`] and leaves the permission
//! branch open (residual risk).
//!
//! Timestamps: Go binds `time.Time` values (go-sqlite3 writes them as sqlite
//! datetime text); the port uses epoch-seconds integers — the `Time` unit
//! of `nd-dbschema`'s `optimize.rs:53` — so expiry is a numeric compare
//! inside sqlite rather than a Go `time.Before` (`core/share.go:45`).
//! Execution runs on the crate's store seam (`rusqlite::Connection`,
//! `store.rs`); Go's `executeSQL`/`queryOne` (`sql_base_repository.go:336-355,375-388`)
//! row counts and `ErrNoRows → ErrNotFound` mapping are reproduced here.

use rusqlite::{Connection, Error as RsError, OptionalExtension};

use crate::sql_bookmarks::INVALID_USER_ID;
use crate::sqlrepo::{Repository, Value};

/// `r.tableName` (`share_repository.go:16` via `registerModel(&model.Share{})`,
/// `sql_base_repository.go:104-110`).
pub const SHARE_TABLE: &str = "share";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// Epoch-seconds timestamp unit (see module header for the go-sqlite3
/// datetime-text deviation).
pub type Time = i64;

/// A `SET column = value` pair, the values squirrel's `SetMap` threads to the
/// UPDATE (`sql_base_repository.go:506-515` via `updateOwned`).
#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    /// The column name — a repository-registered identifier, never user text.
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
}

/// A share row (the `structs:""` column spellings of `model/share.go:15-32`).
/// Optional datetimes are epoch seconds ([`Time`]); `expires_at = 0` means
/// "never expires" (Go's zero `*time.Time`, `core/share.go:44 IsZero()`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShareRecord {
    /// `id` — the share token.
    pub id: String,
    /// `user_id` — server-managed owner (`core/share.go:83-95`).
    pub user_id: String,
    /// `expires_at` (0 = never).
    pub expires_at: Time,
    /// `last_visited_at`.
    pub last_visited_at: Time,
    /// `created_at` — never overwritten on update
    /// (`selectUpdateColumns`, `sql_base_repository.go:605`).
    pub created_at: Time,
    /// `updated_at`.
    pub updated_at: Time,
    /// `resource_ids` (comma-separated entity ids).
    pub resource_ids: String,
    /// `resource_type`.
    pub resource_type: String,
    /// `contents`.
    pub contents: String,
    /// `format`.
    pub format: String,
    /// `downloadable`.
    pub downloadable: bool,
    /// `description`.
    pub description: String,
    /// `max_bit_rate`.
    pub max_bit_rate: i64,
    /// `visit_count`.
    pub visit_count: i64,
}

/// The row [`ShareRepo::get`] materializes: the record plus the joined
/// `user_name as username` (`share_repository.go:20-22`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShareWithOwner {
    /// The share row.
    pub share: ShareRecord,
    /// `username` — the joined `user.user_name`, read-only (not a `share`
    /// column, never written).
    pub username: String,
}

/// Failures surfaced by the share repository.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    /// The SQLite statement failed (write or read).
    #[error(transparent)]
    Sqlite(#[from] RsError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`).
    #[error("record not found")]
    NotFound,

    /// The share's `expires_at` has passed — Go's `model.ErrExpired`
    /// (`model/errors.go:13`, raised at `core/share.go:44-47`).
    #[error("share expired")]
    Expired,

    /// The row exists but the caller may not touch it — Go's
    /// `rest.ErrPermissionDenied` via `classifyOwnedWriteMiss`
    /// (`sql_base_repository.go:554-564`).
    #[error("not allowed to modify this record")]
    PermissionDenied,
}

/// `ownerFilter`/`addRestriction` input resolution
/// (`sql_base_repository.go:67-101`): the invalid (headless) user id and the
/// empty id both mean "no ownership restriction", like Go's
/// `usr.ID != invalidUserId` check plus admin's nil filter.
#[must_use]
pub fn owner_restriction(owner: Option<String>) -> Option<String> {
    owner.filter(|user| user != INVALID_USER_ID && !user.is_empty())
}

/// `selectShare(ctx)` (`share_repository.go:18-22`): `Select().From("share")`
/// with the joined owner row for `user_name as username`, restricted to
/// `owner` when one applies (`r.addRestriction(ctx)`,
/// `sql_base_repository.go:77-85`).
#[must_use]
pub fn share_select(owner: Option<String>) -> Repository {
    let sel = Repository::new(SHARE_TABLE)
        .select(["share.*", "user_name as username"])
        .inner_join("user u", "u.id = share.user_id");
    match owner_restriction(owner) {
        Some(user) => sel.where_eq("share.user_id", user),
        None => sel,
    }
}

/// `Get`/`Read` (`share_repository.go:26-33,161-165`): the share select
/// narrowed to one token.
#[must_use]
pub fn get_sql(id: &str, owner: Option<String>) -> Sql {
    share_select(owner)
        .where_eq("share.id", id)
        .to_sql()
        .expect("share select always has its table")
}

/// `Exists` (`share_repository.go:34-36` via `exists`,
/// `sql_base_repository.go:488-493`): `count(*) as exist` over the
/// ownership-restricted row.
#[must_use]
pub fn exists_sql(id: &str, owner: Option<String>) -> Sql {
    let sel = Repository::new(SHARE_TABLE).select(["count(*) as exist"]);
    let sel = match owner_restriction(owner) {
        Some(user) => sel.where_eq("id", id).where_eq("user_id", user),
        None => sel.where_eq("id", id),
    };
    sel.to_sql().expect("exists query always has its table")
}

/// `CountAll` (`share_repository.go:103-105` via `count`,
/// `sql_base_repository.go:565-575`): the share select's columns replaced by
/// `count(distinct share.id) as count` (the join stays — Go's
/// `RemoveColumns().Columns(...)` keeps the FROM/JOIN chain).
#[must_use]
pub fn count_sql(owner: Option<String>) -> Sql {
    share_select(owner)
        .count(SHARE_TABLE)
        .to_sql()
        .expect("count query always has its table")
}

/// `Update` (`share_repository.go:116-124` via `updateOwned`,
/// `sql_base_repository.go:506-515`): SET `cols` — `updated_at` appended by
/// the caller when Go appends it (`:121`) — restricted to the row token and
/// to `owner` when one applies (`ownedRow`, `sql_base_repository.go:97-101`).
/// `user_id` is never a SET column: ownership is immutable on update
/// (`:504` `delete(updateValues, "user_id")`).
#[must_use]
pub fn update_sql(id: &str, cols: &[Col], owner: Option<String>) -> Sql {
    let set = cols
        .iter()
        .map(|c| format!("{} = ?", c.column))
        .collect::<Vec<_>>()
        .join(", ");
    let mut args = cols.iter().map(|c| c.value.clone()).collect::<Vec<Value>>();
    args.push(Value::Text(id.to_string()));
    let where_sql = match owner_restriction(owner) {
        Some(user) => {
            args.push(Value::Text(user));
            "id = ? AND user_id = ?"
        }
        None => "id = ?",
    };
    (
        format!("UPDATE {SHARE_TABLE} SET {set} WHERE {where_sql}"),
        args,
    )
}

/// `Delete` (`share_repository.go:10-14` via `deleteOwnedAll`,
/// `sql_base_repository.go:542-552`): one ownership-restricted DELETE per
/// token, in order.
#[must_use]
pub fn delete_tokens_sql(ids: &[String], owner: Option<String>) -> Vec<Sql> {
    let owner = owner_restriction(owner);
    ids.iter()
        .map(|id| {
            let mut args = vec![Value::Text(id.to_string())];
            let where_sql = match &owner {
                Some(user) => {
                    args.push(Value::Text(user.clone()));
                    "id = ? AND user_id = ?"
                }
                None => "id = ?",
            };
            (format!("DELETE FROM {SHARE_TABLE} WHERE {where_sql}"), args)
        })
        .collect()
}

/// `NewShareRepository` (`share_repository.go:11-18`): a repository bound to
/// one connection (the pooled lease; Go binds a `dbx.Builder`).
#[derive(Debug)]
pub struct ShareRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ShareRepo<'a> {
    /// `NewShareRepository`: bind to `conn`.
    #[must_use]
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// `Get`/`Read` (`share_repository.go:26-33,161-165`): the joined row for
    /// `id`, or `model.ErrNotFound` on a miss (`queryOne`'s `ErrNoRows`
    /// mapping, `sql_base_repository.go:384`). Expiry is NOT checked here —
    /// Go checks it in the service ([`Self::load`], `core/share.go:44-47`).
    ///
    /// # Errors
    /// [`ShareError::NotFound`] on a miss (or an owner-filtered miss);
    /// [`ShareError::Sqlite`] when the query fails.
    pub fn get(&self, id: &str, owner: Option<String>) -> Result<ShareWithOwner, ShareError> {
        let (sql, args) = get_sql(id, owner);
        let row = self
            .conn
            .query_row(
                &sql,
                rusqlite::params_from_iter(bind_values(&args)),
                row_to_share,
            )
            .optional()?;
        row.ok_or(ShareError::NotFound)
    }

    /// `Exists` (`share_repository.go:34-36`): `res.Exist > 0`
    /// (`sql_base_repository.go:492`).
    ///
    /// # Errors
    /// [`ShareError::Sqlite`] when the query fails.
    pub fn exists(&self, id: &str, owner: Option<String>) -> Result<bool, ShareError> {
        let (sql, args) = exists_sql(id, owner);
        let exist: i64 = self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(bind_values(&args)),
            |row| row.get(0),
        )?;
        Ok(exist > 0)
    }

    /// `CountAll` (`share_repository.go:103-105`): the ownership-restricted
    /// row count.
    ///
    /// # Errors
    /// [`ShareError::Sqlite`] when the query fails.
    pub fn count_all(&self, owner: Option<String>) -> Result<i64, ShareError> {
        let (sql, args) = count_sql(owner);
        let count: i64 = self.conn.query_row(
            &sql,
            rusqlite::params_from_iter(bind_values(&args)),
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// `Update` (`share_repository.go:116-124`): execute the ownership-
    /// restricted UPDATE; a zero-row result classifies the miss
    /// (`runRowWrite` + `classifyOwnedWriteMiss`,
    /// `sql_base_repository.go:531-539,554-564`). The caller supplies the SET
    /// pairs ([`update_sql`]); Go's `s.UpdatedAt = time.Now()` plus the
    /// `updated_at` append (`:121`) are the caller's clock, not the
    /// repository's — pass `updated_at` in `cols` to match Go.
    ///
    /// # Errors
    /// [`ShareError::NotFound`] when no row matched (the port's pinned
    /// miss class, see module docs); [`ShareError::Sqlite`] on failure.
    pub fn update(&self, id: &str, cols: &[Col], owner: Option<String>) -> Result<(), ShareError> {
        let (sql, args) = update_sql(id, cols, owner);
        let count = self
            .conn
            .execute(&sql, rusqlite::params_from_iter(bind_values(&args)))?;
        if count == 0 {
            return Err(ShareError::NotFound);
        }
        Ok(())
    }

    /// `Delete` (`share_repository.go:10-14`): delete every token, stopping
    /// at the first failure (`deleteOwnedAll`, `sql_base_repository.go:542-552`).
    /// A zero-row delete classifies the miss exactly like `Update`
    /// (`deleteOwned` → `runRowWrite` → `classifyOwnedWriteMiss`).
    ///
    /// # Errors
    /// [`ShareError::NotFound`] on the first miss (pinned class, see module
    /// docs); [`ShareError::Sqlite`] on failure.
    pub fn delete(&self, ids: &[String], owner: Option<String>) -> Result<(), ShareError> {
        for (sql, args) in delete_tokens_sql(ids, owner) {
            let count = self
                .conn
                .execute(&sql, rusqlite::params_from_iter(bind_values(&args)))?;
            if count == 0 {
                return Err(ShareError::NotFound);
            }
        }
        Ok(())
    }

    /// `Save` (`share_repository.go:99-112` via `put`,
    /// `sql_base_repository.go:618-641`): the caller supplies `id` (the
    /// nanoid minted by `core/share.go:63-79` `newId`) and both stamps
    /// (`s.CreatedAt`/`s.UpdatedAt = time.Now()`, `:106-107`) — the
    /// repository port writes the row verbatim and never invents an id or
    /// clock. An empty `id` matches Go's `put` INSERT leg only when the
    /// caller pre-mints the id; the minting itself lives with the wrapper
    /// (`shareRepositoryWrapper.Save`, `core/share.go:83-115`), not here.
    ///
    /// # Errors
    /// [`ShareError::Sqlite`] when the INSERT fails (e.g. an unknown owner).
    pub fn save(&self, record: &ShareRecord) -> Result<(), ShareError> {
        self.conn.execute(
            &format!(
                "INSERT INTO {SHARE_TABLE} (id, user_id, expires_at, last_visited_at, \
                 created_at, updated_at, resource_ids, resource_type, contents, format, \
                 downloadable, description, max_bit_rate, visit_count) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ),
            rusqlite::params![
                record.id,
                record.user_id,
                record.expires_at,
                record.last_visited_at,
                record.created_at,
                record.updated_at,
                record.resource_ids,
                record.resource_type,
                record.contents,
                record.format,
                record.downloadable,
                record.description,
                record.max_bit_rate,
                record.visit_count,
            ],
        )?;
        Ok(())
    }

    /// `load` (`core/share.go:38-58`): fetch the share by token, reject an
    /// expired one (`model.ErrExpired`, `:44-47` — a nonzero `expires_at`
    /// in the past), then bump `visit_count`/`last_visited_at` through
    /// `Update(ctx, id, *share, "last_visited_at", "visit_count")`
    /// (`:48-51`, including the `updated_at` append at
    /// `share_repository.go:121`). The visit bump failure is logged and
    /// swallowed in Go (`core/share.go:52-54` `log.Warn`); the port returns
    /// the loaded share either way, exposing the flag for the caller's log.
    /// A `None` owner reads as admin/headless ([`owner_restriction`]).
    ///
    /// # Errors
    /// [`ShareError::NotFound`] when the token matches no readable row;
    /// [`ShareError::Expired`] when `expires_at` has passed;
    /// [`ShareError::Sqlite`] when the visit-count UPDATE fails.
    pub fn load(
        &self,
        id: &str,
        now: Time,
        owner: Option<String>,
    ) -> Result<(ShareWithOwner, bool), ShareError> {
        let found = self.get(id, owner.clone())?;
        // core/share.go:44-47: only a non-zero ExpiresAt in the past expires.
        if found.share.expires_at != 0 && found.share.expires_at < now {
            return Err(ShareError::Expired);
        }
        // core/share.go:48-51 + share_repository.go:116-124: the two visit
        // columns plus the appended updated_at.
        let cols = vec![
            Col::integer("visit_count", found.share.visit_count + 1),
            Col::integer("last_visited_at", now),
            Col::integer("updated_at", now),
        ];
        let bumped = self.update(id, &cols, owner).is_ok();
        Ok((found, bumped))
    }
}

/// Materializes `SELECT share.*, user_name as username` (the column order
/// `selectShare` pins, `share_repository.go:20-21`).
fn row_to_share(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareWithOwner> {
    Ok(ShareWithOwner {
        share: ShareRecord {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            expires_at: row.get("expires_at")?,
            last_visited_at: row.get("last_visited_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            resource_ids: row.get("resource_ids")?,
            resource_type: row.get("resource_type")?,
            contents: row.get("contents")?,
            format: row.get("format")?,
            downloadable: row.get("downloadable")?,
            description: row.get("description")?,
            max_bit_rate: row.get("max_bit_rate")?,
            visit_count: row.get("visit_count")?,
        },
        username: row.get("username")?,
    })
}

/// Binds [`Value`] args positionally — rusqlite's `?` binding, the direct
/// counterpart of dbx `Bind(args)` (`sql_base_repository.go:344`). Bool
/// maps to 0/1, matching squirrel's SQLite rendering (see `sqlrepo.rs`).
fn bind_values(args: &[Value]) -> Vec<rusqlite::types::Value> {
    args.iter()
        .map(|v| match v {
            Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
            Value::Integer(i) => rusqlite::types::Value::Integer(*i),
            Value::Real(x) => rusqlite::types::Value::Real(*x),
            Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
            Value::Null => rusqlite::types::Value::Null,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `share` + `user` DDL (`schema_ddl.rs:57,68`) and one owner
    /// row, so the `selectShare` join and the save FK both behave.
    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE \"user\" ( id varchar(255) not null primary key, \
             user_name varchar(255) default '' not null unique, \
             created_at datetime not null, updated_at datetime not null, \
             token_epoch INTEGER NOT NULL DEFAULT 0 );
             CREATE TABLE \"share\" ( id varchar(255) not null primary key, \
             expires_at datetime, last_visited_at datetime, resource_ids varchar not null, \
             created_at datetime, updated_at datetime, \
             user_id varchar(255) not null constraint share_user_id_fk references user \
             on update cascade on delete cascade, downloadable bool not null default false, \
             description varchar not null default '', resource_type varchar not null default '', \
             contents varchar not null default '', format varchar not null default '', \
             max_bit_rate integer not null default 0, visit_count integer not null default 0 );",
        )
        .expect("share schema creates");
        conn.execute(
            "INSERT INTO \"user\" (id, user_name, created_at, updated_at) \
             VALUES ('user1', 'alice', 0, 0)",
            [],
        )
        .expect("owner row seeds");
        conn
    }

    fn record(id: &str, expires_at: Time) -> ShareRecord {
        ShareRecord {
            id: id.to_string(),
            user_id: "user1".to_string(),
            expires_at,
            last_visited_at: 0,
            created_at: 100,
            updated_at: 100,
            resource_ids: "al-1".to_string(),
            resource_type: "album".to_string(),
            contents: "Some Album".to_string(),
            format: "raw".to_string(),
            downloadable: false,
            description: "desc".to_string(),
            max_bit_rate: 0,
            visit_count: 0,
        }
    }

    #[test]
    fn generated_sql_pins_the_join_and_parameter_vectors() {
        // Arrange: selectShare's shape (share_repository.go:18-22) — the
        // username join plus the id filter, admin (no owner restriction).
        // Act:
        let (sql, args) = get_sql("tok1", None);
        let (exists, exists_args) = exists_sql("tok1", None);
        // Assert: share.* then the joined username, owner arg last.
        assert_eq!(
            sql,
            "SELECT share.*, user_name as username FROM share \
             INNER JOIN user u ON u.id = share.user_id WHERE share.id = ?"
        );
        assert_eq!(args, vec![Value::Text("tok1".to_string())]);
        assert_eq!(exists, "SELECT count(*) as exist FROM share WHERE id = ?");
        assert_eq!(exists_args, vec![Value::Text("tok1".to_string())]);
    }

    #[test]
    fn generated_sql_pins_the_owner_restriction() {
        // Arrange: a non-admin reader — addRestriction appends user_id
        // (sql_base_repository.go:77-85), the id filter stays first.
        // Act:
        let (sql, args) = get_sql("tok1", Some("user1".to_string()));
        let del_sql = delete_tokens_sql(&["tok1".to_string()], Some("user1".to_string()));
        // Assert:
        assert!(sql.ends_with("WHERE share.user_id = ? AND share.id = ?"));
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Text("tok1".to_string()),
            ]
        );
        assert_eq!(
            del_sql[0],
            (
                "DELETE FROM share WHERE id = ? AND user_id = ?".to_string(),
                vec![
                    Value::Text("tok1".to_string()),
                    Value::Text("user1".to_string()),
                ]
            )
        );
        // The headless invalid user id is no restriction at all
        // (sql_base_repository.go:67-75 ownerFilter nil rule).
        let (headless, headless_args) = get_sql("tok1", Some(INVALID_USER_ID.to_string()));
        assert_eq!(headless, get_sql("tok1", None).0);
        assert_eq!(headless_args, vec![Value::Text("tok1".to_string())]);
    }

    #[test]
    fn update_sql_sets_columns_and_appends_the_token_arg() {
        // Arrange: the visit-count UPDATE of core/share.go:48-51 —
        // Update(ctx, id, *share, "last_visited_at", "visit_count") plus
        // share_repository.go:121's updated_at append, non-admin owner.
        let cols = vec![
            Col::integer("visit_count", 2),
            Col::integer("last_visited_at", 500),
            Col::integer("updated_at", 500),
        ];
        // Act:
        let (sql, args) = update_sql("tok1", &cols, Some("user1".to_string()));
        // Assert: SET columns in order, then id, then the owner restriction.
        assert_eq!(
            sql,
            "UPDATE share SET visit_count = ?, last_visited_at = ?, updated_at = ? \
             WHERE id = ? AND user_id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Integer(2),
                Value::Integer(500),
                Value::Integer(500),
                Value::Text("tok1".to_string()),
                Value::Text("user1".to_string()),
            ]
        );
    }

    #[test]
    fn get_reads_the_joined_row_and_misses_report_not_found() {
        // Arrange: the mock's Get (mock_share_repo.go:31-45): a known id
        // returns the stored entity, anything else is model.ErrNotFound.
        let conn = conn();
        let repo = ShareRepo::new(&conn);
        repo.save(&record("tok1", 0)).expect("row seeds");
        // Act:
        let hit = repo.get("tok1", None);
        let miss = repo.get("nope", None);
        let foreign = repo.get("tok1", Some("user2".to_string()));
        // Assert:
        let found = hit.expect("stored token reads");
        assert_eq!(found.share, record("tok1", 0));
        assert_eq!(found.username, "alice".to_string());
        assert!(matches!(miss, Err(ShareError::NotFound)));
        assert!(matches!(foreign, Err(ShareError::NotFound)));
    }

    #[test]
    fn exists_and_count_all_follow_the_owner_restriction() {
        // Arrange: two owners' shares; Exists checks the token under the
        // reader's restriction (share_repository.go:34-36), CountAll counts
        // the restricted set (share_repository.go:103-105).
        let conn = conn();
        conn.execute(
            "INSERT INTO \"user\" (id, user_name, created_at, updated_at) \
             VALUES ('user2', 'bob', 0, 0)",
            [],
        )
        .expect("second owner seeds");
        let repo = ShareRepo::new(&conn);
        let mut other = record("tok2", 0);
        other.user_id = "user2".to_string();
        repo.save(&record("tok1", 0)).expect("row 1 seeds");
        repo.save(&other).expect("row 2 seeds");
        // Act:
        let admin_exists = repo.exists("tok2", None);
        let owner_exists = repo.exists("tok2", Some("user1".to_string()));
        let admin_count = repo.count_all(None);
        let owner_count = repo.count_all(Some("user1".to_string()));
        // Assert:
        assert!(admin_exists.expect("admin sees any token"));
        assert!(!owner_exists.expect("owner restriction applies"));
        assert_eq!(admin_count.expect("admin counts all"), 2);
        assert_eq!(owner_count.expect("owner counts own"), 1);
    }

    #[test]
    fn update_touches_only_the_owned_row_and_reports_a_miss() {
        // Arrange: the mock's Update (mock_share_repo.go:23-30) records the
        // id and cols; the real repository additionally refuses a foreign
        // row via the ownership WHERE clause (sql_base_repository.go:506-515).
        let conn = conn();
        let repo = ShareRepo::new(&conn);
        repo.save(&record("tok1", 0)).expect("row seeds");
        let cols = vec![Col::text("description", "new desc")];
        // Act:
        let owned = repo.update("tok1", &cols, Some("user1".to_string()));
        let foreign = repo.update("tok1", &cols, Some("user2".to_string()));
        let after = repo.get("tok1", None);
        // Assert:
        owned.expect("owned row updates");
        assert!(matches!(foreign, Err(ShareError::NotFound)));
        assert_eq!(
            after.expect("row still reads").share.description,
            "new desc".to_string()
        );
    }

    #[test]
    fn delete_removes_by_token_and_stops_at_a_miss() {
        // Arrange: deleteOwnedAll (share_repository.go:10-14) deletes each
        // token in order, ownership-restricted.
        let conn = conn();
        let repo = ShareRepo::new(&conn);
        repo.save(&record("tok1", 0)).expect("row 1 seeds");
        repo.save(&record("tok2", 0)).expect("row 2 seeds");
        // Act:
        repo.delete(&["tok1".to_string(), "tok2".to_string()], None)
            .expect("both tokens delete");
        let miss = repo.delete(&["tok1".to_string()], None);
        let foreign = repo.delete(&["tok3".to_string()], Some("user2".to_string()));
        // Assert:
        assert!(matches!(miss, Err(ShareError::NotFound)));
        assert!(matches!(foreign, Err(ShareError::NotFound)));
        assert!(!repo.exists("tok1", None).expect("count reads"));
        assert!(!repo.exists("tok2", None).expect("count reads"));
    }

    #[test]
    fn load_rejects_expired_tokens_and_bumps_the_visit_count() {
        // Arrange: core/share.go:38-58 — an expired token raises
        // model.ErrExpired (:44-47), a live one bumps visit_count and
        // last_visited_at (:48-51). expires_at = 0 never expires (:44).
        let conn = conn();
        let repo = ShareRepo::new(&conn);
        repo.save(&record("expired", 100))
            .expect("expired row seeds");
        repo.save(&record("live", 0)).expect("live row seeds");
        repo.save(&record("later", 900)).expect("later row seeds");
        // Act:
        let expired = repo.load("expired", 500, None);
        let (loaded, bumped) = repo.load("live", 500, None).expect("live token loads");
        let (untouched, _) = repo
            .load("later", 500, None)
            .expect("future-expiry token loads");
        // Assert:
        assert!(matches!(expired, Err(ShareError::Expired)));
        assert!(bumped, "visit bump succeeded");
        assert_eq!(loaded.share.visit_count, 0);
        let reloaded = repo.get("live", None).expect("bumped row reads");
        assert_eq!(reloaded.share.visit_count, 1);
        assert_eq!(reloaded.share.last_visited_at, 500);
        assert_eq!(reloaded.share.updated_at, 500);
        assert_eq!(untouched.share.visit_count, 0);
    }

    #[test]
    fn load_reports_not_found_for_unknown_tokens() {
        // Arrange: the mock's Get contract (mock_share_repo.go:31-45):
        // an unknown token is model.ErrNotFound, surfaced by load too.
        let conn = conn();
        let repo = ShareRepo::new(&conn);
        // Act:
        let miss = repo.load("nope", 500, None);
        // Assert:
        assert!(matches!(miss, Err(ShareError::NotFound)));
    }
}
