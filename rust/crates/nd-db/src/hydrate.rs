//! S-D3 shared hydration core: the row → entity-record seam for the eight Go
//! `PostScan` hooks (`docs/rust-port/hydrate-scan-contract.md`, S-D3 brief).
//!
//! ## Two-phase shape (Go's `Scan` → `PostScan`)
//!
//! Go's dbx first scans the raw row columns into the `db*` struct and then
//! runs the entity's `PostScan` hook to decode the JSON shim columns
//! (`persistence/rows.go` machinery, `dbUser.PostScan`
//! `persistence/user_repository.go:36-43`). The port mirrors that split
//! exactly, following the landed `UserRow{record, libraries_json}` →
//! `UserRow::into_record` precedent (`user_repo.rs`):
//!
//! 1. a **raw row** fn — [`row_to_user`] — reads the plain columns dbx scans
//!    and hands back a raw struct ([`crate::user_repo::UserRow`]) with the
//!    JSON shim column still unparsed;
//! 2. a **decode** fn — [`user_post_scan`] — applies the entity's hook over
//!    the raw payload and produces the hydrated record.
//!
//! Later sub-slices (S-D3b/c/d/e: folder, playlist, album, mediafile,
//! artist, playlist_track, scrobble_buffer) add their own raw-row + decode
//! pairs here on top of the shared [`RowGet`] accessor and the landed
//! `sql_json`/`sql_tags`/`sql_participations` codecs.
//!
//! ## NULL parity (Go `database/sql`)
//!
//! NULL into a **non-pointer** numeric/string/time field **errors**; NULL
//! into a pointer yields nil. [`RowGet`] enforces the same: the bare
//! `get_*` getters error on NULL, the `get_opt_*` getters yield `None`.
//!
//! ## Absent-column tolerance (the two corrections)
//!
//! Base reads JOIN `annotation` (and the mediafile base read also JOINs
//! `bookmark`), but the *light* queries (e.g. scrobble-buffer's `m.*` on a
//! missing media file) do not emit those columns, and dbx discards
//! unmatched columns into a throwaway `NullString`
//! (`persistence/rows.go:84`). A ported hydrator must therefore treat
//! optional (joined) columns as **absent** on light queries — reading them
//! with `get_opt_*` yields `None`-free access **only when the column is
//! present**; when it is *absent* the accessor must not panic. Use
//! [`RowGet::has_column`] (probing with the accessor's own
//! missing-column error) to branch: absent optional columns decode to
//! their zero/None value.
//! Required (base-table) columns may use the bare getters unconditionally.
//!
//! ## Errors
//!
//! [`HydrateError::Decoded`] carries Go's EXACT error prefix verbatim
//! (e.g. `"parsing user libraries from db: "`, brief §4c). Callers must
//! depend only on the prefix: the suffix is the decode failure, and its
//! wording comes from `serde_json`/`encoding-json`-style rendering, which
//! is allowed to differ from Go's `encoding/json` byte-for-byte.
//! No new prefixes are invented; hooks whose Go code has no wrapper use
//! the codec's own message.

use chrono::{DateTime, Utc};
use rusqlite::types::Type;
use rusqlite::{Error as SqliteError, Result as SqliteResult, Row};

use crate::time;
use crate::user_repo::{UserLibrary, UserRecord, UserRow};

/// Go's `dbUser.PostScan` wrapper, verbatim
/// (`persistence/user_repository.go:38`).
pub const USER_LIBRARIES_PREFIX: &str = "parsing user libraries from db: ";

/// Failures of the shared hydration seam.
#[derive(Debug, thiserror::Error)]
pub enum HydrateError {
    /// A JSON shim column decoded with an error, carrying Go's EXACT
    /// wrapper prefix verbatim (e.g. `"parsing user libraries from db: …"`,
    /// `USER_LIBRARIES_PREFIX`).
    ///
    /// Depend only on the prefix: the suffix is the underlying decode
    /// failure and its exact wording is `serde_json`'s, not Go's
    /// `encoding/json`'s.
    #[error("{0}")]
    Decoded(String),

    /// The row read itself failed (statement, conversion, NULL into a
    /// required column, missing required column).
    #[error(transparent)]
    Sqlite(#[from] SqliteError),
}

/// The boxed error payload for a decode failure surfaced as a sqlite
/// conversion failure (see [`HydrateError::into_sqlite`]).
#[derive(Debug)]
struct DecodedPayload(String);

impl std::fmt::Display for DecodedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodedPayload {}

impl HydrateError {
    /// The seam's failure as a `rusqlite` conversion failure, the shape Go's
    /// `ScanStruct` error takes out of the read (`database/sql` surfaces a
    /// column-conversion failure from `Rows.Scan`): a
    /// `FromSqlConversionFailure` whose payload IS the Go-prefixed message.
    #[must_use]
    pub fn into_sqlite(self) -> SqliteError {
        match self {
            Self::Sqlite(e) => e,
            Self::Decoded(msg) => {
                SqliteError::FromSqlConversionFailure(0, Type::Text, Box::new(DecodedPayload(msg)))
            }
        }
    }
}

/// The shared column accessor over a raw row (brief §4a).
///
/// NULL semantics follow Go (`database/sql/convert.go`): the bare getters
/// **error** on NULL; the `opt_*` getters yield `None`.
///
/// Absent-column rule: joined/optional columns may be missing on light
/// queries (brief §2 corrections). Callers probe with [`Self::has_column`]
/// (or the accessor's own missing-column error) and fall back to the
/// zero/None value — a missing *optional* column is data, not a bug.
/// Missing *required* (base-table) columns surface as `InvalidColumnName`
/// errors.
pub trait RowGet {
    /// A TEXT column Go scans into a non-pointer `string`: **NULL errors**
    /// (Go parity), as does an absent column.
    fn get_text(&self, column: &str) -> SqliteResult<String>;

    /// A coalesce'd INTEGER `bool` column (Go's `bool` non-pointer):
    /// **NULL errors**.
    fn get_bool(&self, column: &str) -> SqliteResult<bool>;

    /// A non-pointer `int` column: **NULL errors**.
    fn get_i32(&self, column: &str) -> SqliteResult<i32>;

    /// A non-pointer `int64` column: **NULL errors**.
    fn get_i64(&self, column: &str) -> SqliteResult<i64>;

    /// A non-pointer `real` column: **NULL errors**.
    fn get_f64(&self, column: &str) -> SqliteResult<f64>;

    /// A `*string` column: NULL → `None`.
    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>>;

    /// A `*float64` column (the `rg_*` shims, `average_rating`):
    /// NULL → `None`.
    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>>;

    /// A `*time.Time` column: NULL → `None`, the DDL's
    /// [`crate::time::ZERO_DATE_SENTINEL`] → `None` (routed through
    /// [`crate::time::optional_sentinel`]), a genuinely malformed value
    /// → error.
    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>>;

    /// `true` when the row's SELECT emits `column`. The
    /// absent-optional-column rule: joined columns (`annotation`,
    /// `bookmark`, …) may be absent on light queries. Probing uses the
    /// accessor's own missing-column error, so no statement handle is
    /// needed.
    fn has_column(&self, column: &str) -> bool;
}

impl RowGet for Row<'_> {
    fn get_text(&self, column: &str) -> SqliteResult<String> {
        let v: Option<String> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            // Go: NULL into a non-pointer string errors.
            None => Err(null_required(column, Type::Text)),
        }
    }

    fn get_bool(&self, column: &str) -> SqliteResult<bool> {
        let v: Option<bool> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_i32(&self, column: &str) -> SqliteResult<i32> {
        let v: Option<i32> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_i64(&self, column: &str) -> SqliteResult<i64> {
        let v: Option<i64> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Integer)),
        }
    }

    fn get_f64(&self, column: &str) -> SqliteResult<f64> {
        let v: Option<f64> = self.get(column)?;
        match v {
            Some(v) => Ok(v),
            None => Err(null_required(column, Type::Real)),
        }
    }

    fn get_opt_text(&self, column: &str) -> SqliteResult<Option<String>> {
        self.get(column)
    }

    fn get_opt_f64(&self, column: &str) -> SqliteResult<Option<f64>> {
        self.get(column)
    }

    fn get_opt_dt(&self, column: &str) -> SqliteResult<Option<DateTime<Utc>>> {
        // NULL → None, the '0000-00-00 00:00:00' sentinel → None, genuine
        // malformation → error (time.rs:118).
        time::optional_sentinel(self, column)
    }

    fn has_column(&self, column: &str) -> bool {
        // Row has no `column_index`; the accessor's own missing-column
        // error is the probe. `get_ref` needs no conversion, so a present
        // column never errors here.
        self.get_ref(column).is_ok()
    }
}

/// The boxed error payload for NULL into a required column.
#[derive(Debug)]
struct NullRequired(String);

impl std::fmt::Display for NullRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NULL value in required column `{}` (Go parity: NULL into non-pointer errors)",
            self.0
        )
    }
}

impl std::error::Error for NullRequired {}

/// The Go-parity NULL failure for a non-pointer column.
fn null_required(column: &str, ty: Type) -> SqliteError {
    SqliteError::FromSqlConversionFailure(0, ty, Box::new(NullRequired(column.to_string())))
}

/// `dbUser` row mapping (phase 1 of the two-phase seam): the user columns
/// dbx scans (`persistence/user_repository.go:30-34`) plus the raw
/// `libraries_json` SELECT-alias, still unparsed.
///
/// `libraries_json` is a non-pointer `string` in Go's `dbUser`
/// (`persistence/user_repository.go:33`), so it is read with
/// [`RowGet::get_text`]: the select's `COALESCE(…, '[]')` makes it non-NULL,
/// and a NULL (only possible if the alias were dropped) is a scan error in
/// Go too (NULL into a non-pointer string, `database/sql/convert.go`).
///
/// # Errors
/// A missing or NULL *required* user column fails, per Go parity.
pub fn row_to_user(row: &Row<'_>) -> SqliteResult<UserRow> {
    let libraries_json = row.get_text("libraries_json")?;
    Ok(UserRow {
        record: UserRecord {
            id: row.get_text("id")?,
            user_name: row.get_text("user_name")?,
            name: row.get_text("name")?,
            email: row.get_text("email")?,
            is_admin: row.get_bool("is_admin")?,
            last_login_at: row.get_opt_text("last_login_at")?,
            last_access_at: row.get_opt_text("last_access_at")?,
            created_at: row.get_text("created_at")?,
            updated_at: row.get_text("updated_at")?,
            scrobble_filter: row.get_text("scrobble_filter")?,
            password: row.get_text("password")?,
            token_epoch: row.get_i64("token_epoch")?,
            libraries: Vec::new(),
        },
        libraries_json,
    })
}

/// `dbUser.PostScan` (phase 2 of the two-phase seam,
/// `persistence/user_repository.go:36-43`): decode the raw `libraries_json`
/// onto the record.
///
/// Empty payload (NULL coalesced away) yields an empty library set like
/// Go's guard; the select's `COALESCE(…, '[]')` means a no-libraries user
/// reads the literal `"[]"`, which parses to the same empty (non-nil) set.
/// A non-empty payload that fails to parse is [`HydrateError::Decoded`]
/// with Go's exact prefix.
///
/// # Errors
/// [`HydrateError::Decoded`] carrying `USER_LIBRARIES_PREFIX`.
pub fn user_post_scan(raw: UserRow) -> Result<UserRecord, HydrateError> {
    let mut record = raw.record;
    record.libraries = parse_libraries_json(&raw.libraries_json)
        .map_err(|err| HydrateError::Decoded(format!("{USER_LIBRARIES_PREFIX}{err}")))?;
    Ok(record)
}

/// `PostScan`'s decode (`user_repository.go:37-41`), shared by
/// [`user_post_scan`]. Empty input is the skip-and-return-nil branch;
/// otherwise the JSON array of the select's `libraries_json`.
///
/// # Errors
/// The `serde_json` failure Go's `json.Unmarshal` would return.
pub fn parse_libraries_json(json: &str) -> Result<Vec<UserLibrary>, serde_json::Error> {
    if json.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// One row with every user column present: the happy path through
    /// both phases.
    fn full_user_row(conn: &Connection) -> UserRow {
        const SQL: &str = "SELECT 'u1' AS id, 'jdoe' AS user_name, 'John' AS name, \
             'j@x.y' AS email, 1 AS is_admin, \
             '2024-01-02 03:04:05+00:00' AS last_login_at, \
             NULL AS last_access_at, \
             '2024-01-01 00:00:00+00:00' AS created_at, \
             '2024-02-01 00:00:00+00:00' AS updated_at, \
             '' AS scrobble_filter, 'CIPHER' AS password, \
             0 AS token_epoch, \
             '[{\"id\":7,\"name\":\"Rock\",\"path\":\"/rock\"}]' AS libraries_json";
        conn.query_row(SQL, [], row_to_user)
            .expect("full row reads")
    }

    /// The 12 user columns without `libraries_json` (the light-query shape).
    const USER_COLS_NO_JSON: &str = "SELECT 'u1' AS id, 'jdoe' AS user_name, 'John' AS name, \
         'j@x.y' AS email, 0 AS is_admin, NULL AS last_login_at, \
         NULL AS last_access_at, '2024-01-01 00:00:00+00:00' AS created_at, \
         '2024-01-01 00:00:00+00:00' AS updated_at, '' AS scrobble_filter, \
         'CIPHER' AS password, 0 AS token_epoch";

    #[test]
    fn two_phase_split_hydrates_libraries() {
        // Arrange:
        let conn = Connection::open_in_memory().expect("memory db opens");
        // Act:
        let raw = full_user_row(&conn);
        let record = user_post_scan(raw).expect("payload parses");
        // Assert:
        assert_eq!(record.id, "u1");
        assert_eq!(record.libraries.len(), 1);
        assert_eq!(record.libraries[0].id, 7);
        assert_eq!(record.libraries[0].name, "Rock");
        assert_eq!(record.libraries[0].path, "/rock");
        // Assert: NULL pointer time is None, non-NULL required text read.
        assert_eq!(
            record.last_login_at.as_deref(),
            Some("2024-01-02 03:04:05+00:00")
        );
        assert_eq!(record.last_access_at, None);
    }

    #[test]
    fn empty_and_null_libraries_decode_to_an_empty_set() {
        // "" is Go's guard-skip branch; the select coalesces no-library
        // users to "[]", which parses to the same empty (non-nil) vec.
        assert!(parse_libraries_json("").unwrap().is_empty());
        let parsed = parse_libraries_json("[]").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_libraries_json_yields_decoded_with_gos_prefix() {
        // Arrange: a row whose libraries_json will not parse.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let raw = conn
            .query_row(
                &format!("{USER_COLS_NO_JSON}, '{{\"not\": an array}}' AS libraries_json"),
                [],
                row_to_user,
            )
            .expect("the raw row itself is valid");
        // Act:
        let err = user_post_scan(raw).unwrap_err();
        // Assert: Decoded, with Go's exact prefix up front.
        let HydrateError::Decoded(msg) = err else {
            panic!("expected Decoded, got {err:?}");
        };
        assert!(msg.starts_with(USER_LIBRARIES_PREFIX), "{msg}");
    }

    #[test]
    fn required_getters_error_on_null_and_optional_yield_none() {
        // Arrange: one row where every column is NULL.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let err = conn
            .query_row("SELECT NULL AS id, NULL AS t", [], |row| row.get_text("id"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("NULL value in required column `id`"),
            "{err}"
        );
        conn.query_row(
            "SELECT NULL AS f, NULL AS b, NULL AS n, NULL AS d",
            [],
            |row| {
                assert!(row.get_f64("f").is_err());
                assert!(row.get_bool("b").is_err());
                assert!(row.get_i64("n").is_err());
                assert!(row.get_opt_f64("f").unwrap().is_none());
                assert_eq!(row.get_opt_text("f").unwrap(), None);
                Ok(())
            },
        )
        .expect("opt getters tolerate NULL");
    }

    #[test]
    fn opt_dt_maps_null_and_sentinel_to_none_and_errors_on_garbage() {
        let conn = Connection::open_in_memory().expect("memory db opens");
        let expected = time::parse("2024-01-02 03:04:05+00:00").unwrap();
        conn.query_row(
            "SELECT NULL AS a, '0000-00-00 00:00:00' AS b, \
             '2024-01-02 03:04:05+00:00' AS c, 'not a date' AS d",
            [],
            |row| {
                assert_eq!(row.get_opt_dt("a")?, None, "NULL is absent");
                assert_eq!(row.get_opt_dt("b")?, None, "the sentinel is absent");
                assert_eq!(row.get_opt_dt("c")?, Some(expected));
                assert!(row.get_opt_dt("d").is_err(), "garbage must error");
                Ok(())
            },
        )
        .expect("opt_dt read");
    }

    #[test]
    fn a_select_without_libraries_json_errors_not_panics() {
        // The absent-column rule: a light SELECT lacking an optional
        // column must surface a rusqlite error, never a panic.
        let conn = Connection::open_in_memory().expect("memory db opens");
        let err = conn
            .query_row(USER_COLS_NO_JSON, [], row_to_user)
            .unwrap_err();
        assert!(matches!(err, SqliteError::InvalidColumnName(_)), "{err:?}");
        // has_column reports the absence for the tolerant opt-by-name path.
        conn.query_row("SELECT 1 AS id", [], |row| {
            assert!(!row.has_column("libraries_json"));
            assert!(row.has_column("id"));
            Ok(())
        })
        .expect("probe read");
    }
}
