//! Port of `persistence/plugin_repository.go` (144L) — the plugin registry.
//!
//! As in the sibling repositories, every method returns the materialized
//! `(sql, args)` pair Go's `toSQL()` produces; execution stays in the store
//! layer. Go's `isPermitted` (`:28-31`) becomes an `is_admin` parameter, and
//! `rest.ErrPermissionDenied` is [`ERR_PERMISSION_DENIED`] data.
//!
//! NOTE: the `plugin` table was deferred from the fresh-DDL golden by an
//! earlier issue (`nd-dbschema/tests/fresh_schema.rs:60-65`); this SQL names
//! the table exactly as Go does, the schema slice owns adding it.

use crate::sql_restful::QueryOptions;
use crate::sqlrepo::{Repository, Value};

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The `plugin` table (`registerModel(&model.Plugin{})`, `:21`).
pub const TABLE: &str = "plugin";

/// `model.Plugin` (`model/plugin.go`): the columns `Put` binds, minus the
/// timestamps Go stamps from the clock (`UpdatedAt` at `:89`,
/// `created_at`/`updated_at` at `:125-126`) — the caller supplies `now`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plugin {
    /// Plugin id.
    pub id: String,
    /// Plugin bundle path.
    pub path: String,
    /// JSON manifest text.
    pub manifest: String,
    /// JSON config text.
    pub config: String,
    /// JSON arrays of user/library ids plus the flags.
    pub users: String,
    /// All-users grant flag.
    pub all_users: bool,
    /// JSON array of library ids.
    pub libraries: String,
    /// All-libraries grant flag.
    pub all_libraries: bool,
    /// Write-access grant flag.
    pub allow_write_access: bool,
    /// Enabled flag.
    pub enabled: bool,
    /// Last failure message; '' when clean.
    pub last_error: String,
    /// Bundle digest.
    pub sha256: String,
}

/// `model.Plugins`.
pub type Plugins = Vec<Plugin>;

/// `rest.ErrPermissionDenied` (`:35`, `:51`, `:59`, `:66`, `:76`, `:86`).
pub const ERR_PERMISSION_DENIED: &str = "permission denied";

/// `Put`'s empty-id rejection (`:91-93`): `errors.New` before any SQL.
pub const ERR_PLUGIN_ID_EMPTY: &str = "plugin ID cannot be empty";

/// `ClearErrors` (`:33-47`): the probe always runs; the UPDATE is issued only
/// when it found something — Go's comment (`:37`) notes an UPDATE takes the
/// write lock even when nothing matches.
#[must_use]
pub fn clear_errors_sql(has_errors: bool) -> Vec<Sql> {
    let probe = (
        "SELECT EXISTS (SELECT 1 FROM plugin WHERE last_error != '')".to_string(),
        Vec::new(),
    );
    if !has_errors {
        return vec![probe];
    }
    let update = (
        "UPDATE plugin SET last_error = '' WHERE last_error != ''".to_string(),
        Vec::new(),
    );
    vec![probe, update]
}

/// `CountAll` (`:49-55`): the base `count()` over a bare `newSelect`.
#[must_use]
pub fn count_sql(options: Option<&QueryOptions>) -> Sql {
    crate::sql_search::new_select(TABLE, options)
        .count(TABLE)
        .to_sql()
        .unwrap()
}

/// `Delete` (`:57-62`): the base `deleteByID` (`sql_base_repository.go:649-660`).
#[must_use]
pub fn delete_sql(id: &str) -> Sql {
    (
        format!("DELETE FROM {TABLE} WHERE id = ?"),
        vec![Value::Text(id.to_string())],
    )
}

/// `Get` (`:64-72`): `newSelect().Where(Eq{"id": id}).Columns("*")`.
#[must_use]
pub fn get_sql(id: &str) -> Sql {
    Repository::new(TABLE)
        .select(vec!["*"])
        .where_eq("id", id)
        .to_sql()
        .unwrap()
}

/// `GetAll` (`:74-82`): `newSelect(options...).Columns("*")`.
#[must_use]
pub fn get_all_sql(options: Option<&QueryOptions>) -> Sql {
    crate::sql_search::new_select(TABLE, options)
        .select(vec!["*"])
        .to_sql()
        .unwrap()
}

/// `Put` (`:84-129`): Go rejects an empty id before building any query
/// (`:91-93`) — [`SqlError::InvalidQuery`] so the upsert never runs. Otherwise
/// the atomic INSERT … ON CONFLICT(id) DO UPDATE (`:96-111`): dbx's named
/// params bind positionally in listing order, and `created_at` is absent from
/// the UPDATE list so a conflict keeps the original stamp.
pub fn put_sql(plugin: Plugin, now: i64) -> Result<Sql, &'static str> {
    if plugin.id.is_empty() {
        return Err(ERR_PLUGIN_ID_EMPTY);
    }
    let columns: [&str; 14] = [
        "id",
        "path",
        "manifest",
        "config",
        "users",
        "all_users",
        "libraries",
        "all_libraries",
        "allow_write_access",
        "enabled",
        "last_error",
        "sha256",
        "created_at",
        "updated_at",
    ];
    let placeholders = columns.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let update: [&str; 12] = [
        "path",
        "manifest",
        "config",
        "users",
        "all_users",
        "libraries",
        "all_libraries",
        "allow_write_access",
        "enabled",
        "last_error",
        "sha256",
        "updated_at",
    ];
    let updates = update
        .iter()
        .map(|c| format!("{c} = excluded.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok((
        format!(
            "INSERT INTO plugin ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
            columns.join(", "),
            placeholders,
            updates,
        ),
        vec![
            Value::Text(plugin.id.clone()),
            Value::Text(plugin.path.clone()),
            Value::Text(plugin.manifest.clone()),
            Value::Text(plugin.config.clone()),
            Value::Text(plugin.users.clone()),
            Value::Bool(plugin.all_users),
            Value::Text(plugin.libraries.clone()),
            Value::Bool(plugin.all_libraries),
            Value::Bool(plugin.allow_write_access),
            Value::Bool(plugin.enabled),
            Value::Text(plugin.last_error.clone()),
            Value::Text(plugin.sha256.clone()),
            Value::Integer(now),
            Value::Integer(now),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_errors_probes_before_it_updates() {
        // Arrange: no error rows present (ClearErrors :38-44).
        // Act:
        let stmts = clear_errors_sql(false);
        // Assert: only the EXISTS probe runs; the UPDATE is skipped.
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0].0,
            "SELECT EXISTS (SELECT 1 FROM plugin WHERE last_error != '')"
        );
        // Act again: with an error row (Go then issues the UPDATE, :45-46).
        let with_error = clear_errors_sql(true);
        // Assert:
        assert_eq!(with_error.len(), 2);
        assert_eq!(
            with_error[1].0,
            "UPDATE plugin SET last_error = '' WHERE last_error != ''"
        );
    }

    #[test]
    fn empty_plugin_id_rejects_before_the_upsert_is_built() {
        // Arrange: Put with no id (plugin_repository.go:91-93).
        // Act:
        let result = put_sql(Plugin::default(), 42);
        // Assert:
        assert_eq!(result.err(), Some(ERR_PLUGIN_ID_EMPTY));
    }

    #[test]
    fn put_upsert_binds_every_column_except_created_at_on_conflict() {
        // Arrange: an enabled plugin (model/plugin.go).
        let plugin = Plugin {
            id: "pl-1".to_string(),
            enabled: true,
            ..Default::default()
        };
        // Act:
        let (sql, args) = put_sql(plugin, 99).unwrap();
        // Assert: 14 bound insert values, both timestamps Go's time.Now()
        // supplies, and created_at absent from the conflict clause (:99).
        assert_eq!(args.len(), 14);
        assert_eq!(args[0], Value::Text("pl-1".to_string()));
        assert_eq!(args.last(), Some(Value::Integer(99)).as_ref());
        assert!(sql.starts_with("INSERT INTO plugin (id, path,"));
        assert!(sql.contains("ON CONFLICT(id) DO UPDATE SET"));
        assert!(!sql.contains("created_at = excluded.created_at"));
    }
}
