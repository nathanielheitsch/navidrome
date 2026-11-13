//! Synchronous execution of repository statements, including typed row mapping.
//!
//! SQL identifiers/fragments must come from trusted repository code. Values are
//! always bound. Use these functions on a leased connection or transaction;
//! async callers must enter through [`crate::store::SQLStore::with_connection`].

use rusqlite::types::{ToSql, ToSqlOutput};
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};

use crate::sqlrepo::Value;

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match self {
            Self::Text(value) => value.to_sql(),
            Self::Integer(value) => value.to_sql(),
            Self::Real(value) => value.to_sql(),
            Self::Bool(value) => value.to_sql(),
            Self::Null => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Null)),
        }
    }
}

/// Execute one statement and return the affected-row count.
///
/// # Errors
/// Returns SQLite prepare, binding, or execution failures.
pub fn execute(conn: &Connection, statement: &(String, Vec<Value>)) -> rusqlite::Result<usize> {
    conn.execute(&statement.0, params_from_iter(&statement.1))
}

/// Read at most one row; absence is `None`, not a database error.
///
/// # Errors
/// Returns SQLite prepare, binding, execution, or row-mapping failures.
pub fn query_optional<T>(
    conn: &Connection,
    statement: &(String, Vec<Value>),
    map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Option<T>> {
    conn.query_row(&statement.0, params_from_iter(&statement.1), map)
        .optional()
}

/// Read owned typed rows, dropping the prepared statement before returning.
///
/// # Errors
/// Returns SQLite prepare, binding, execution, or row-mapping failures.
pub fn query_all<T>(
    conn: &Connection,
    statement: &(String, Vec<Value>),
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut prepared = conn.prepare(&statement.0)?;
    let rows = prepared.query_map(params_from_iter(&statement.1), map)?;
    rows.collect()
}
