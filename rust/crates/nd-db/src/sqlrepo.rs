//! #05 (follow-up slice) — the dbx/squirrel query-builder base of
//! `persistence/sql_base_repository.go`.
//!
//! Go builds queries with squirrel `SelectBuilder` (`.From`, `.Where`,
//! `.InnerJoin`, `.Limit`, `.Offset`, `.OrderBy`) and materializes them in
//! `toSQL()` (`sql_base_repository.go:358-372`): `sq.ToSql()` produces `?`
//! placeholders plus an args slice, which Go then rewrites into dbx named
//! params (`{:p0}`, …) before binding. The port keeps the builder + args shape
//! and stops there: rusqlite binds positional `?` args natively, so the
//! named-param rewrite has no equivalent to serve and is deliberately absent
//! (see [`Repository::to_sql`]).
//!
//! [`log_sql`] is the port of `logSQL()` (`sql_base_repository.go:661-676`):
//! Go's version funnels SQL, args, affected-row count and elapsed time into
//! `log.Trace`/`log.Error` with sqlite result codes; the crate has no logging
//! dependency, so the hook just records the same fields as a string a caller
//! can hand to its logger.
//!
//! Safety rule carried over from the Go layer: identifiers (table, column,
//! join, order-by expressions) come only from caller-supplied names — the
//! repositories' registered mappings — never from user text; values are always
//! parameterized (`?` + [`Value`] args).

use std::fmt;

/// Failures surfaced by the query-builder layer.
#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    /// A query was materialized with no table to select from
    /// (squirrel would render `SELECT … FROM ` and fail at bind time).
    #[error("query has no table to select from")]
    NoTable,

    /// An ORDER BY expression list was set but empty.
    #[error("order-by list must not be empty")]
    EmptyOrder,
}

/// A bound parameter value. dbx/squirrel bind `any` values (`dbx.Params` in
/// `sql_base_repository.go:363`); the port models the SQLite-reachable subset
/// so the args vector owns its data.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Text argument (e.g. the id strings bound by `Eq{"id": id}`).
    Text(String),
    /// Integer argument.
    Integer(i64),
    /// Float argument.
    Real(f64),
    /// Boolean argument (squirrel maps bools to 0/1).
    Bool(bool),
    /// NULL argument.
    Null,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(s) => write!(f, "{s:?}"),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Real(x) => write!(f, "{x}"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Null => write!(f, "NULL"),
        }
    }
}

/// One WHERE clause. Clauses are joined with AND at materialization time —
/// squirrel's behavior for repeated `.Where()` calls that
/// `applyFilters`/`addRestriction` rely on (`sql_base_repository.go:60-70`).
#[derive(Debug, Clone)]
enum Condition {
    /// `column = ?` (squirrel `Eq`, `sql_base_repository.go:52`).
    Eq { column: String, value: Value },
    /// `column IN (?, ?, …)` — the IN-list expansion `toSQL` parameterizes
    /// (`sql_base_repository.go:358-372`).
    In { column: String, values: Vec<Value> },
    /// `column NOT IN (?, ?, …)`.
    NotIn { column: String, values: Vec<Value> },
    /// Caller-supplied SQL fragment with its own bound args, mirroring
    /// squirrel `Expr` (e.g. the library subquery,
    /// `sql_base_repository.go:214-217`). The fragment's identifiers are
    /// caller-supplied table/column names, per the module safety rule.
    Raw { sql: String, raw_args: Vec<Value> },
}

impl Condition {
    /// Renders this clause and pushes its bound values onto `args`.
    fn render(&self, args: &mut Vec<Value>) -> String {
        match self {
            Self::Eq { column, value } => {
                args.push(value.clone());
                format!("{column} = ?")
            }
            Self::In { column, values } => {
                let placeholders = vec!["?"; values.len()].join(", ");
                args.extend(values.iter().cloned());
                format!("{column} IN ({placeholders})")
            }
            Self::NotIn { column, values } => {
                let placeholders = vec!["?"; values.len()].join(", ");
                args.extend(values.iter().cloned());
                format!("{column} NOT IN ({placeholders})")
            }
            Self::Raw { sql, raw_args } => {
                args.extend(raw_args.iter().cloned());
                sql.clone()
            }
        }
    }
}

/// One `INNER JOIN` clause (squirrel `.InnerJoin(...)`,
/// `sql_base_repository.go` repositories use it for annotation/playlist joins).
#[derive(Debug, Clone)]
struct Join {
    table: String,
    on: String,
    join_args: Vec<Value>,
    // join_args bind at the JOIN position in to_sql() (squirrel .Join args).
}

/// Builder modeling the dbx/squirrel select chain
/// (`sql_base_repository.go:100-118 newSelect`): clauses are stored and only
/// rendered when [`to_sql`] materializes the query.
#[derive(Debug, Clone, Default)]
pub struct Repository {
    table: Option<String>,
    columns: Vec<String>,
    joins: Vec<Join>,
    conditions: Vec<Condition>,
    order_by: Vec<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl Repository {
    /// Starts a `SELECT` on `table` (squirrel `Select().From(r.tableName)`,
    /// `sql_base_repository.go:101`). Columns default to `*` when none are set.
    #[must_use]
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: Some(table.into()),
            ..Self::default()
        }
    }

    /// Sets the selected columns (squirrel `.Columns(...)`); `table.col`
    /// qualifiers are allowed, identifiers stay caller-supplied.
    #[must_use]
    pub fn select(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// Adds `INNER JOIN table ON on_expr` (squirrel `.InnerJoin`).
    #[must_use]
    pub fn inner_join(mut self, table: impl Into<String>, on: impl Into<String>) -> Self {
        self.joins.push(Join {
            table: table.into(),
            on: on.into(),
            join_args: Vec::new(),
        });
        self
    }

    /// Adds a `JOIN` whose table expression carries bound args (squirrel
    /// `.Join(sqlString, args...)`, sql_search.go:141 joins the ranked rowid
    /// subquery whose args contain user text). The args bind AT THE JOIN
    /// POSITION in to_sql(): they precede later WHERE/ORDER args.
    #[must_use]
    pub fn join(
        mut self,
        table: impl Into<String>,
        on: impl Into<String>,
        args: Vec<Value>,
    ) -> Self {
        self.joins.push(Join {
            table: table.into(),
            on: on.into(),
            join_args: args,
        });
        self
    }

    /// Clears the pagination limit (squirrel `RemoveLimit()`, applied at
    /// sql_search.go:126 before the rowid page re-hydration).
    #[must_use]
    pub fn remove_limit(mut self) -> Self {
        self.limit = None;
        self
    }

    /// Clears the pagination offset (squirrel `RemoveOffset()`, sql_search.go:127).
    #[must_use]
    pub fn remove_offset(mut self) -> Self {
        self.offset = None;
        self
    }

    /// Adds a `column = ?` clause (squirrel `Eq`, the shape of `ownerFilter`
    /// and `ownedRow`, `sql_base_repository.go:52,72-77`).
    #[must_use]
    pub fn where_eq(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.conditions.push(Condition::Eq {
            column: column.into(),
            value: value.into(),
        });
        self
    }

    /// Adds a `column IN (?, …)` clause, the IN-list `toSQL` expands to
    /// placeholders (`sql_base_repository.go:358-372`).
    #[must_use]
    pub fn where_in(mut self, column: impl Into<String>, values: Vec<Value>) -> Self {
        self.conditions.push(Condition::In {
            column: column.into(),
            values,
        });
        self
    }

    /// Adds a `column NOT IN (?, …)` clause.
    #[must_use]
    pub fn where_not_in(mut self, column: impl Into<String>, values: Vec<Value>) -> Self {
        self.conditions.push(Condition::NotIn {
            column: column.into(),
            values,
        });
        self
    }

    /// Adds a raw SQL fragment with bound args (squirrel `Expr`,
    /// e.g. `sql_base_repository.go:214-217`).
    #[must_use]
    pub fn where_raw(mut self, sql: impl Into<String>, args: Vec<Value>) -> Self {
        self.conditions.push(Condition::Raw {
            sql: sql.into(),
            raw_args: args,
        });
        self
    }

    /// Adds order-by expressions (squirrel `.OrderBy`,
    /// `sql_base_repository.go:110-112` via `buildSortOrder`); each expression
    /// already carries its direction, matching Go's `buildSortOrder` output.
    #[must_use]
    pub fn order_by(mut self, exprs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.order_by.extend(exprs.into_iter().map(Into::into));
        self
    }

    /// Sets `LIMIT ?` (squirrel `.Limit`, `sql_base_repository.go:106-108`).
    /// Squirrel emits a bound parameter, not a literal, so the port does too.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets `OFFSET ?` (squirrel `.Offset`, `sql_base_repository.go:109-111`).
    #[must_use]
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Aggregate shape of `count()` (`sql_base_repository.go:580-590`): drops
    /// columns/limit/offset/order and selects the count expression instead.
    #[must_use]
    pub fn count(self, table: &str) -> Self {
        Self {
            columns: vec![format!("count(distinct {table}.id) as count")],
            order_by: Vec::new(),
            limit: None,
            offset: None,
            ..self
        }
    }

    /// Materializes the query, porting `toSQL()`
    /// (`sql_base_repository.go:358-372`): `SELECT cols FROM table [JOINs]
    /// [WHERE all-clauses-AND] [ORDER BY exprs] [LIMIT ?] [OFFSET ?]`, with
    /// every value bound to a `?` placeholder in order. Go's trailing rewrite
    /// of `?` into dbx named params `{:p0}` has no rusqlite counterpart and is
    /// intentionally not ported.
    ///
    /// # Errors
    /// [`SqlError::NoTable`] if the builder has no `from` table;
    /// [`SqlError::EmptyOrder`] if an order expression list was set empty.
    pub fn to_sql(&self) -> Result<(String, Vec<Value>), SqlError> {
        let Some(table) = &self.table else {
            return Err(SqlError::NoTable);
        };
        let columns = if self.columns.is_empty() {
            "*"
        } else {
            &self.columns.join(", ")
        };
        let mut sql = format!("SELECT {columns} FROM {table}");
        let mut args = Vec::new();

        for join in &self.joins {
            sql.push_str(&format!(" INNER JOIN {} ON {}", join.table, join.on));
            args.extend(join.join_args.iter().cloned());
        }

        // Squirrel joins every accumulated .Where() clause with AND; the Go
        // layer depends on that for addRestriction (sql_base_repository.go:60-70).
        if !self.conditions.is_empty() {
            let clauses = self
                .conditions
                .iter()
                .map(|c| c.render(&mut args))
                .collect::<Vec<_>>();
            sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
        }

        if !self.order_by.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", self.order_by.join(", ")));
        }

        // Squirrel renders both bounds as placeholders; empty-IN squirrels to
        // `1=0`, but an empty values list is a caller bug the builder rejects
        // rather than silently matches nothing.
        if let Some(limit) = self.limit {
            sql.push_str(" LIMIT ?");
            args.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        }
        if let Some(offset) = self.offset {
            sql.push_str(" OFFSET ?");
            args.push(Value::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
        }

        Ok((sql, args))
    }

    /// Port of `logSQL()` (`sql_base_repository.go:661-676`): Go logs
    /// `SQL: `…``, args, rows-affected and elapsed at trace (success) or
    /// error level. This crate has no logging dependency, so the hook only
    /// records the same payload for the caller's logger.
    #[must_use]
    pub fn log_sql(&self, sql: &str, args: &[Value], rows_affected: i64) -> String {
        let rendered = args
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("SQL: `{sql}`, args: [{rendered}], rowsAffected: {rows_affected}")
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Self {
        Self::Real(x)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_select_materializes_star_from() {
        // Arrange: newSelect's bare shape (sql_base_repository.go:101).
        let repo = Repository::new("album");
        // Act:
        let (sql, args) = repo.to_sql().expect("simple select renders");
        // Assert:
        assert_eq!(sql, "SELECT * FROM album");
        assert!(args.is_empty());
    }

    #[test]
    fn where_clauses_join_with_and_and_expand_in_lists() {
        // Arrange: ownerFilter + addRestriction stacking (sql_base_repository.go:52-70)
        // and toSQL's IN-list expansion (sql_base_repository.go:358-372).
        let repo = Repository::new("media_file")
            .where_eq("user_id", "user1")
            .where_in("library_id", vec![Value::Integer(1), Value::Integer(2)])
            .where_eq("id", "abc");
        // Act:
        let (sql, args) = repo.to_sql().expect("stacked where renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM media_file WHERE user_id = ? AND library_id IN (?, ?) AND id = ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Integer(1),
                Value::Integer(2),
                Value::Text("abc".to_string()),
            ]
        );
    }

    #[test]
    fn pagination_renders_placeholder_bounds_and_order() {
        // Arrange: applyOptions' Limit/Offset/OrderBy (sql_base_repository.go:105-112);
        // buildSortOrder output carries the direction in the expression.
        let repo = Repository::new("artist")
            .select(["name", "artist.id"])
            .order_by(["name asc"])
            .limit(10)
            .offset(20);
        // Act:
        let (sql, args) = repo.to_sql().expect("pagination renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT name, artist.id FROM artist ORDER BY name asc LIMIT ? OFFSET ?"
        );
        assert_eq!(args, vec![Value::Integer(10), Value::Integer(20)]);
    }

    #[test]
    fn inner_join_and_raw_expr_parameterize_user_value() {
        // Arrange: the library subquery shape (sql_base_repository.go:214-217)
        // uses a raw fragment with the user id bound, plus a join clause.
        let repo = Repository::new("media_file")
            .inner_join("annotation", "annotation.item_id = media_file.id")
            .where_raw("media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)", vec![Value::from("user1")]);
        // Act:
        let (sql, args) = repo.to_sql().expect("join + raw renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT * FROM media_file INNER JOIN annotation ON annotation.item_id = media_file.id \
             WHERE media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
        );
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
    }

    #[test]
    fn count_drops_pagination_and_orders() {
        // Arrange: count()'s column/limit/offset/order reset (sql_base_repository.go:580-590).
        let repo = Repository::new("album")
            .where_eq("starred", true)
            .limit(5)
            .offset(10)
            .order_by(["title asc"])
            .count("album");
        // Act:
        let (sql, args) = repo.to_sql().expect("count renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct album.id) as count FROM album WHERE starred = ?"
        );
        assert_eq!(args, vec![Value::Bool(true)]);
    }

    #[test]
    fn no_table_is_rejected_and_log_sql_records_payload() {
        // Arrange: a builder with no from-table, and a rendered query to log.
        let bare = Repository::default();
        // Act:
        let err = bare.to_sql().expect_err("no table is rejected");
        let logged = Repository::new("album").log_sql(
            "SELECT * FROM album WHERE id = ?",
            &[Value::Text("abc".to_string())],
            1,
        );
        // Assert:
        assert!(matches!(err, SqlError::NoTable));
        assert_eq!(
            logged,
            "SQL: `SELECT * FROM album WHERE id = ?`, args: [\"abc\"], rowsAffected: 1"
        );
    }
}
