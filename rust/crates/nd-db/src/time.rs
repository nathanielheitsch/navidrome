//! The crate-wide timestamp storage convention: SQLite `datetime`/`timestamp`
//! columns hold TEXT written in rusqlite's own `chrono` spelling.
//!
//! Why one helper exists (the bug it prevents): binding an epoch-second
//! `i64` into a `datetime` column stores it with `typeof() = integer`, because
//! SQLite's affinity rules let INTEGER win over the declared `datetime` type
//! (`sqlite3.org/datatypes.html` §3). INTEGER sorts BEFORE TEXT, so both
//! consequences are silent:
//!
//! * a TEXT-bound range predicate — Go's `Gt{"media_file.created_at": since}`
//!   (`persistence/mediafile_repository.go:533,559`) — skips integer rows;
//! * `ORDER BY created_at DESC` returns integer rows first regardless of
//!   chronology.
//!
//! The convention is rusqlite's `ToSql for DateTime<Utc>`
//! (`rusqlite-0.32.1/src/types/chrono.rs:88-93`): `%F %T%.f%:z`, i.e.
//! `2024-01-01 00:00:00+00:00` (and `2024-01-01 00:00:00.123+00:00` when
//! fractional seconds are non-zero — `%.f` prints them only then). Its
//! counterpart `FromSql` (`:117-136`) parses that shape, the `T`-separated
//! RFC3339 shape, and falls back to SQLite's naive `current_timestamp`
//! default, so reading a row written by Go or by the DDL default still works.
//!
//! Two rules the repositories follow:
//!
//! 1. Never mix separators in one database. `DateTime::to_rfc3339()` emits a
//!    `T`; this helper emits rusqlite's space, so `params![dt]` (rusqlite's
//!    own `ToSql`) and the explicit [`value`] construction that repos need for
//!    `Vec<(String, Value)>` SET-lists are byte-identical.
//! 2. Stamp timestamps explicitly instead of relying on the DDL's
//!    `CURRENT_TIMESTAMP` default, which produces the naive third shape
//!    (`2026-11-11 23:19:10`). Go stamps them too
//!    (`sql_base_repository.go`'s `UpdatedAt = time.Now()` legs), so this is
//!    not a behaviour change.
//!
//! `None`/NULL means "absent", matching Go's `*time.Time` nils
//! (`model/share.go:19,20`, `model/artist.go:39,45,46`); the zero-integer
//! sentinel this port used before is gone.
//!
//! Converted here: `share_repo`, `radio_repo` (including the `artwork_queue`
//! row its `Put` enqueues), `artist_repo`, and the `artwork_queue_repo`
//! stamps/predicates (`retry_at <= now`, `artwork_queue_repository.go:167`).
//! Remaining work, stated so nobody assumes it is done: `library_repo`,
//! `mediafile_repo`, `folder_repo`, `album_repo`, `playqueue_repo`,
//! `artwork_repo` and `plugin_repo` still bind epoch integers into
//! `datetime`/`timestamp` columns, so their range predicates and ordering are
//! still affected by the affinity rule above. `playlist_repo`,
//! `smart_playlist_repo`, `sql_annotations`, `sql_bookmarks` and `user_repo`
//! already bind TEXT through a caller-supplied `&str` stamp
//! (`user_repo.rs:227-230`); their fixtures now use this module's shape, and
//! their signatures are the natural next conversion. `user_repo`'s record
//! keeps the stored TEXT opaquely on purpose (`user_repo.rs:490-492`); only
//! the bytes it writes had to be unified.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::sqlrepo::Value;

/// rusqlite's `ToSql for DateTime<Utc>` format string, verbatim
/// (`rusqlite-0.32.1/src/types/chrono.rs:91`). Anything that stamps a
/// timestamp column goes through it.
pub const SQLITE_DATETIME_FORMAT: &str = "%F %T%.f%:z";

/// The caller-owned clock: Go's `time.Now()` (`sql_base_repository.go:141`,
/// `radio_repository.go:97`, `artist_repository.go:240`). Callers pass the
/// stamp into a statement, so tests stay deterministic; this is the only
/// place the crate reads the wall clock.
#[must_use]
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

/// The stored TEXT for `dt`, byte-identical to what `params![dt]` binds.
#[must_use]
pub fn to_text(dt: &DateTime<Utc>) -> String {
    dt.format(SQLITE_DATETIME_FORMAT).to_string()
}

/// A bound TEXT timestamp for the `Vec<Value>`/`Vec<Col>` SET-lists the
/// repositories build (`artist_repo::put_cols`, `radio_repo::put_cols`).
#[must_use]
pub fn value(dt: &DateTime<Utc>) -> Value {
    Value::Text(to_text(dt))
}

/// `Some(dt)` binds TEXT, `None` binds NULL — Go's `*time.Time` nil
/// (`model/share.go:19` `ExpiresAt`, whose zero means "never expires",
/// `core/share.go:44`).
#[must_use]
pub fn optional_value(dt: Option<&DateTime<Utc>>) -> Value {
    dt.map(value).unwrap_or(Value::Null)
}

/// A `SET column = <timestamp>` pair for repositories that model SET pairs
/// as `(String, Value)` (`artist_repo.rs:517`).
#[must_use]
pub fn col(column: &str, dt: &DateTime<Utc>) -> (String, Value) {
    (column.to_string(), value(dt))
}
/// Fresh DDL defaults several `datetime` columns to the sentinel
/// `'0000-00-00 00:00:00'` (`nd-dbschema`: `library.last_scan_at`/
/// `last_scan_started_at`, `album.imported_at`). chrono cannot parse it
/// (month and day are 0), so a strict read of a default row would fail.
///
/// Go reaches the same place and resolves it the same way: parsing that text
/// errors and yields the zero `time.Time`, and the scanner tests
/// `!lib.LastScanAt.IsZero()` (`scanner/controller.go:124`,
/// `model/library.go:64`), so the sentinel means "never scanned" rather than
/// a date. This port maps it to `None` for the same reason — `None` is the
/// port's "absent" spelling, mirroring Go's `*time.Time` nils above.
pub const ZERO_DATE_SENTINEL: &str = "0000-00-00 00:00:00";

/// Read a nullable timestamp column, treating the DDL's
/// [`ZERO_DATE_SENTINEL`] as absent (`None`) rather than a parse failure.
///
/// # Errors
/// Propagates a genuine decode failure — a malformed date is an error, but
/// the sentinel is not.
pub fn optional_sentinel(
    row: &rusqlite::Row<'_>,
    column: &str,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(column)?;
    match raw {
        None => Ok(None),
        Some(text) if text == ZERO_DATE_SENTINEL => Ok(None),
        Some(_) => row.get(column).map(Some),
    }
}
/// A `SET column = <timestamp>` pair where `None` sets the column to NULL
/// (`artist_repo.rs`'s `external_info_updated_at`, Go's nil `*time.Time`).
#[must_use]
pub fn optional_col(column: &str, dt: Option<&DateTime<Utc>>) -> (String, Value) {
    (column.to_string(), optional_value(dt))
}

/// Read a timestamp column back as `DateTime<Utc>`, accepting every shape a
/// database written before this convention can contain: rusqlite's
/// space-separated text, `T`-separated RFC3339 (the `Z` or an offset), and
/// SQLite's naive `current_timestamp` (read as UTC, exactly as rusqlite's
/// `FromSql` fallback does, `types/chrono.rs:132-135`).
///
/// A NULL column and an unparseable value both yield `None`; the caller
/// decides which of the two it can tolerate.
#[must_use]
pub fn parse(text: &str) -> Option<DateTime<Utc>> {
    // rusqlite picks the RFC3339 layout by the 11th byte, then falls back to
    // naive — same order, same formats.
    let fmt = if text.len() >= 11 && text.as_bytes()[10] == b'T' {
        "%FT%T%.f%#z"
    } else {
        "%F %T%.f%#z"
    };
    if let Ok(dt) = DateTime::parse_from_str(text, fmt) {
        return Some(dt.with_timezone(&Utc));
    }
    // Couldn't parse as rfc3339 — fall back to naive, as rusqlite does.
    NaiveDateTime::parse_from_str(text, "%F %T%.f")
        .ok()
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// `parse` for a nullable column (`Option<&str>`/`Option<String>` reads).
#[must_use]
pub fn parse_optional(text: Option<&str>) -> Option<DateTime<Utc>> {
    text.and_then(parse)
}

/// `true` when `expires_at` has passed — Go's
/// `!expiresAt.IsZero() && expiresAt.Before(time.Now())` (`core/share.go:44`),
/// where a `None` expiry ("never expires") never counts as expired.
#[must_use]
pub fn is_expired(expires_at: Option<&DateTime<Utc>>, now: &DateTime<Utc>) -> bool {
    expires_at.is_some_and(|at| at < now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use rusqlite::Connection;

    fn at(year: u32, month: u32, day: u32, h: u32, mi: u32, s: u32, nano: u32) -> DateTime<Utc> {
        let naive = NaiveDate::from_ymd_opt(i32::try_from(year).unwrap(), month, day)
            .unwrap()
            .and_hms_nano_opt(h, mi, s, nano)
            .unwrap();
        Utc.from_utc_datetime(&naive)
    }

    #[test]
    fn to_text_pins_rusqlite_space_separated_shape_with_offset() {
        // The whole point: this is what params![dt] stores, so the helper and
        // a direct bind cannot drift.
        assert_eq!(
            to_text(&at(2024, 1, 1, 0, 0, 0, 0)),
            "2024-01-01 00:00:00+00:00"
        );
        assert_eq!(
            to_text(&at(2026, 11, 11, 23, 19, 10, 0)),
            "2026-11-11 23:19:10+00:00"
        );
    }

    #[test]
    fn fractional_seconds_print_only_when_nonzero() {
        // `%.f` prints "" for a whole second and ".SSS" otherwise.
        assert_eq!(
            to_text(&at(2024, 1, 1, 0, 0, 0, 0)),
            "2024-01-01 00:00:00+00:00"
        );
        assert_eq!(
            to_text(&at(2024, 1, 1, 0, 0, 0, 123_000_000)),
            "2024-01-01 00:00:00.123+00:00"
        );
        assert_eq!(
            to_text(&at(2024, 1, 1, 0, 0, 0, 7_000_000)),
            "2024-01-01 00:00:00.007+00:00"
        );
    }

    #[test]
    fn helper_text_is_byte_identical_to_a_direct_bind() {
        // Arrange: one DB, two rows — one stamped through params![DateTime],
        // one through the helper's Value, both into the same datetime column.
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch("CREATE TABLE t (id text primary key, created_at datetime not null)")
            .expect("table creates");
        let dt = at(2024, 1, 1, 0, 0, 0, 0);
        conn.execute(
            "INSERT INTO t (id, created_at) VALUES ('bind', ?)",
            rusqlite::params![dt],
        )
        .expect("rusqlite bind writes");
        conn.execute(
            "INSERT INTO t (id, created_at) VALUES (?, ?)",
            rusqlite::params!["helper", value(&dt)],
        )
        .expect("helper value writes");
        // Act:
        let stored: Vec<(String, String, String)> = conn
            .prepare("SELECT id, created_at, typeof(created_at) FROM t ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .expect("rows read");
        // Assert: identical text, and TEXT affinity actually held.
        assert_eq!(stored.len(), 2);
        assert_eq!(
            stored[0].1, stored[1].1,
            "bind and helper must store identical text"
        );
        assert_eq!(stored[0].1, "2024-01-01 00:00:00+00:00");
        for row in &stored {
            assert_eq!(row.2, "text", "timestamp columns must be text");
        }
    }

    #[test]
    fn sentinel_reads_as_absent_and_a_real_date_reads_as_a_time() {
        // The fresh DDL defaults library.last_scan_at/last_scan_started_at and
        // album.imported_at to '0000-00-00 00:00:00', which chrono cannot parse
        // (month and day are 0). Go's equivalent parse errors and yields the
        // zero time.Time, and the scanner tests !LastScanAt.IsZero()
        // (model/library.go:64, scanner/controller.go:124), so the sentinel means
        // "never scanned" — this port spells that None.
        let conn = Connection::open_in_memory().expect("memory db opens");
        conn.execute_batch(
            "CREATE TABLE t (id text primary key, last_scan_at datetime not null default '0000-00-00 00:00:00');",
        )
        .expect("table creates");
        conn.execute(
            "INSERT INTO t (id, last_scan_at) VALUES ('sentinel', '0000-00-00 00:00:00')",
            [],
        )
        .expect("sentinel row writes");
        conn.execute(
            "INSERT INTO t (id, last_scan_at) VALUES ('real', ?)",
            rusqlite::params![value(&at(2024, 1, 1, 0, 0, 0, 0))],
        )
        .expect("real row writes");
        conn.execute("INSERT INTO t (id) VALUES ('default')", [])
            .expect("default row writes via the DDL default");

        let read = |id: &str| {
            conn.query_row("SELECT last_scan_at FROM t WHERE id = ?", [id], |row| {
                optional_sentinel(row, "last_scan_at")
            })
            .expect("row reads")
        };
        assert_eq!(read("sentinel"), None, "the sentinel is absent");
        assert_eq!(read("default"), None, "the DDL default is absent too");
        assert_eq!(read("real"), Some(at(2024, 1, 1, 0, 0, 0, 0)));

        // A genuinely malformed date is still an error, not silently absent.
        conn.execute(
            "INSERT INTO t (id, last_scan_at) VALUES ('garbage', 'not a date')",
            [],
        )
        .expect("garbage row writes");
        assert!(
            conn.query_row(
                "SELECT last_scan_at FROM t WHERE id = ?",
                ["garbage"],
                |row| { optional_sentinel(row, "last_scan_at") }
            )
            .is_err(),
            "a malformed date must not read as absent"
        );
    }

    #[test]
    fn parse_accepts_every_shape_a_pre_convention_db_can_hold() {
        let expected = at(2024, 1, 1, 0, 0, 0, 0);
        assert_eq!(parse("2024-01-01 00:00:00+00:00"), Some(expected));
        assert_eq!(parse("2024-01-01T00:00:00Z"), Some(expected));
        assert_eq!(parse("2024-01-01T02:00:00+02:00"), Some(expected));
        // SQLite's naive CURRENT_TIMESTAMP default, read as UTC (rusqlite's
        // FromSql fallback, types/chrono.rs:132-135).
        assert_eq!(parse("2024-01-01 00:00:00"), Some(expected));
        assert_eq!(
            parse("2024-01-01 00:00:00.123+00:00"),
            Some(at(2024, 1, 1, 0, 0, 0, 123_000_000))
        );
        assert_eq!(parse(""), None);
        assert_eq!(
            parse("1704067200"),
            None,
            "an epoch integer is not a timestamp"
        );
        assert_eq!(parse_optional(None), None);
    }

    #[test]
    fn optional_binding_maps_none_to_null() {
        assert_eq!(optional_value(None), Value::Null);
        assert_eq!(
            optional_value(Some(&at(2024, 1, 1, 0, 0, 0, 0))),
            Value::Text("2024-01-01 00:00:00+00:00".to_string())
        );
        assert_eq!(
            col("created_at", &at(2024, 1, 1, 0, 0, 0, 0)).0,
            "created_at"
        );
        assert_eq!(optional_col("expires_at", None).1, Value::Null);
    }

    #[test]
    fn expiry_matches_gos_is_zero_and_before_rule() {
        // core/share.go:44 — a nil expiry never expires, a past one does.
        let now = at(2024, 1, 2, 0, 0, 0, 0);
        assert!(!is_expired(None, &now));
        assert!(is_expired(Some(&at(2024, 1, 1, 0, 0, 0, 0)), &now));
        assert!(
            !is_expired(Some(&now), &now),
            "exactly now is not before now"
        );
        assert!(!is_expired(Some(&at(2024, 1, 3, 0, 0, 0, 0)), &now));
    }
}
