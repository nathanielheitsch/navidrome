//! Port of `persistence/property_repository.go` (62L) — the `property`
//! key/value table (`persistence.NewPropertyRepository`,
//! `persistence/property_repository.go:9-13`).
//!
//! Go's repository is a bare `sqlRepository` over `property (id, value)`;
//! every method builds one squirrel statement and hands it to
//! `executeSQL`/`queryOne`/`delete`, which materialize through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`. As in the sibling
//! `sql_*` ports, execution stays with the caller (`store.rs` seam), so this
//! port stops at the same [`Sql`] pairs and pins their shapes.
//!
//! Values are plain TEXT (`value string` on both the read and the write,
//! `:10,30,33`); Go stores JSON only when a caller hands it a JSON string —
//! the table itself has no JSON handling, so none is ported.
//!
//! `DefaultGet` (`:38-45`) is Go's `Get` plus an `ErrNotFound → defaultValue`
//! fold at execution time; the SQL shape it wraps is exactly [`get_sql`], so
//! the default fold ports with the execution slice, not here.

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
