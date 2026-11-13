//! S-D3 P2 parity — the pinned driver's timestamp grammar EXECUTED against real
//! SQLite, through the shared hydration seam.
//!
//! Go truth under test (`go-sqlite3@v1.14.52/sqlite3.go`):
//! * `SQLiteTimestampFormats` (`:331-343`), tried in order with
//!   `time.ParseInLocation(…, time.UTC)`, after `strings.TrimSuffix(s, "Z")`
//!   (`:2823`) — one final `Z`;
//! * a parse failure assigns `time.Time{}` and reports NO error (`:2832-2842`);
//! * an INTEGER cell in a declared `timestamp`/`datetime`/`date` column becomes a
//!   `time.Time` (`:2781-2797`): magnitude strictly beyond 1e12 is milliseconds,
//!   everything else seconds, the multiplication is wrapping int64, and the result
//!   is `.UTC()`;
//! * the switch is on the LOWER-CASED DECLARED type (`:2645`, `:345-348`), matched
//!   exactly — `datetime(3)` and an expression column are not timestamps.
//!
//! Every expectation below was checked against the pinned Go toolchain (1.27.4) by
//! scanning real values out of a real `go-sqlite3` DB and marshalling them with
//! `encoding/json`, including the offset-preserving wire form.

use chrono::{DateTime, Utc};
use nd_db::hydrate::{self, GoTime, RowGet};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

/// One isolated on-disk DB per invocation, freshly migrated and pooled (the
/// harness shape of `tests/runtime_hydrate.rs`).
fn store() -> (SQLStore, std::path::PathBuf) {
    let path =
        std::path::PathBuf::from(nd_utils::files::temp_file_name("nd-db-ts-grammar-", ".db"));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let pool = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(pool.checkout().unwrap().conn()).unwrap();
    (SQLStore::new(pool), path)
}

fn run<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("store lease or statement failed: {e}"))
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// A probe table exercising the driver's declared-type switch: `datetime`,
/// `TIMESTAMP`, `DATE`, a plain `INTEGER`, and the non-timestamp spelling
/// `datetime(3)`.
const PROBE_DDL: &str = "CREATE TABLE probe (
    key TEXT,
    dt datetime,
    ts TIMESTAMP,
    d DATE,
    plain INTEGER,
    txt TEXT,
    weird datetime(3)
)";

/// `annotation.user_id` has a FK to `user`, so the annotation cases seed a real
/// owner row first (the shape `tests/runtime_timestamp_convention.rs` uses).
fn insert_user(store: &SQLStore, id: &str) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO user(id, user_name, name, email, password, is_admin, created_at, updated_at) \
             VALUES(?,?,?,?,?,?,?,?)",
            rusqlite::params![id, id, id, format!("{id}@example.com"), "encrypted", 1, "2024-01-01 00:00:00+00:00", "2024-01-01 00:00:00+00:00"],
        )
        .unwrap_or_else(|e| panic!("user insert failed: {e}"));
    });
}

/// Read one probe column as Go's `time.Time` (zone included).
fn gotime(store: &SQLStore, key: &str, column: &str) -> SqliteOutcome {
    run(store, |conn| {
        conn.query_row(
            "SELECT * FROM probe WHERE key = ?",
            rusqlite::params![key],
            |row| row.get_gotime(column),
        )
    })
}

type SqliteOutcome = Result<Option<GoTime>, rusqlite::Error>;

/// `Ok(Some(t))` or a panic with the actual error, for the many cases the driver
/// really does decode.
fn fetched(outcome: SqliteOutcome, what: &str) -> GoTime {
    match outcome {
        Ok(Some(t)) => t,
        Ok(None) => panic!("{what}: expected a value, got NULL"),
        Err(e) => panic!("{what}: unexpected error {e}"),
    }
}

fn wire(store: &SQLStore, key: &str, column: &str, what: &str) -> String {
    fetched(gotime(store, key, column), what).to_rfc3339_nano()
}

fn utc(store: &SQLStore, key: &str, column: &str, what: &str) -> DateTime<Utc> {
    fetched(gotime(store, key, column), what).to_utc()
}

/// TEXT cells the nine layouts must accept, with the RFC3339Nano text Go's
/// `encoding/json` gives each one (offset preserved, wall clock in that zone).
#[test]
fn text_grammar_accepts_the_nine_layouts_and_keeps_the_parsed_zone() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        for (key, value) in [
            ("offset", "2024-02-02 03:00:00+03:00"),
            ("offset_t", "2024-02-02T03:00:00+03:00"),
            ("utc_z", "2024-02-02 03:00:00Z"),
            ("naive", "2024-02-02 03:00:00"),
            ("naive_t", "2024-02-02T03:00:00"),
            ("frac", "2024-02-02 03:00:00.500"),
            ("comma", "2024-02-02 03:00:00,5+03:00"),
            ("nine", "2024-02-02 03:00:00.123456789+05:45"),
            ("minute", "2024-02-02 03:00"),
            ("minute_t", "2024-02-02T03:00"),
            ("date_only", "2024-02-02"),
            ("one_digit_hour", "2024-02-02 3:00:00"),
            ("double_space", "2024-02-02  03:00:00"),
            ("one_z_kept", "2024-02-02 03:00:00+03:00Z"),
            ("zero_offset", "2024-02-02 03:00:00-00:00"),
            ("long_frac", "2024-02-02 03:00:00.999999999999+00:00"),
            ("leap_day", "2024-02-29 03:00:00"),
        ] {
            conn.execute(
                "INSERT INTO probe(key, dt) VALUES(?, ?)",
                rusqlite::params![key, value],
            )
            .unwrap_or_else(|e| panic!("{key} insert: {e}"));
        }
    });

    // The offset survives to the wire — the headline P2 parity fix.
    assert_eq!(
        wire(&store, "offset", "dt", "space layout with offset"),
        "2024-02-02T03:00:00+03:00",
        "Go marshals the value in the zone it parsed, not UTC"
    );
    // The INSTANT is still the offset-subtracted one.
    assert_eq!(
        utc(&store, "offset", "dt", "space layout with offset").to_rfc3339_lite(),
        "2024-02-02T00:00:00Z",
        "the UTC instant is wall time minus the offset"
    );
    assert_eq!(
        wire(&store, "offset_t", "dt", "T layout with offset"),
        "2024-02-02T03:00:00+03:00"
    );
    // UTC (and a literal zero offset) spell `Z`.
    for key in ["utc_z", "naive", "naive_t", "zero_offset"] {
        assert_eq!(
            wire(&store, key, "dt", key),
            "2024-02-02T03:00:00Z",
            "{key} is a UTC zone, rendered Z"
        );
    }
    // Fractional seconds, comma included, trailing zeros trimmed.
    assert_eq!(
        wire(&store, "frac", "dt", "period fraction"),
        "2024-02-02T03:00:00.5Z"
    );
    assert_eq!(
        wire(&store, "comma", "dt", "comma fraction"),
        "2024-02-02T03:00:00.5+03:00",
        "Go's commaOrPeriod accepts a comma exactly like a period"
    );
    assert_eq!(
        wire(&store, "nine", "dt", "nine digits + odd offset"),
        "2024-02-02T03:00:00.123456789+05:45"
    );
    // A fraction longer than nine digits is truncated to ten CHARACTERS
    // (separator counted), so `.999999999999` is `.999999999`.
    assert_eq!(
        wire(&store, "long_frac", "dt", "over-long fraction"),
        "2024-02-02T03:00:00.999999999Z"
    );
    // Minute-precision layouts fill seconds with zero.
    for key in ["minute", "minute_t"] {
        assert_eq!(wire(&store, key, "dt", key), "2024-02-02T03:00:00Z");
    }
    // Date-only layout: midnight.
    assert_eq!(
        wire(&store, "date_only", "dt", "date only"),
        "2024-02-02T00:00:00Z"
    );
    // `15` is `stdHour` — a one-digit hour parses; `skip` folds a space run.
    assert_eq!(
        wire(&store, "one_digit_hour", "dt", "one-digit hour"),
        "2024-02-02T03:00:00Z",
        "Go's getnum(value, false) for `15` accepts a single digit"
    );
    assert_eq!(
        wire(&store, "double_space", "dt", "double space"),
        "2024-02-02T03:00:00Z"
    );
    // `strings.TrimSuffix` removes exactly ONE final `Z`, so this still matches.
    assert_eq!(
        wire(&store, "one_z_kept", "dt", "one trailing Z"),
        "2024-02-02T03:00:00+03:00"
    );
    assert_eq!(
        wire(&store, "leap_day", "dt", "leap day"),
        "2024-02-29T03:00:00Z"
    );

    cleanup(store, path);
}

/// Spellings chrono accepts but Go's nine layouts do not; plus the DDL sentinel
/// and outright garbage. All of them are Go's zero time with NO read error — the
/// driver's failure arm (`:2832-2842`) never fails a TEXT DATETIME scan.
#[test]
fn chrono_only_spellings_and_garbage_scan_as_go_zero_with_no_error() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        for (key, value) in [
            // `+0300` / `+03` / `+1:30` are other Go std arms, not `-07:00`.
            ("compact_tz", "2024-02-02 03:00:00-0300"),
            ("short_tz", "2024-02-02 03:00:00+03"),
            ("thin_tz", "2024-02-02 03:00:00+1:30"),
            // chrono's `%:z` would take this; Go's layout has no space before it.
            ("spaced_tz", "2024-02-02 03:00:00 +03:00"),
            // Two `Z`s: TrimSuffix leaves one behind, which is extra text.
            ("double_z", "2024-02-02 03:00:00ZZ"),
            ("z_then_tz", "2024-02-02 03:00:00.5Z+03:00"),
            // Impossible calendar values.
            ("bad_month", "2024-13-02 03:00:00"),
            ("bad_leap", "2023-02-29 03:00:00"),
            ("short_year", "024-02-02 03:00:00"),
            ("single_month", "2024-2-02 03:00:00"),
            ("second_60", "2024-02-02 23:59:60"),
            ("hour_24", "2024-02-02 24:00:00"),
            // Out of range for the offset arm (`hr > 24`).
            ("tz_25", "2024-02-02T03:00:00+25:00"),
            // Extra text, lowercase `t`, tab separator, the DDL sentinel.
            ("trailing", "2024-02-02 03:00:00extra"),
            ("lower_t", "2024-02-02t03:00:00"),
            ("tab_sep", "2024-02-02\t03:00:00"),
            ("sentinel", "0000-00-00 00:00:00"),
            ("garbage", "not a date"),
            ("empty", ""),
            // A multi-byte rune inside the hour/offset slots: Go's parser indexes
            // raw bytes, so it is simply not a digit (`errBad`), never a panic.
            ("accent_hour", "2024-02-02 03:00:00\u{00e9}1:30"),
            ("euro_offset", "2024-02-02 03:00:00+03:\u{20ac}"),
            ("nbsp_tail", "2024-02-02 03:00:00\u{00a0}+03:00"),
            // `stdZeroMonth`/`stdZeroDay` are `fixed`, so a `+` in either slot is
            // `errBad` — a numeric parse (`str::parse`) would wrongly accept it.
            ("plus_month", "2024-+2-02 03:00:00"),
            ("plus_day", "2024-02-+2 03:00:00"),
        ] {
            conn.execute(
                "INSERT INTO probe(key, dt) VALUES(?, ?)",
                rusqlite::params![key, value],
            )
            .unwrap_or_else(|e| panic!("{key} insert: {e}"));
        }
    });

    for key in [
        "compact_tz",
        "short_tz",
        "thin_tz",
        "spaced_tz",
        "double_z",
        "z_then_tz",
        "bad_month",
        "bad_leap",
        "short_year",
        "single_month",
        "second_60",
        "hour_24",
        "tz_25",
        "trailing",
        "lower_t",
        "tab_sep",
        "sentinel",
        "garbage",
        "empty",
        "accent_hour",
        "euro_offset",
        "nbsp_tail",
        "plus_month",
        "plus_day",
    ] {
        // Go reports no error at all: the read succeeds and yields `time.Time{}`.
        let outcome = gotime(&store, key, "dt");
        let t = match outcome {
            Ok(Some(t)) => t,
            Ok(None) => panic!("{key}: NULL where a value was written"),
            Err(e) => panic!("{key}: the driver must NOT fail a TEXT datetime scan: {e}"),
        };
        assert!(
            t.is_go_zero(),
            "{key} must be Go's zero time, got {}",
            t.to_rfc3339_nano()
        );
        assert_eq!(t.to_rfc3339_nano(), "0001-01-01T00:00:00Z", "{key}");
        // The `*time.Time` shape is a POINTER to the zero, not an absent value.
        let opt = run(&store, |conn| {
            conn.query_row(
                "SELECT dt FROM probe WHERE key = ?",
                rusqlite::params![key],
                |row| row.get_opt_dt("dt"),
            )
        })
        .unwrap_or_else(|e| panic!("{key}: get_opt_dt errored: {e}"));
        assert_eq!(
            opt,
            Some(hydrate::go_zero_time()),
            "{key} is a pointer to Go's zero, not None"
        );
    }

    cleanup(store, path);
}

/// An offset Go's parser accepts but its RFC3339 formatter refuses to print
/// (`+24:60` is a 25-hour offset; `timezone hour outside of range [0,23]`). The
/// read succeeds; the port renders the UTC-normalised text because the DTO has no
/// error channel. Documented divergence, pinned here.
#[test]
fn an_offset_go_cannot_render_is_normalised_not_dropped() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt) VALUES('tz_2460', '2024-02-02 03:00:00+24:60')",
            [],
        )
        .unwrap();
    });
    let t = fetched(gotime(&store, "tz_2460", "dt"), "25-hour offset");
    assert_eq!(t.offset_secs, 90000, "hour ≤ 24 and minute ≤ 60 parse");
    assert!(!t.is_go_zero(), "the read succeeded");
    // Go's `json.Marshal` of this value ERRORS; the port renders UTC text instead.
    assert_eq!(t.to_rfc3339_nano(), "2024-02-01T02:00:00Z");
    cleanup(store, path);
}

/// INTEGER cells in the three declared timestamp spellings, executed: zero,
/// seconds, milliseconds, the strict `> 1e12` boundary, and Go's wrapping
/// multiplication. Verified against the pinned driver.
#[test]
fn integer_cells_in_declared_timestamp_columns_become_times() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        for (key, value) in [
            ("zero", 0_i64),
            ("seconds", 1_707_246_000_i64),
            ("millis", 1_707_246_000_000_i64),
            ("exactly_1e12", 1_000_000_000_000_i64),
            ("minus_1e12", -1_000_000_000_000_i64),
            ("just_over_1e12", 1_000_000_000_001_i64),
            ("just_under", -1_000_000_000_001_i64),
            ("int_max", i64::MAX),
            ("int_min", i64::MIN),
        ] {
            conn.execute(
                "INSERT INTO probe(key, dt, ts, d) VALUES(?, ?, ?, ?)",
                rusqlite::params![key, value, value, value],
            )
            .unwrap_or_else(|e| panic!("{key} insert: {e}"));
        }
        // SQLite really stores them as INTEGER, which is what drives the arm.
        for key in ["zero", "seconds", "millis", "int_max"] {
            let stored: String = conn
                .query_row(
                    "SELECT typeof(dt) FROM probe WHERE key = ?",
                    rusqlite::params![key],
                    |row| row.get::<_, String>(0),
                )
                .unwrap();
            assert_eq!(stored, "integer", "{key} must be stored as INTEGER");
        }
    });

    // 0 is `time.Unix(0, 0)`, NOT Go's zero time.
    let zero = utc(&store, "zero", "dt", "integer 0");
    assert_eq!(zero.to_rfc3339_lite(), "1970-01-01T00:00:00Z");
    assert!(
        !hydrate::is_go_zero(&zero),
        "Unix(0, 0) is not Go's zero time"
    );
    assert_eq!(
        utc(&store, "seconds", "dt", "seconds").to_rfc3339_lite(),
        "2024-02-06T19:00:00Z"
    );

    // ≤ 1e12 in magnitude is read as SECONDS — which for exactly ±1e12 lands far
    // outside the RFC3339 year range. Go's `json.Marshal` of that value ERRORS
    // (`year outside of range [0,9999]`); the port renders chrono's extended text
    // instead, since the DTO has no error channel. Pinned as measured.
    assert_eq!(
        utc(&store, "exactly_1e12", "dt", "exactly 1e12").to_rfc3339_lite(),
        "+33658-09-27T01:46:40Z",
        "the test is strictly `> 1e12`, so 1e12 is SECONDS, not milliseconds"
    );
    assert_eq!(
        utc(&store, "minus_1e12", "dt", "exactly -1e12").to_rfc3339_lite(),
        "-29719-04-05T22:13:20Z",
        "and -1e12 is seconds too"
    );

    // Beyond it is milliseconds (× 1e6, not × 1e9).
    assert_eq!(
        utc(&store, "millis", "dt", "milliseconds").to_rfc3339_lite(),
        "2024-02-06T19:00:00Z",
        "13-digit milliseconds land on the same instant as the seconds form"
    );
    assert_eq!(
        fetched(gotime(&store, "just_over_1e12", "dt"), "1e12+1").to_rfc3339_nano(),
        "2001-09-09T01:46:40.001Z",
        "1e12+1 ms is 1000000000.001 s"
    );
    assert_eq!(
        fetched(gotime(&store, "just_under", "dt"), "-(1e12+1)").to_rfc3339_nano(),
        "1938-04-24T22:13:19.999Z",
        "negative milliseconds normalise by borrowing a whole second"
    );

    // The multiplication is plain Go int64, so it WRAPS:
    // i64::MAX * 1e6 == -1_000_000 ns == 1969-12-31T23:59:59.999Z.
    let wrapped = utc(&store, "int_max", "dt", "i64::MAX");
    assert_eq!(wrapped.to_rfc3339_lite(), "1969-12-31T23:59:59Z");
    assert_eq!(
        fetched(gotime(&store, "int_max", "dt"), "i64::MAX").to_rfc3339_nano(),
        "1969-12-31T23:59:59.999Z",
        "Go's wrapping int64 multiply, verified against the driver"
    );
    // i64::MIN * 1e6 == 0 ns.
    assert_eq!(
        utc(&store, "int_min", "dt", "i64::MIN").to_rfc3339_lite(),
        "1970-01-01T00:00:00Z"
    );

    // All three declared spellings behave identically, and the integer path
    // carries NO offset (the driver calls `.UTC()`).
    for column in ["dt", "ts", "d"] {
        let t = fetched(gotime(&store, "seconds", column), column);
        assert_eq!(t.offset_secs, 0, "{column}: the integer path is UTC");
        assert_eq!(t.to_rfc3339_nano(), "2024-02-06T19:00:00Z");
    }

    cleanup(store, path);
}

/// The declared type gates the conversion. An INTEGER in a plain `INTEGER`
/// column, in `datetime(3)` (not one of the driver's three names), or in an
/// expression column has no timestamp meaning, and `database/sql` cannot store
/// the driver's `int64` into `time.Time` — a scan error, not a guess.
#[test]
fn the_declared_type_decides_and_a_non_timestamp_integer_errors() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt, plain, weird) VALUES('i', 1707246000, 1707246000, 1707246000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt) VALUES('t', '2024-02-02 03:00:00+03:00')",
            [],
        )
        .unwrap();
    });

    // Declared timestamp → decoded.
    assert_eq!(
        wire(&store, "i", "dt", "INTEGER in datetime"),
        "2024-02-06T19:00:00Z"
    );
    // Declared plain INTEGER → unsupported Scan, exactly Go's wording.
    let err = gotime(&store, "i", "plain").expect_err("int64 into time.Time must fail");
    assert!(
        err.to_string().contains("unsupported Scan") && err.to_string().contains("int64"),
        "Go's message names the driver type, got {err}"
    );
    // `datetime(3)` is not `datetime`: exact match, not a substring.
    let weird = gotime(&store, "i", "weird").expect_err("datetime(3) is not a timestamp column");
    assert!(
        weird.to_string().contains("unsupported Scan"),
        "got {weird}"
    );
    // An expression column has no decltype at all → the driver's `default` arm.
    let cast: Result<Option<GoTime>, rusqlite::Error> = run(&store, |conn| {
        conn.query_row(
            "SELECT CAST(dt AS INTEGER) AS expr FROM probe WHERE key = 't'",
            [],
            |row| row.get_gotime("expr"),
        )
    });
    assert!(
        cast.expect_err("an expression column has no declared type")
            .to_string()
            .contains("unsupported Scan"),
        "CAST(...) keeps its storage type"
    );

    // A TEXT CELL is gated on the declared type the same way (`sqlite3.go:2822`):
    // the three timestamp spellings parse, and everything else hands the driver a
    // `string`, which `database/sql` cannot store into `time.Time`. Verified
    // against the pinned driver on Go 1.27.4, where a declared `TEXT`/`varchar`
    // column and a bare string literal both give
    // `unsupported Scan, storing driver.Value type string into type *time.Time`.
    run(&store, |conn| {
        conn.execute(
            "INSERT INTO probe(key, dt, ts, d, txt, weird) VALUES('s', \
             '2024-02-02 03:00:00+03:00', '2024-02-02 03:00:00+03:00', \
             '2024-02-02', '2024-02-02 03:00:00+03:00', '2024-02-02 03:00:00+03:00')",
            [],
        )
        .unwrap();
    });
    for column in ["dt", "ts"] {
        assert_eq!(
            wire(&store, "s", column, column),
            "2024-02-02T03:00:00+03:00",
            "{column}: a TEXT cell in a declared timestamp column parses"
        );
    }
    // The DATE spelling takes the value through the date-only layout, so the
    // stored `2024-02-02` is midnight UTC.
    assert_eq!(wire(&store, "s", "d", "date"), "2024-02-02T00:00:00Z");
    for column in ["txt", "weird"] {
        let err = gotime(&store, "s", column)
            .expect_err("{column} is not one of the driver's three timestamp names");
        assert!(
            err.to_string().contains("unsupported Scan") && err.to_string().contains("string"),
            "Go's message names the driver type, got {err}"
        );
    }
    // A bare string literal has no declared type at all, so no conversion either.
    let literal: Result<Option<GoTime>, rusqlite::Error> = run(&store, |conn| {
        conn.query_row("SELECT '2024-02-02 03:00:00' AS dt", [], |row| {
            row.get_gotime("dt")
        })
    });
    assert!(
        literal
            .expect_err("an expression column has no declared type")
            .to_string()
            .contains("unsupported Scan"),
        "a string literal is a plain string for the driver"
    );
    // `CAST(dt AS TEXT)` likewise loses the declared type.
    let cast_text: Result<Option<GoTime>, rusqlite::Error> = run(&store, |conn| {
        conn.query_row(
            "SELECT CAST(dt AS TEXT) AS dt FROM probe WHERE key = 's'",
            [],
            |row| row.get_gotime("dt"),
        )
    });
    assert!(
        cast_text
            .expect_err("CAST(… AS TEXT) has no declared type")
            .to_string()
            .contains("unsupported Scan"),
        "the cast result is a plain string"
    );

    // A REAL and a BLOB cell likewise fail rather than being coerced.
    run(&store, |conn| {
        conn.execute("CREATE TABLE kinds (t datetime)", []).unwrap();
        conn.execute("INSERT INTO kinds VALUES(1.5)", []).unwrap();
        conn.execute("INSERT INTO kinds VALUES(x'0102')", [])
            .unwrap();
    });
    let real = run(&store, |conn| {
        conn.query_row("SELECT t FROM kinds LIMIT 1", [], |row| row.get_gotime("t"))
    });
    assert!(real
        .expect_err("float64 into time.Time")
        .to_string()
        .contains("float64"));
    let blob = run(&store, |conn| {
        conn.query_row("SELECT t FROM kinds LIMIT 1 OFFSET 1", [], |row| {
            row.get_gotime("t")
        })
    });
    assert!(blob
        .expect_err("[]byte into time.Time")
        .to_string()
        .contains("[]byte"));

    cleanup(store, path);
}

/// NULL stays Go's nil pointer for the optional getter and Go's
/// NULL-into-non-pointer error for the bare one — on the INTEGER path exactly as
/// on the TEXT path (`sqlite3.go:2832` assigns `nil` before the decltype switch).
#[test]
fn null_and_the_annotation_wire_text_keep_go_pointer_semantics() {
    let (store, path) = store();
    insert_user(&store, "u");
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        conn.execute("INSERT INTO probe(key, dt) VALUES('n', NULL)", [])
            .unwrap();
        // The annotation table's `starred_at`/`play_date`/`rated_at` are
        // `datetime` columns (`schema_ddl.rs`), which is where the offset
        // preservation is observable on the wire.
        conn.execute(
            "INSERT INTO annotation(user_id, item_id, item_type, play_count, play_date, \
             rating, starred, starred_at, rated_at) \
             VALUES('u', 'i', 'album', 3, '2024-02-02 03:00:00+03:00', 5, 1, \
             '2024-02-02 03:00:00+03:00', 1707246000)",
            [],
        )
        .unwrap();
    });

    assert_eq!(
        run(&store, |conn| conn.query_row(
            "SELECT * FROM probe WHERE key = 'n'",
            [],
            |row| row.get_opt_dt("dt"),
        ))
        .unwrap(),
        None,
        "SQL NULL is Go's nil pointer"
    );
    let null_required = run(&store, |conn| {
        conn.query_row("SELECT * FROM probe WHERE key = 'n'", [], |row| {
            row.get_dt("dt")
        })
    });
    assert!(
        null_required
            .expect_err("NULL into a non-pointer time.Time errors")
            .to_string()
            .contains("NULL value in required column"),
        "the bare getter keeps Go's NULL error"
    );

    // End-to-end through the annotation hydrator: the stored offset reaches the
    // DTO's wire text, and an INTEGER stamp goes through the integer arm.
    let annotation = run(&store, |conn| {
        conn.query_row(
            "SELECT a.play_count, a.play_date, a.rating, a.starred, a.starred_at, a.rated_at, \
             0.0 AS average_rating \
             FROM annotation a WHERE a.user_id = 'u' AND a.item_id = 'i'",
            [],
            hydrate::row_to_annotation,
        )
    })
    .expect("annotation hydrates");
    assert_eq!(
        annotation.play_date.as_deref(),
        Some("2024-02-02T03:00:00+03:00"),
        "the wire keeps the stored offset, not a UTC-normalised instant"
    );
    assert_eq!(
        annotation.starred_at.as_deref(),
        Some("2024-02-02T03:00:00+03:00")
    );
    assert_eq!(
        annotation.rated_at.as_deref(),
        Some("2024-02-06T19:00:00Z"),
        "an INTEGER stamp goes through the driver's integer arm"
    );
    assert_eq!(annotation.play_count, 3, "the rest of the row hydrates");
    assert_eq!(annotation.rating, 5);
    assert!(annotation.starred);

    cleanup(store, path);
}

/// A `+00:00` offset really is Go's UTC location and renders `Z`; and the zero
/// test follows Go's `IsZero`, which looks at the INSTANT, not the zone.
#[test]
fn zero_utc_renders_z_and_is_zero_follows_the_instant() {
    let (store, path) = store();
    run(&store, |conn| {
        conn.execute_batch(PROBE_DDL).unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt) VALUES('plus_zero', '2024-02-02 00:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt) VALUES('zero_instant_offset', '0001-01-01 03:00:00+03:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO probe(key, dt) VALUES('zero_instant_minus', '0001-01-01 00:00:00-03:00')",
            [],
        )
        .unwrap();
    });

    assert_eq!(
        wire(&store, "plus_zero", "dt", "+00:00"),
        "2024-02-02T00:00:00Z"
    );
    // Its instant is year-1 midnight UTC, so Go says IsZero while still printing
    // the offset.
    let zero_with_zone = fetched(gotime(&store, "zero_instant_offset", "dt"), "zero + 03:00");
    assert!(
        zero_with_zone.is_go_zero(),
        "Go's IsZero looks at the instant"
    );
    assert_eq!(
        zero_with_zone.to_rfc3339_nano(),
        "0001-01-01T03:00:00+03:00"
    );
    assert!(
        !fetched(gotime(&store, "zero_instant_minus", "dt"), "zero - 03:00").is_go_zero(),
        "its instant is not year-1 midnight UTC"
    );

    cleanup(store, path);
}

/// Pure-grammar coverage of the driver's two decode arms, so the boundary values
/// are exercised without a statement in the way.
#[test]
fn driver_decode_fns_match_the_pinned_oracle_values() {
    // TEXT
    assert_eq!(
        hydrate::driver_parse_text_dt("2024-02-02 03:00:00+03:00").to_rfc3339_nano(),
        "2024-02-02T03:00:00+03:00"
    );
    assert!(hydrate::driver_parse_text_dt("2024-02-02 03:00:00 -0300").is_go_zero());
    // Multi-byte runes mid-arm and a `+` in a fixed-width date field: Go's byte
    // indexing means `errBad`, and no slicing panic.
    assert!(hydrate::driver_parse_text_dt("2024-02-02 03:00:00\u{00e9}1:30").is_go_zero());
    assert!(hydrate::driver_parse_text_dt("2024-02-02 03:00:00+03:\u{20ac}").is_go_zero());
    assert!(hydrate::driver_parse_text_dt("2024-+2-02 03:00:00").is_go_zero());
    assert!(hydrate::driver_parse_text_dt("2024-02-+2 03:00:00").is_go_zero());
    assert_eq!(
        hydrate::driver_parse_text_dt("2024-02-02 03:00:00+24:60").offset_secs,
        90000
    );
    assert_eq!(
        hydrate::driver_parse_text_dt("2024-02-02 03:00:00.999999999999+00:00").to_rfc3339_nano(),
        "2024-02-02T03:00:00.999999999Z"
    );
    // INTEGER
    assert_eq!(
        hydrate::driver_integer_dt(0).to_rfc3339_nano(),
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(
        hydrate::driver_integer_dt(1_707_246_000).to_rfc3339_nano(),
        "2024-02-06T19:00:00Z"
    );
    assert_eq!(
        hydrate::driver_integer_dt(1_707_246_000_000).to_rfc3339_nano(),
        "2024-02-06T19:00:00Z"
    );
    assert_eq!(
        hydrate::driver_integer_dt(i64::MAX).to_rfc3339_nano(),
        "1969-12-31T23:59:59.999Z"
    );
    assert_eq!(
        hydrate::driver_integer_dt(i64::MIN).to_rfc3339_nano(),
        "1970-01-01T00:00:00Z"
    );
}

/// The DTO keeps the stamp as a plain `String` (`nd-model/src/annotation.rs`),
/// so the offset text the driver produced reaches the marshalled wire verbatim —
/// the codec does not re-normalise it to UTC.
#[test]
fn the_offset_text_reaches_the_annotation_dto_wire() {
    let (store, path) = store();
    insert_user(&store, "u");
    run(&store, |conn| {
        conn.execute(
            "INSERT INTO annotation(user_id, item_id, item_type, play_count, play_date, \
             rating, starred, starred_at, rated_at) \
             VALUES('u', 'i', 'album', 3, '2024-02-02 03:00:00+03:00', 5, 1, \
             '2024-02-02 03:00:00+03:00', '2024-02-02 03:00:00')",
            [],
        )
        .unwrap();
    });
    let annotation = run(&store, |conn| {
        conn.query_row(
            "SELECT play_count, play_date, rating, starred, starred_at, rated_at, \
             0.0 AS average_rating FROM annotation WHERE user_id = 'u' AND item_id = 'i'",
            [],
            hydrate::row_to_annotation,
        )
    })
    .expect("annotation hydrates");
    // Act: the DTO's own codec (Go's `json.Marshal` of `model.Annotations`).
    let wire = serde_json::to_string(&annotation).expect("DTO marshals");
    // Assert: offset preserved on the wire, UTC still `Z`.
    assert!(
        wire.contains("\"playDate\":\"2024-02-02T03:00:00+03:00\""),
        "offset reaches the wire, got {wire}"
    );
    assert!(
        wire.contains("\"starredAt\":\"2024-02-02T03:00:00+03:00\""),
        "got {wire}"
    );
    assert!(
        wire.contains("\"ratedAt\":\"2024-02-02T03:00:00Z\""),
        "a naive stamp is UTC and renders Z, got {wire}"
    );
    cleanup(store, path);
}

/// `DateTime<Utc>` → the RFC3339Nano text `encoding/json` gives a UTC `time.Time`,
/// which is what the record-side timestamps and [`hydrate::to_go_time_text`]
/// already use.
trait Lite {
    fn to_rfc3339_lite(&self) -> String;
}

impl Lite for DateTime<Utc> {
    /// Second-resolution `Z` text (chrono's `to_rfc3339` keeps a `.000` fraction,
    /// which Go would trim, so render the fields directly).
    fn to_rfc3339_lite(&self) -> String {
        self.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }
}
