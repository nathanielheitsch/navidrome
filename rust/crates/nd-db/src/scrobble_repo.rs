//! Port of `persistence/scrobble_repository.go` (90L) — the submitted-scrobbles
//! table behind the "recently played" endpoints.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`count`/`queryOne`/`queryAll`, which materialize through
//! `toSQL()` (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and
//! logging stay with `store.rs`/the caller, so the port stops at the same
//! `(sql, args)` pairs (see [`Sql`]).
//!
//! Ported here: `baseQuery`'s user-scoped three-column select (`:18-27`),
//! `RecordScrobble`'s SetMap insert (`:40-49`), `CountAll`/`Count` via the base
//! `count()` (`:51-52`, `:54-56`, `sql_base_repository.go:566-576`), `Get`'s
//! id-filtered select (`:58-61`), and the registered `from`/`to` timestamp
//! filters (`:13-18`, `:35-37` — `fromTs` is `>=`, `toTs` is `<=`).
//!
//! Not ported, with the reason: `setSortMappings({"submission_time"})` and
//! `parseRestOptions` over a live request context belong to the auth-aware
//! slice (`sql_restful.rs` owns the registry); the REST paths take the
//! caller's already-parsed `QueryOptions`. Go's `submission_time` filter keys
//! land here as explicit [`from_ts`]/[`to_ts`] fragments so their operator
//! difference (`>=` vs `<=`) stays pinned.

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
            let sq = crate::sql_search::apply_filters(sq, options);
            crate::sql_search::apply_pagination(sq, options)
        }
    }
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
            "SELECT id, media_file_id, submission_time FROM scrobbles WHERE scrobbles.user_id = ? LIMIT ? OFFSET ?"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Integer(10),
                Value::Integer(5),
            ]
        );
    }
}
