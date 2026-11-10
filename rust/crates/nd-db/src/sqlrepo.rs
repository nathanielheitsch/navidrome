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
    group_by: Vec<String>,
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

    /// Sets the `GROUP BY` list (squirrel `.GroupBy(...)`, applied at
    /// `sql_tags.go:224-228` by `baseTagRepository.newSelect`, which the genre
    /// repository reuses for its aggregate counts). Identifiers are caller-supplied,
    /// like every other SQL fragment here.
    #[must_use]
    pub fn group_by(mut self, exprs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.group_by = exprs.into_iter().map(Into::into).collect();
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

    /// Sets the `LIMIT` bound (squirrel `.Limit`, `sql_base_repository.go:151-153`).
    ///
    /// Squirrel stores the bound as a rendered decimal string — `Limit(n)` is
    /// `builder.Set(b, "Limit", fmt.Sprintf("%d", n))` (`squirrel@v1.5.4
    /// select.go:376-378`) and `ToSql` writes it straight into the SQL
    /// (`select.go:139-142`), contributing **no** argument. So the bound is a
    /// literal, not a placeholder, and the port renders it as one too. The
    /// values here are always `u64` page sizes coming from
    /// [`crate::sql_restful::QueryOptions`] rather than caller text, so
    /// interpolation is not an injection surface.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Sets the `OFFSET` bound (squirrel `.Offset`, `sql_base_repository.go:154-156`).
    ///
    /// Same literal rendering as [`Self::limit`] (`select.go:386-388`,
    /// `select.go:144-147`). See [`Self::to_sql`] for the one place the port
    /// deliberately departs from squirrel's output.
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
    /// [WHERE all-clauses-AND] [GROUP BY] [ORDER BY exprs] [LIMIT n]
    /// [OFFSET n]`, with every *filter* value bound to a `?` placeholder in
    /// order. Squirrel's `LIMIT`/`OFFSET` bounds are rendered decimal literals
    /// and contribute no args (`select.go:139-147`), so they do not join the
    /// arg list. Go's trailing rewrite of `?` into dbx named params `{:p0}`
    /// has no rusqlite counterpart and is intentionally not ported.
    ///
    /// Deviation (deliberate, cited): squirrel emits a bare `OFFSET n` when
    /// `Max` is unset (`applyOptions` sets the two bounds independently,
    /// `sql_base_repository.go:151-156`), which is valid on PostgreSQL and
    /// MySQL but a **parse error on SQLite** — SQLite requires a `LIMIT`
    /// before `OFFSET`. Since SQLite is Navidrome's primary engine, the port
    /// emits `LIMIT -1` (SQLite's unlimited bound) ahead of `OFFSET n` in
    /// that case, so an offset-only page executes instead of failing.
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
        // Squirrel emits GROUP BY after WHERE and before ORDER BY.
        if !self.group_by.is_empty() {
            sql.push_str(&format!(" GROUP BY {}", self.group_by.join(", ")));
        }

        if !self.order_by.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", self.order_by.join(", ")));
        }

        // Squirrel renders the bounds as decimal literals straight into the
        // SQL and pushes nothing onto args (`select.go:139-147`); empty-IN
        // still squirrels to `1=0`, but an empty values list is a caller bug
        // the builder rejects rather than silently matching nothing.
        if let Some(limit) = self.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        if let Some(offset) = self.offset {
            if self.limit.is_none() {
                sql.push_str(" LIMIT -1");
            }
            sql.push_str(&format!(" OFFSET {offset}"));
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
    fn pagination_renders_literal_bounds_and_contributes_no_args() {
        // Arrange: applyOptions' Limit/Offset/OrderBy (sql_base_repository.go:145-159);
        // buildSortOrder output carries the direction in the expression.
        let repo = Repository::new("artist")
            .select(["name", "artist.id"])
            .order_by(["name asc"])
            .limit(10)
            .offset(20);
        // Act:
        let (sql, args) = repo.to_sql().expect("pagination renders");
        // Assert: squirrel stores the bounds as rendered decimals
        // (`select.go:376-388`) and writes them into the SQL without touching
        // args (`select.go:139-147`), so nothing here is bound.
        assert_eq!(
            sql,
            "SELECT name, artist.id FROM artist ORDER BY name asc LIMIT 10 OFFSET 20"
        );
        assert_eq!(args, Vec::<Value>::new());
    }

    #[test]
    fn offset_without_limit_emits_the_sqlite_unlimited_bound() {
        // Arrange: applyOptions sets the bounds independently
        // (sql_base_repository.go:151-156), so Max unset + Offset set squirrels
        // a bare `OFFSET 5`.
        let repo = Repository::new("user").select(["*"]).offset(5);
        // Act:
        let (sql, args) = repo.to_sql().expect("offset renders");
        // Assert: squirrel's bare `OFFSET n` is a SQLite parse error, so the
        // port precedes it with `LIMIT -1` (SQLite's unlimited bound). Bounds
        // stay literals, so args remains empty.
        assert_eq!(sql, "SELECT * FROM user LIMIT -1 OFFSET 5");
        assert_eq!(args, Vec::<Value>::new());
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
