//! Port of `persistence/user_props_repository.go` (62L).
//!
//! Go's `userPropsRepository` is a bare `sqlRepository` over the fixed
//! `user_props` table (composite PK `user_id, key`): `Put` is an upsert
//! (UPDATE first; INSERT only when the UPDATE matched nothing —
//! `user_props_repository.go:17-28`), `Get` selects the single `value` column
//! and maps a miss to `model.ErrNotFound` (`queryOne`, `sql_base_repository.go:375-388`),
//! `DefaultGet` substitutes the default on not-found
//! (`user_props_repository.go:45-53`), and `Delete` is the conditional
//! `delete()` (`sql_base_repository.go:644-647`) — which unlike
//! `deleteByID` succeeds silently on a miss. All four behaviors are pinned
//! by the mock contract in `tests/mock_user_props_repo.go` (Put stores,
//! Get returns `model.ErrNotFound` for a miss, Delete of a missing key
//! returns `model.ErrNotFound` — see below: the repository itself keeps
//! Go's silent `delete()`; the mock's stricter not-found is a mock-only
//! convenience the interface does not require, so the port follows the
//! real repository).
//!
//! Execution uses `rusqlite::Connection` directly (the crate's store seam,
//! `store.rs`): Go hands `(sql, args)` from `toSQL()`
//! (`sql_base_repository.go:358-372`) to dbx; the port executes the
//! equivalent statements with every value bound to a `?` placeholder —
//! the fixed table/column names are the repository's registered names,
//! never caller text (safety rule, `sqlrepo.rs` module docs).

use rusqlite::{Connection, Error as RsError, OptionalExtension};

/// `r.tableName` (`user_props_repository.go:13`).
pub const USER_PROPS_TABLE: &str = "user_props";

/// Failures of the user-props repository.
#[derive(Debug, thiserror::Error)]
pub enum UserPropsError {
    /// The SQLite statement failed (write or read).
    #[error(transparent)]
    Sqlite(#[from] RsError),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10`).
    #[error("record not found")]
    NotFound,
}

/// `NewUserPropsRepository` (`user_props_repository.go:11-16`): a repository
/// bound to one connection (Go binds a `dbx.Builder`; the port threads the
/// connection explicitly per the #S1 freeze — no process-global `Db()`).
#[derive(Debug)]
pub struct UserPropsRepo<'a> {
    conn: &'a Connection,
}

impl<'a> UserPropsRepo<'a> {
    /// `NewUserPropsRepository`: bind to `conn` (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// `Put` (`user_props_repository.go:17-28`): UPDATE `value` where
    /// `(user_id, key)` matches; if no row matched, INSERT the triple.
    /// Both statements carry Go's exact parameter order.
    ///
    /// # Errors
    /// [`UserPropsError::Sqlite`] when either statement fails.
    pub fn put(&self, user_id: &str, key: &str, value: &str) -> Result<(), UserPropsError> {
        // Arrange: Update(r.tableName).Set("value", value)
        // .Where(And{Eq{"user_id": userId}, Eq{"key": key}}) — user_id/key/value.
        let count = self.conn.execute(
            &format!("UPDATE {USER_PROPS_TABLE} SET value = ? WHERE user_id = ? AND key = ?"),
            rusqlite::params![value, user_id, key],
        )?;
        if count > 0 {
            return Ok(());
        }
        // Insert(r.tableName).Columns("user_id", "key", "value")
        // .Values(userId, key, value) — same triple order.
        self.conn.execute(
            &format!("INSERT INTO {USER_PROPS_TABLE} (user_id, key, value) VALUES (?, ?, ?)"),
            rusqlite::params![user_id, key, value],
        )?;
        Ok(())
    }

    /// `Get` (`user_props_repository.go:30-40`): the single `value` for
    /// `(user_id, key)`, or `model.ErrNotFound` when no row matched
    /// (`queryOne`'s `sql.ErrNoRows` mapping, `sql_base_repository.go:384`).
    ///
    /// # Errors
    /// [`UserPropsError::NotFound`] on a miss;
    /// [`UserPropsError::Sqlite`] when the query fails.
    pub fn get(&self, user_id: &str, key: &str) -> Result<String, UserPropsError> {
        // Select("value").From(r.tableName)
        // .Where(And{Eq{"user_id": userId}, Eq{"key": key}}).
        let value: Option<String> = self
            .conn
            .query_row(
                &format!("SELECT value FROM {USER_PROPS_TABLE} WHERE user_id = ? AND key = ?"),
                rusqlite::params![user_id, key],
                |row| row.get(0),
            )
            .optional()?;
        value.ok_or(UserPropsError::NotFound)
    }

    /// `DefaultGet` (`user_props_repository.go:42-53`): `Get` with
    /// `default_value` substituted for `model.ErrNotFound` — other errors
    /// still return the default, as Go does (`:50` returns
    /// `defaultValue, nil` only for ErrNotFound; `:52-53` returns
    /// `defaultValue, err` for everything else, so the value is always the
    /// default when an error is present).
    ///
    /// # Errors
    /// [`UserPropsError::Sqlite`] when the underlying [`Self::get`] query
    /// fails for a reason other than not-found.
    pub fn default_get(
        &self,
        user_id: &str,
        key: &str,
        default_value: &str,
    ) -> Result<String, UserPropsError> {
        match self.get(user_id, key) {
            Ok(value) => Ok(value),
            Err(UserPropsError::NotFound) => Ok(default_value.to_string()),
            Err(other) => Err(other),
        }
    }

    /// `Delete` (`user_props_repository.go:55-57` via `delete()`,
    /// `sql_base_repository.go:644-647`): the conditional DELETE over
    /// `(user_id, key)`. A missing row is NOT an error here — `delete()`
    /// ignores the affected count; only the mock (`mock_user_props_repo.go:49-58`)
    /// reports `model.ErrNotFound`, and the interface consumer
    /// (`model/user_props.go`) does not rely on it.
    ///
    /// # Errors
    /// [`UserPropsError::Sqlite`] when the statement fails.
    pub fn delete(&self, user_id: &str, key: &str) -> Result<(), UserPropsError> {
        // Delete(r.tableName).Where(And{Eq{"user_id": userId}, Eq{"key": key}}).
        self.conn.execute(
            &format!("DELETE FROM {USER_PROPS_TABLE} WHERE user_id = ? AND key = ?"),
            rusqlite::params![user_id, key],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection with the real `user_props` DDL (`schema_ddl.rs:70`),
    /// the same table Go migrates in.
    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute(
            "CREATE TABLE user_props ( user_id varchar not null, key varchar not null, \
             value varchar, constraint user_props_pk primary key (user_id, key) )",
            [],
        )
        .expect("user_props table creates");
        conn
    }

    #[test]
    fn put_inserts_when_missing_and_updates_when_present() {
        // Arrange: the mock's Put semantics (mock_user_props_repo.go:19-26)
        // over the real table — first Put inserts, second Put overwrites.
        let conn = conn();
        let repo = UserPropsRepo::new(&conn);
        // Act:
        repo.put("user1", "theme", "dark").expect("insert leg runs");
        let before = repo.get("user1", "theme").expect("inserted row reads");
        repo.put("user1", "theme", "light")
            .expect("update leg runs");
        let after = repo.get("user1", "theme").expect("updated row reads");
        // Assert:
        assert_eq!(before, "dark".to_string());
        assert_eq!(after, "light".to_string());
        // Put kept exactly one row: Go's UPDATE-then-INSERT upsert.
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM user_props", [], |row| row.get(0))
            .expect("count reads");
        assert_eq!(rows, 1);
    }

    #[test]
    fn get_misses_and_found_rows_match_the_mock_contract() {
        // Arrange: the mock's Get (mock_user_props_repo.go:28-37): stored
        // values read back, everything else is model.ErrNotFound.
        let conn = conn();
        let repo = UserPropsRepo::new(&conn);
        repo.put("user1", "locale", "en").expect("row seeds");
        // Act:
        let hit = repo.get("user1", "locale");
        let miss_key = repo.get("user1", "nope");
        let miss_user = repo.get("user2", "locale");
        // Assert:
        assert_eq!(hit.expect("stored key reads"), "en".to_string());
        assert!(matches!(miss_key, Err(UserPropsError::NotFound)));
        assert!(matches!(miss_user, Err(UserPropsError::NotFound)));
    }

    #[test]
    fn default_get_substitutes_the_default_on_not_found() {
        // Arrange: the mock's DefaultGet (mock_user_props_repo.go:41-53):
        // found value passes through; a miss yields the default, not an error.
        let conn = conn();
        let repo = UserPropsRepo::new(&conn);
        repo.put("user1", "locale", "en").expect("row seeds");
        // Act:
        let hit = repo.default_get("user1", "locale", "pt");
        let miss = repo.default_get("user1", "nope", "pt");
        // Assert:
        assert_eq!(hit.expect("found value passes through"), "en".to_string());
        assert_eq!(miss.expect("default substitutes"), "pt".to_string());
    }

    #[test]
    fn delete_removes_the_row_and_stays_silent_on_a_miss() {
        // Arrange: the repository's Delete is `delete()`
        // (sql_base_repository.go:644-647): no count check, so a second
        // delete of the same key succeeds silently — unlike the mock
        // (mock_user_props_repo.go:49-58) which reports ErrNotFound.
        let conn = conn();
        let repo = UserPropsRepo::new(&conn);
        repo.put("user1", "theme", "dark").expect("row seeds");
        // Act:
        repo.delete("user1", "theme").expect("existing row deletes");
        let gone = repo.get("user1", "theme");
        let repeat = repo.delete("user1", "theme");
        // Assert:
        assert!(matches!(gone, Err(UserPropsError::NotFound)));
        repeat.expect("missing row deletes silently");
    }
}
