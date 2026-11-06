//! Port of `persistence/sql_search_like.go` (107L) — the LIKE search strategy.
//!
//! [`LikeSearch`] is Go's `likeSearch` struct (`sql_search_like.go:6-17`): a
//! squirrel `Sqlizer` filter plus the `ToSql()` it renders. Go nests `And{}`
//! of `Or{}` of one-entry `Like{}` maps (`:35-58` legacy path, `:74-97`
//! core-columns path) and `execute` prepends that filter to the caller's
//! select chain (`:19-22`). This port flattens the nesting into
//! [`crate::sqlrepo::Repository`] through [`Repository::where_raw`]: one raw
//! fragment per word group, one bound [`Value::Text`] per LIKE pattern — so
//! user words never reach SQL text and typed `%`, `_`, `'` stay a PARAMETER.
//!
//! Two behaviours Go leaves implicit are named here: squirrel `Like` renders
//! `column LIKE ?`, and SQLite's `LIKE` is case-insensitive for ASCII, so the
//! port emits `LIKE` unchanged (no `LOWER()`); and Go's shorthand quotes
//! neither `%` nor `_` (`:61-63,93-95`), safe only because its legacy input
//! goes through `str.SanitizeStrings`, whereas here the pattern is bound
//! verbatim — [`escape_like`] is that counterpart, backslash-escaping SQLite's
//! wildcards (`\`, `%`, `_`) before wrapping (SQLite's default LIKE escape is
//! `\`, so no `ESCAPE` clause).
//!
//! `str.SanitizeStrings` is reused from `nd-utils`; `r.queryAll` and the
//! two-phase rowid machinery belong to other slices, so builders here stop at
//! the `(sql, args)` pair `toSQL()` hands the bind layer. Legacy keeps Go's
//! single-space `strings.Split` (space runs make empty words); the core-column
//! path keeps `strings.Fields`. Go's `nil` strategies become `None`. Choosing
//! the LIKE branch is the FTS lane's job — [`like_strategy`] only names it
//! through [`crate::sql_search_fts::SearchStrategy`].

use crate::sql_search::Sql;
use crate::sql_search_fts::SearchStrategy;
use crate::sqlrepo::{Repository, SqlError, Value};
use nd_utils::str::sanitize_strings;

/// `likeSearchColumns` (`sql_search_like.go:74-78`) — core columns per table,
/// pinned to the Go literal declaration order so generated SQL is stable.
const LIKE_SEARCH_COLUMNS: [(&str, &[&str]); 3] = [
    ("media_file", &["title", "album", "artist", "album_artist"]),
    ("album", &["name", "album_artist"]),
    ("artist", &["name"]),
];
/// No registered columns — Go's `likeSearchColumns` lookup miss.
const NO_COLUMNS: [&str; 0] = [];
/// SQL keyword joining words of a query (squirrel `And{}`).
const AND: &str = " AND ";
/// SQL keyword joining the columns of one word; squirrel renders `Or{}`
/// parenthesized, so it cannot bind wrongly once AND-joined.
const OR: &str = " OR ";
/// Characters SQLite treats as LIKE wildcards.
const LIKE_WILDCARDS: [char; 3] = ['\\', '%', '_'];

/// Escapes the SQLite LIKE wildcards (`\`, `%`, `_`) in user text so a typed
/// percent or underscore matches itself instead of widening the pattern. SQLite's
/// default LIKE escape character is `\`, so the pattern needs no `ESCAPE` clause.
fn escape_like(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        if LIKE_WILDCARDS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The registered columns of `tableName`, or `None` when Go's
/// `likeSearchColumns[tableName]` lookup misses (`sql_search_like.go:79-82`).
pub fn like_search_columns(table: &str) -> Option<&[&str]> {
    for (name, columns) in LIKE_SEARCH_COLUMNS {
        if name == table {
            return Some(columns);
        }
    }
    None
}

/// Renders the nested squirrel filter both Go expr functions build
/// (`sql_search_like.go:59-63,90-96`) as one raw fragment plus its bound args,
/// ordered like the `?` placeholders. `column` is the single legacy column
/// (`<table>.full_text`); `columns` the core-column list, whose per-word group
/// is OR-joined. `sep` is the inter-word gap of the legacy path
/// (`sql_search_like.go:54-56`).
fn build_filter(table: &str, column: &str, columns: &[&str], sep: &str, words: Vec<String>) -> Sql {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    for word in words {
        let escaped = escape_like(&word);
        let pattern = format!("%{sep}{escaped}%");
        if columns.is_empty() {
            clauses.push(format!("{table}.{column} LIKE ?"));
            args.push(Value::Text(pattern.clone()));
        } else {
            let ors = columns
                .iter()
                .map(|col| format!("{table}.{col} LIKE ?"))
                .collect::<Vec<String>>()
                .join(OR);
            clauses.push(format!("({ors})"));
            for _ in columns {
                args.push(Value::Text(pattern.clone()));
            }
        }
    }
    (clauses.join(AND), args)
}

/// `legacySearchExpr` (`sql_search_like.go:35-58`): LIKE patterns against
/// `<table>.full_text`, one per space-split word of the sanitized query. `None`
/// when sanitization yields nothing (`:42-45`).
fn legacy_filter(table: &str, query: &str, full_string: bool) -> Option<Sql> {
    const FULL_TEXT: &str = "full_text";
    let sanitized = sanitize_strings(&[query]);
    if sanitized.is_empty() {
        return None;
    }
    let sep = if full_string { "" } else { " " };
    let words = sanitized.split(" ").map(String::from).collect();
    Some(build_filter(table, FULL_TEXT, &NO_COLUMNS, sep, words))
}

/// `likeSearchExpr` (`sql_search_like.go:85-97`): LIKE patterns against the
/// registered core columns — each word must match at least one column (OR
/// within the word), every word must match (AND between words). No minimum
/// length: single CJK characters are meaningful words. `None` for an empty
/// query (`:86-89`) or unregistered table (`:79-82`).
fn like_filter(table: &str, query: &str) -> Option<Sql> {
    let words: Vec<String> = query.split_whitespace().map(String::from).collect();
    let columns = like_search_columns(table).unwrap_or(&NO_COLUMNS);
    if words.is_empty() || columns.is_empty() {
        return None;
    }
    Some(build_filter(table, "", columns, "", words))
}

/// `likeSearch` (`sql_search_like.go:6-17`) as data: the table `execute`
/// prepends the filter to, plus the filter's materialized `ToSql()` pair.
#[derive(Debug, Clone)]
pub struct LikeSearch {
    /// Repository table the strategy searches (`sqlRepository.tableName`).
    pub table_name: String,
    /// The squirrel filter, already materialized as `(sql, args)`.
    pub filter: Sql,
}

impl LikeSearch {
    /// Port of `likeSearch.ToSql()` (`sql_search_like.go:13-17`).
    #[must_use]
    pub fn to_sql(&self) -> Sql {
        self.filter.clone()
    }
}

/// `newLegacySearch` (`sql_search_like.go:30-32`): the full-text LIKE strategy
/// for `table`, or `None` when no searchable token survives sanitization.
/// `full_string` is `conf.Server.Search.FullString` (`:54-56`).
#[must_use]
pub fn new_legacy_search(table: &str, query: &str, full_string: bool) -> Option<LikeSearch> {
    let filter = legacy_filter(table, query, full_string)?;
    Some(LikeSearch {
        table_name: table.to_string(),
        filter,
    })
}

/// `newLikeSearch` (`sql_search_like.go:40-42`): the core-columns LIKE strategy
/// for `table`, or `None` when the query or the table yields nothing to match.
pub fn new_like_search(table: &str, query: &str) -> Option<LikeSearch> {
    let filter = like_filter(table, query)?;
    Some(LikeSearch {
        table_name: table.to_string(),
        filter,
    })
}

/// `likeSearch.execute` (`sql_search_like.go:16-24`): the caller's select chain
/// with the strategy's filter AND-joined and `sq.OrderBy(cfg.OrderBy...)`.
/// Order-by expressions are caller-supplied column names, never user text.
pub fn execute_sql(search: LikeSearch, order_by: Vec<String>) -> Result<Sql, SqlError> {
    Repository::new(search.table_name.clone())
        .where_raw(search.filter.0.clone(), search.filter.1.clone())
        .order_by(order_by.iter().cloned())
        .to_sql()
}

/// The LIKE branch of Go's `getSearchStrategy` (`sql_search.go:42-44` legacy
/// backend / full-string, `:46-48` CJK fallback) reported through the FTS
/// lane's [`SearchStrategy`] seam: the query text the LIKE filter searches for
/// — sanitized on the full-string path (`sql_search_like.go:36`), trimmed on
/// the fallback path (`:86`).
#[must_use]
pub fn like_strategy(query: &str, full_string: bool) -> SearchStrategy {
    if full_string {
        SearchStrategy::Like(sanitize_strings(&[query]))
    } else {
        SearchStrategy::Like(query.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_words_wrap_around_the_inter_word_separator() {
        // Arrange: Go's default Search.FullString=false.
        let Some(search) = new_legacy_search("media_file", "abbey road", false) else {
            panic!("expected a legacy strategy");
        };
        // Act:
        let (sql, args) = search.to_sql();
        // Assert: squirrel's And{} of two Like{} on the full_text column, each
        // pattern carrying the leading separator space.
        assert_eq!(
            sql, "media_file.full_text LIKE ? AND media_file.full_text LIKE ?",
            "sql"
        );
        assert_eq!(
            args,
            [
                Value::Text("% abbey%".to_string()),
                Value::Text("% road%".to_string())
            ],
            "args",
        );
    }

    #[test]
    fn full_string_search_drops_the_separator() {
        // Arrange: conf.Server.Search.FullString=true.
        let Some(search) = new_legacy_search("media_file", "radio", true) else {
            panic!("expected a legacy strategy");
        };
        // Act:
        let (sql, args) = search.to_sql();
        // Assert:
        assert_eq!(sql, "media_file.full_text LIKE ?", "sql");
        assert_eq!(args, [Value::Text("%radio%".to_string())], "args");
    }

    #[test]
    fn core_columns_or_join_and_user_wildcards_stay_literal() {
        // Arrange: a CJK fallback query over the album table whose second word
        // carries a user-typed percent, apostrophe and underscore.
        let Some(search) = new_like_search("album", "周杰伦 50%'_x") else {
            panic!("expected a core-columns strategy");
        };
        // Act:
        let (sql, args) = search.to_sql();
        // Assert: two AND-joined groups of OR-joined core columns; typed
        // wildcards escaped inside the bound values, the apostrophe kept as a
        // parameter value, never SQL text.
        assert_eq!(sql, "(album.name LIKE ? OR album.album_artist LIKE ?) AND (album.name LIKE ? OR album.album_artist LIKE ?)", "sql");
        assert_eq!(
            args,
            [
                Value::Text("%周杰伦%".to_string()),
                Value::Text("%周杰伦%".to_string()),
                Value::Text("%50\\%'\\_x%".to_string()),
                Value::Text("%50\\%'\\_x%".to_string()),
            ],
            "args",
        );
    }

    #[test]
    fn unsearchable_queries_have_no_strategy() {
        // Arrange: Go's nil returns — whitespace-only like query, unregistered
        // table, sanitizer-empty legacy query (bare quotes are dropped).
        // Act:
        let like_only_spaces = new_like_search("media_file", "   ");
        let unknown_table = new_like_search("unknown_table", "周杰伦");
        let quotes_only = new_legacy_search("album", "'''", false);
        // Assert:
        assert!(like_only_spaces.is_none(), "empty like query");
        assert!(unknown_table.is_none(), "unknown table");
        assert!(quotes_only.is_none(), "sanitizer-empty query");
    }
}
