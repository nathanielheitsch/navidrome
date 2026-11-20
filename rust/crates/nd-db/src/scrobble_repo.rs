//! Port of `persistence/scrobble_repository.go` (90L) — the submitted-scrobbles
//! table behind the "recently played" endpoints.
//!
//! Executed user-scoped reads preserve integer Unix seconds and inclusive
//! bounds (`persistence/scrobble_repository.go:17-31,47-83`).
//! QueryOptions carries caller-parsed filters and column sorts. Camel/snake
//! fields, comma lists and relative field directions follow Go's sort builder.

use crate::hydrate::RowGet;
use crate::sql_restful::QueryOptions;
use crate::sqlrepo::{Repository, Value};

/// The `scrobbles` table (`:33`).
pub const TABLE: &str = "scrobbles";

/// `model.Scrobble` (`model/scrobble.go`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scrobble {
    /// Integer primary key (Go `int64`).
    pub id: i64,
    /// Go `MediaFileID`.
    pub media_file_id: String,
    /// Not selected by `baseQuery` (Go `json:"-"`); the insert still writes it.
    pub user_id: String,
    /// Unix seconds.
    pub submission_time: i64,
}

/// `model.Scrobbles`.
pub type Scrobbles = Vec<Scrobble>;

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `baseQuery`'s select list (`:24`): the three columns every scrobble read
/// fetches, in Go's declaration order.
pub const BASE_COLUMNS: [&str; 3] = ["id", "media_file_id", "submission_time"];

/// `fromTs` (`:13-15`): the registered `from` filter —
/// `scrobbles.submission_time >= ?`.
#[must_use]
pub fn from_ts(min_time: i64) -> Sql {
    (
        "scrobbles.submission_time >= ?".to_string(),
        vec![Value::Integer(min_time)],
    )
}

/// `toTs` (`:17-18`): the registered `to` filter —
/// `scrobbles.submission_time <= ?`.
#[must_use]
pub fn to_ts(max_time: i64) -> Sql {
    (
        "scrobbles.submission_time <= ?".to_string(),
        vec![Value::Integer(max_time)],
    )
}

/// `baseQuery` (`:18-27`): the user-scoped select every read composes. The
/// logged-in user's id is Eq-bound (`:25`); `options` carry the registered
/// filters (Go's `from`/`to` keys) and pagination, applied in Go's order.
#[must_use]
pub fn base_query(user_id: &str, options: Option<&QueryOptions>) -> Repository {
    let sq = Repository::new(TABLE)
        .select(BASE_COLUMNS)
        .where_eq("scrobbles.user_id", user_id);
    match options {
        None => sq,
        Some(options) => {
            let mut sq = crate::sql_search::apply_filters(sq, options);
            if !options.sort.is_empty() {
                sq = sq.order_by(scrobble_sort(&options.sort, &options.order));
            }
            crate::sql_search::apply_pagination(sq, options)
        }
    }
}

// Go sql_base_repository.go:164-227: snake-case fallback for unmapped keys,
// comma lists, trimmed order, and field-local direction relative to that order.
// Only the parent-approved fixed scrobble column allowlist is supported.
// Unsupported names/expressions become a fixed missing-column reference:
// typed SQLite failure, never interpolated SQL or a silently unsorted read.
fn scrobble_sort(sort: &str, order: &str) -> Vec<String> {
    let descending = order.trim().eq_ignore_ascii_case("desc");
    if sort.chars().all(|c| c == ',') {
        return vec!["scrobbles.__unsupported_sort".to_string()];
    }
    sort.split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let normalized = crate::sql_restful::to_snake_case(part.trim());
            // Go's direction splitter uses literal ASCII space, not whitespace.
            let fields: Vec<&str> = normalized
                .split(' ')
                .filter(|part| !part.is_empty())
                .collect();
            let column = match fields.first().copied() {
                Some("id") => "scrobbles.id",
                Some("media_file_id") => "scrobbles.media_file_id",
                Some("submission_time") => "scrobbles.submission_time",
                _ => return "scrobbles.__unsupported_sort".to_string(),
            };
            let reverse = match fields.get(1).copied() {
                None | Some("asc") => false,
                Some("desc") => true,
                _ => return "scrobbles.__unsupported_sort".to_string(),
            };
            if fields.len() > 2 {
                return "scrobbles.__unsupported_sort".to_string();
            }
            let direction = if descending ^ reverse { "desc" } else { "asc" };
            format!("{column} {direction}")
        })
        .collect()
}

/// `RecordScrobble` (`:40-49`): the SetMap insert — `submission_time` is Go's
/// `submissionTime.Unix()` (`:47`), passed as unix seconds. The table's integer
/// pk self-assigns; `id.NewRandom()` is not involved.
#[must_use]
pub fn record_scrobble_sql(media_file_id: &str, user_id: &str, submission_time: i64) -> Sql {
    (
        format!("INSERT INTO {TABLE} (media_file_id, user_id, submission_time) VALUES (?, ?, ?)"),
        vec![
            Value::from(media_file_id),
            Value::from(user_id),
            Value::Integer(submission_time),
        ],
    )
}

/// `CountAll` (`:51-52`) / `Count` (`:54-56`) via the base `count()`
/// (`sql_base_repository.go:566-576`): the base query's user scope and filters
/// kept, pagination dropped.
#[must_use]
pub fn count_all_sql(user_id: &str, options: Option<&QueryOptions>) -> Sql {
    base_query(user_id, options)
        .count(TABLE)
        .to_sql()
        .expect("the scrobbles count materializes")
}

/// `Get` (`:58-61`): one of the user's scrobbles by id — both the user scope
/// and the id bind, so one user can never read another's rows.
#[must_use]
pub fn get_sql(user_id: &str, id: i64) -> Sql {
    base_query(user_id, None)
        .where_eq("id", id)
        .to_sql()
        .expect("the scrobble get materializes")
}

/// `GetAll` (`:63-67`) / `ReadAll` (`:75-77`): the user's scrobbles, filtered
/// and paged.
#[must_use]
pub fn get_all_sql(user_id: &str, options: Option<&QueryOptions>) -> Sql {
    base_query(user_id, options)
        .to_sql()
        .expect("the scrobbles select materializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_query_scopes_to_the_user_and_selects_three_columns() {
        // Arrange/Act: baseQuery (:18-27) bare.
        let (sql, args) = base_query("user1", None)
            .to_sql()
            .expect("the base select materializes");
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, media_file_id, submission_time FROM scrobbles WHERE scrobbles.user_id = ?"
        );
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
    }

    #[test]
    fn record_scrobble_inserts_unix_seconds() {
        // Arrange/Act: RecordScrobble's SetMap (:42-48).
        let (sql, args) = record_scrobble_sql("mf1", "user1", 1_700_000);
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO scrobbles (media_file_id, user_id, submission_time) VALUES (?, ?, ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("mf1".to_string()),
                Value::Text("user1".to_string()),
                Value::Integer(1_700_000),
            ]
        );
    }

    #[test]
    fn count_keeps_user_scope_and_drops_pagination() {
        // Arrange/Act: CountAll (:51-52) over base count() (:566-576).
        let (sql, args) = count_all_sql("user1", None);
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct scrobbles.id) as count FROM scrobbles WHERE scrobbles.user_id = ?"
        );
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
    }

    #[test]
    fn get_binds_both_the_user_and_the_id() {
        // Arrange/Act: Get (:58-61).
        let (sql, args) = get_sql("user1", 42);
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, media_file_id, submission_time FROM scrobbles WHERE scrobbles.user_id = ? AND id = ?"
        );
        assert_eq!(
            args,
            vec![Value::Text("user1".to_string()), Value::Integer(42),]
        );
    }

    #[test]
    fn from_and_to_filters_pin_their_operators() {
        // Arrange/Act: fromTs (:13-15) and toTs (:17-18).
        let from = from_ts(100);
        let to = to_ts(200);
        // Assert:
        assert_eq!(from.0, "scrobbles.submission_time >= ?");
        assert_eq!(from.1, vec![Value::Integer(100)]);
        assert_eq!(to.0, "scrobbles.submission_time <= ?");
        assert_eq!(to.1, vec![Value::Integer(200)]);
    }

    #[test]
    fn get_all_applies_pagination() {
        // Arrange: GetAll (:63-67) with a page.
        let options = QueryOptions {
            max: 10,
            offset: 5,
            ..QueryOptions::default()
        };
        // Act:
        let (sql, args) = get_all_sql("user1", Some(&options));
        // Assert:
        assert_eq!(
            sql,
            "SELECT id, media_file_id, submission_time FROM scrobbles WHERE scrobbles.user_id = ? LIMIT 10 OFFSET 5"
        );
        // Assert: the bounds are squirrel literals (`select.go:139-147`).
        assert_eq!(args, vec![Value::Text("user1".to_string())]);
    }
}

/// Strict scrobble read failures.
#[derive(Debug, thiserror::Error)]
pub enum ScrobbleError {
    /// No row matched this user's id.
    #[error("record not found")]
    NotFound,
    /// Concrete driver or scan failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Executed scrobble reads.
pub struct ScrobbleRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> ScrobbleRepo<'a> {
    /// Bind a caller-owned connection.
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// Count with filters, without pagination (Go CountAll :59-61).
    /// # Errors
    /// Returns concrete SQLite failures.
    pub fn count_all(
        &self,
        user_id: &str,
        options: Option<&QueryOptions>,
    ) -> Result<i64, ScrobbleError> {
        Ok(
            crate::execution::query_optional(self.conn, &count_all_sql(user_id, options), |row| {
                row.get_i64("count")
            })?
            .unwrap_or(0),
        )
    }

    /// Get one user-scoped row.
    /// # Errors
    /// Returns NotFound or concrete SQLite failures.
    pub fn get(&self, user_id: &str, id: i64) -> Result<Scrobble, ScrobbleError> {
        crate::execution::query_optional(self.conn, &get_sql(user_id, id), scrobble_from_row)?
            .ok_or(ScrobbleError::NotFound)
    }

    /// List filtered, sorted and paginated scrobbles.
    /// # Errors
    /// Returns concrete SQLite failures.
    pub fn get_all(
        &self,
        user_id: &str,
        options: Option<&QueryOptions>,
    ) -> Result<Scrobbles, ScrobbleError> {
        Ok(crate::execution::query_all(
            self.conn,
            &get_all_sql(user_id, options),
            scrobble_from_row,
        )?)
    }
}

fn scrobble_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Scrobble> {
    Ok(Scrobble {
        id: row.get_i64("id")?,
        media_file_id: row.get_text("media_file_id")?,
        submission_time: row.get_i64("submission_time")?,
        user_id: String::new(),
    })
}
