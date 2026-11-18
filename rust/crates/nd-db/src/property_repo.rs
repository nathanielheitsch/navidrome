//! Port of `persistence/property_repository.go` (62L) — the `property`
//! key/value table (`persistence.NewPropertyRepository`,
//! `persistence/property_repository.go:9-13`).
//!
//! Go's repository is a bare `sqlRepository` over `property (id, value)`;
//! every method builds one squirrel statement and hands it to
//! `executeSQL`/`queryOne`/`delete`, which materialize through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`. The WRITE legs stop at
//! those [`Sql`] pairs, as in the sibling `sql_*` ports; the READ legs are
//! EXECUTED: [`PropertyRepo`] is `propertyRepository` bound to one leased
//! connection and runs through [`crate::execution`], the port of `queryOne`
//! (`sql_base_repository.go:375-388`), in the shape `radio_repo::RadioRepo`
//! establishes.
//!
//! Values are plain TEXT (`value string` on both the read and the write,
//! `:10,30,33`); Go stores JSON only when a caller hands it a JSON string —
//! the table itself has no JSON handling, so none is ported.
//!
//! `DefaultGet` (`:38-45`) is Go's `Get` plus an `ErrNotFound → defaultValue`
//! fold at execution time, so it ports as [`PropertyRepo::default_get`] on top
//! of [`PropertyRepo::get`]; the SQL shape it wraps is exactly [`get_sql`] and no
//! second statement is built.
//!
//! Storage class on the read: Go scans a non-pointer `string` (`:30-32`), whose
//! arm accepts every non-NULL driver value through `asString`
//! (`database/sql/convert.go:533-544`), so an INTEGER cell in the declared
//! `varchar` column reads as its decimal text — executed against the real
//! repository with `insert into property(id, value) values('num', 7)`,
//! `Get("num")` returned `"7"` with `err=<nil>`. [`crate::hydrate::RowGet::get_text`]
//! accepts the same cell and renders the same text (`hydrate.rs:520-544`). The
//! fresh DDL declares `value varchar(255) default '' not null`
//! (`nd-dbschema/src/schema_ddl.rs:53`), so a NULL cell is not reachable through
//! the schema; were one present, Go's scan errors and `get_text` errors the same
//! way (the decision is recorded on [`PropertyError::Sqlite`]).

use crate::sqlrepo::Value;

/// `r.tableName` (`NewPropertyRepository`, `property_repository.go:12`).
pub const PROPERTY_TABLE: &str = "property";

/// One materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `Put`'s two statements (`property_repository.go:14-27`), in Go's execution
/// order: the UPDATE runs first and the INSERT runs only when the update
/// matched no row (`:19-21`).
#[derive(Debug, Clone, PartialEq)]
pub struct PutSql {
    /// `Update(property).Set("value", value).Where(Eq{"id": id})` (`:15-17`).
    pub update: Sql,
    /// `Insert(property).Columns("id", "value").Values(id, value)` (`:23-24`).
    pub insert: Sql,
}

/// `Put` (`property_repository.go:14-27`): update-then-insert upsert pair.
/// Go binds `value` before `id` on the update (squirrel `Set` before `Where`)
/// and `id` before `value` on the insert (`:23-24`).
#[must_use]
pub fn put_sql(id: &str, value: &str) -> PutSql {
    PutSql {
        update: (
            format!("UPDATE {PROPERTY_TABLE} SET value = ? WHERE id = ?"),
            vec![Value::from(value), Value::from(id)],
        ),
        insert: (
            format!("INSERT INTO {PROPERTY_TABLE} (id, value) VALUES (?, ?)"),
            vec![Value::from(id), Value::from(value)],
        ),
    }
}

/// `Get` (`property_repository.go:29-36`): `Select("value").From(property).
/// Where(Eq{"id": id})` — the row's `value` column only.
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    (
        format!("SELECT value FROM {PROPERTY_TABLE} WHERE id = ?"),
        vec![Value::from(id)],
    )
}

/// `Delete` (`property_repository.go:47-49`): `r.delete(Eq{"id": id})` —
/// `Delete(property).Where(Eq{"id": id})` (`sql_base_repository.go:644-648`).
/// Go's delete succeeds silently on a missing row, so no row count is pinned.
#[must_use]
pub fn delete_sql(id: &str) -> Sql {
    (
        format!("DELETE FROM {PROPERTY_TABLE} WHERE id = ?"),
        vec![Value::from(id)],
    )
}

/// Failures of the executed property reads ([`PropertyRepo`]).
///
/// `Get`/`DefaultGet` have two error paths in Go: `toSQL` (which the fixed table
/// and single `Eq` make unreachable) and `queryOne`'s result, whose
/// `sql.ErrNoRows` is folded into `model.ErrNotFound`
/// (`sql_base_repository.go:375-388`).
#[derive(Debug, thiserror::Error)]
pub enum PropertyError {
    /// The statement failed, or the `value` cell would not scan into Go's
    /// non-pointer `string`. There is no `PostScan` hook here
    /// (`grep -n "dbProperty\|PostScan" persistence/property_repository.go` is
    /// empty), so this is a pure statement/scan failure.
    ///
    /// NULL is the only realistic scan failure and the fresh schema forbids it
    /// (`schema_ddl.rs:53` declares the column `not null` with a `''` default), so
    /// this arm is not reachable through a schema-built DB; it is kept because
    /// Go's scan genuinely fails on a NULL cell in a hand-made database, and
    /// reporting an error there is Go parity rather than inventing an empty value.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// No row matched — Go's `model.ErrNotFound` (`model/errors.go:10` →
    /// `rest.ErrNotFound`, `data not found`, `deluan/rest@v1.0.1/errors.go:14`).
    ///
    /// Executed against the real repository (oracle `/tmp/sd3g1_oracle`):
    /// `Get("missing")` returned `err="data not found"` with the value `""`, and
    /// `Get("k")` after `Delete("k")` returned the same. Go returns the empty
    /// string beside that error (`property_repository.go:33-36`), which the port
    /// cannot carry in a `Result`; no Go caller reads it before checking the error.
    /// [`PropertyRepo::default_get`] folds this variant into its default.
    #[error("data not found")]
    NotFound,
}

/// `propertyRepository` (`property_repository.go:9-13`) bound to one pooled
/// connection — the executed READ side of the property repository.
///
/// Go binds a `dbx.Builder` and reaches the DB through the ambient ctx; this crate
/// has no ambient context (the `sqlrepo.rs` DI freeze), so the caller threads a
/// lease from [`crate::store::SQLStore::with_connection_sync`] (a transaction
/// derefs to a `&Connection`) and every statement runs through
/// [`crate::execution`]. There is no hydration: Go's `queryOne` target is the
/// anonymous `struct{ Value string }` at `:31-33`, so the read takes one column
/// and nothing else.
///
/// No read takes a user and no read carries an ownership clause: the repository
/// defines no `userFilter`, and `Get` builds its statement with a plain
/// `Select(...).From(table).Where(Eq{"id": id})` (`:30`) rather than `newSelect`,
/// so no library or user restriction is ever added.
#[derive(Debug)]
pub struct PropertyRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> PropertyRepo<'a> {
    /// `NewPropertyRepository` (`property_repository.go:9-13`): bind to `conn`
    /// (the pooled lease).
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `Get` (`property_repository.go:29-36`): the `value` column for one id.
    ///
    /// # Errors
    /// [`PropertyError::NotFound`] when no row carries the id; see that variant
    /// for Go's empty-string-beside-error shape. [`PropertyError::Sqlite`] on a
    /// statement or scan failure.
    pub fn get(&self, id: &str) -> Result<String, PropertyError> {
        let statement = get_sql(id);
        // Go's queryOne (`:32`) → dbx `Rows.one` (`dbx@v1.12.0/rows.go:237-244`),
        // whose no-rows condition `queryOne` maps to model.ErrNotFound.
        crate::execution::query_optional(self.conn, &statement, |row| {
            crate::hydrate::RowGet::get_text(row, "value")
        })?
        .ok_or(PropertyError::NotFound)
    }

    /// `DefaultGet` (`property_repository.go:38-45`): `Get` with Go's
    /// `ErrNotFound → defaultValue` fold.
    ///
    /// All three of Go's arms are ported: NotFound yields the default with no
    /// error; any OTHER error yields the default BESIDE that error (`:42-44`),
    /// which a `Result` cannot carry — the port returns `Err` alone. Every Go
    /// caller in the tree (`core/artwork/housekeeping.go:73`,
    /// `server/subsonic/browsing.go:36`, `cmd/artwork.go:208`, `cmd/root.go:213`)
    /// checks the error before using the value, so the observable behavior is the
    /// same. Executed (oracle `/tmp/sd3g1_oracle`): `DefaultGet("missing",
    /// "fallback")` returned `"fallback"` with `err=<nil>`, and `DefaultGet("k",
    /// "fallback")` on an existing key returned the stored `"v1"`.
    ///
    /// # Errors
    /// [`PropertyError::Sqlite`] when the statement or scan fails. Never
    /// [`PropertyError::NotFound`], which this method folds away.
    pub fn default_get(&self, id: &str, default_value: &str) -> Result<String, PropertyError> {
        match self.get(id) {
            Ok(value) => Ok(value),
            Err(PropertyError::NotFound) => Ok(default_value.to_string()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_sql_selects_value_bound_by_id() {
        // Arrange: Get's Select("value")/Eq{"id"} (property_repository.go:30-33).
        let id = "last_scan.md5";
        // Act:
        let (sql, args) = get_sql(id);
        // Assert:
        assert_eq!(sql, "SELECT value FROM property WHERE id = ?");
        assert_eq!(args, vec![Value::Text(id.to_string())]);
    }

    #[test]
    fn put_sql_updates_first_then_inserts_with_go_bind_order() {
        // Arrange: Put's update-then-insert pair (:15-24); the UPDATE binds
        // value first (Set before Where), the INSERT binds id first.
        let id = "last_scan.md5";
        let value = "abc123";
        // Act:
        let put = put_sql(id, value);
        // Assert:
        assert_eq!(
            put.update,
            (
                "UPDATE property SET value = ? WHERE id = ?".to_string(),
                vec![Value::Text(value.to_string()), Value::Text(id.to_string()),],
            )
        );
        assert_eq!(
            put.insert,
            (
                "INSERT INTO property (id, value) VALUES (?, ?)".to_string(),
                vec![Value::Text(id.to_string()), Value::Text(value.to_string()),],
            )
        );
    }

    #[test]
    fn delete_sql_deletes_bound_by_id() {
        // Arrange: Delete via r.delete(Eq{"id": id}) (:47-49).
        let id = "last_scan.md5";
        // Act:
        let (sql, args) = delete_sql(id);
        // Assert:
        assert_eq!(sql, "DELETE FROM property WHERE id = ?");
        assert_eq!(args, vec![Value::Text(id.to_string())]);
    }
}
