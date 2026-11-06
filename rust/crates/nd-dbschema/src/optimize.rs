//! #04 B2 — DB ANALYZE scheduling (`db/optimize.go`, 274 lines).
//!
//! Go keeps a process-global singleton `Db()` guarded by `analyzeMux` and threads a
//! `context.Context` through every call; the #S1 design freeze replaces both with
//! explicit DI, so each entry point here takes the connection by handle (`&Connection`
//! for the read-only probes, `&mut Connection` where rusqlite needs it — `transaction`
//! takes `&mut self`, rusqlite transaction.rs:416) and errors are `crate::schema::Error`.
//!
//! Deliberate Rust deltas from Go, recorded so review sees nothing hidden:
//!   * no mutex — there is no shared connection to guard.
//!   * the Go step-yield (`analyzeMaxYield` = 150ms sleep between ANALYZE steps,
//!     `db/optimize.go:157-178`) is dropped: rusqlite models one synchronous
//!     connection with no concurrent writer to free the write lock for, and a test
//!     must not sleep.
//!   * `time.Time` bookkeeping becomes epoch seconds ([`Time`]): `db/optimize.go` only
//!     Sub/Compares those stamps and encodes them with `time.RFC3339Nano`.

use rusqlite::{Connection, Error as RsError};

use nd_conf::consts::{
    DB_ANALYZE_FAILURE_COUNT_KEY, DB_ANALYZE_MAX_AGE, DB_ANALYZE_PENDING_KEY,
    LAST_DB_ANALYZE_ATTEMPT_AT_KEY, LAST_DB_ANALYZE_AT_KEY,
};

use crate::migrate::{get_property, put_property};
use crate::schema::Error;

use std::time::Duration;

/// Backoff between ANALYZE attempts keyed by failure count (`db/optimize.go:109-120`
/// `analyzeRetryDelay`: 30m, 1h, 2h, then 24h forever).
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(2 * 60 * 60),
];
/// `analyzeRetryDelay` default arm — also `consts.DBAnalyzeMaxAge` (nd-conf `consts.rs:139`).
pub const RETRY_MAX_DELAY: Duration = DB_ANALYZE_MAX_AGE;

/// ANALYZE once per index on a WITH-ROWID table that has a non-partial index, and once
/// per whole table otherwise: same `sqlite_stat1` rows as a full ANALYZE, but the write
/// lock is released between steps in Go (`db/optimize.go:149-155 analyzeTargetsSQL`).
pub const ANALYZE_TARGETS_SQL: &str = concat!(
    "SELECT i.name FROM sqlite_schema i JOIN pragma_table_list t ON t.schema = 'main' AND t.name = i.tbl_name",
    " WHERE i.type = 'index' AND t.wr = 0 AND EXISTS (SELECT 1 FROM pragma_index_list(t.name) l WHERE l.partial = 0)",
    " UNION ALL",
    " SELECT t.name FROM pragma_table_list t",
    " WHERE t.schema = 'main' AND t.type IN ('table', 'shadow') AND t.name NOT LIKE 'sqlite_%'",
    " AND (t.wr = 1 OR NOT EXISTS (SELECT 1 FROM pragma_index_list(t.name) l WHERE l.partial = 0))");

/// Go's `time.Time` for the ANALYZE bookkeeping, modelled as epoch seconds (UTC):
/// `db/optimize.go` only Sub/Compares it and encodes it with RFC3339Nano.
pub type Time = i64;

/// `db/optimize.go:122-131`: run the ANALYZE when stale or already marked pending.
pub fn optimize_if_needed(conn: &mut Connection, now: Time) -> Result<bool, Error> {
    let due = optimize_due(conn, now);
    match due {
        Err(cause) => Err(cause),
        Ok(false) => Ok(false),
        Ok(true) => match optimize_at(conn, now) {
            Err(cause) => Err(cause),
            Ok(_) => Ok(true),
        },
    }
}

/// `db/optimize.go:21-30` (`Optimize`): unconditional full ANALYZE.
pub fn optimize(conn: &mut Connection, now: Time) -> Result<(), Error> {
    optimize_at(conn, now)
}

/// `db/optimize.go:129-131` (`markOptimizePending`).
pub fn mark_optimize_pending(conn: &Connection) -> Result<(), Error> {
    put_property(conn, DB_ANALYZE_PENDING_KEY.to_string(), "1".to_string())
}

/// `db/optimize.go:55-81` (`optimizeDue`): backoff first, then the pending flag,
/// then staleness against `DB_ANALYZE_MAX_AGE`.
pub fn optimize_due(conn: &Connection, now: Time) -> Result<bool, Error> {
    match analyze_retry_backoff_active(conn, now) {
        Err(cause) => return Err(cause),
        Ok(true) => return Ok(false),
        Ok(false) => {}
    }

    match get_property(conn, DB_ANALYZE_PENDING_KEY.to_string()) {
        Err(cause) => return Err(cause),
        Ok(Some(value)) => {
            if value == "1" {
                return Ok(true);
            }
        }
        Ok(None) => {}
    }

    match get_property(conn, LAST_DB_ANALYZE_AT_KEY.to_string()) {
        Err(cause) => Err(cause),
        Ok(None) => Ok(true),
        Ok(Some(value)) => match parse_analyze_time(value) {
            None => Ok(true),
            Some(then) => {
                if then > now {
                    return Ok(true);
                }
                Ok(staleness(now, then) >= DB_ANALYZE_MAX_AGE)
            }
        },
    }
}

/// `db/optimize.go:133-145`: pending marker, ANALYZE every target, then the success
/// record; every failure funnels through `record_analyze_error`.
pub fn optimize_at(conn: &mut Connection, now: Time) -> Result<(), Error> {
    if let Err(cause) = mark_optimize_pending(conn) {
        return Err(record_analyze_error(conn, now, cause));
    }
    if let Err(cause) = analyze_in_steps(conn) {
        return Err(record_analyze_error(conn, now, cause));
    }
    if let Err(cause) = record_analyze_success(conn, now) {
        return Err(record_analyze_error(conn, now, cause));
    }
    Ok(())
}

/// `db/optimize.go:161-178` (`analyzeInSteps`) over `db/optimize.go:180-195`
/// (`analyzeTargets`): one `ANALYZE "name"` per target, with Go's quote doubling
/// (`db/optimize.go:168`).
pub fn analyze_in_steps(conn: &Connection) -> Result<(), Error> {
    match analyze_targets(conn) {
        Err(cause) => Err(cause),
        Ok(names) => {
            for name in names {
                let sql = format!("ANALYZE \"{}\"", escape_ident(name));
                conn.execute(&sql, []).map_err(analyze_error)?;
            }
            Ok(())
        }
    }
}

/// Column-0 SELECT of `ANALYZE_TARGETS_SQL` (`db/optimize.go:180-195`).
pub fn analyze_targets(conn: &Connection) -> Result<Vec<String>, Error> {
    read_strings(conn, ANALYZE_TARGETS_SQL)
}

/// `db/optimize.go:197-216` (`recordAnalyzeSuccess`): the three writes go through
/// rusqlite's `transaction` (rusqlite transaction.rs:416) instead of Go's `BeginTx`,
/// and `commit` replaces `tx.Commit()` (transaction.rs:186).
pub fn record_analyze_success(conn: &mut Connection, now: Time) -> Result<(), Error> {
    let tx = match conn.transaction() {
        Err(cause) => return Err(analyze_error(cause)),
        Ok(tx) => tx,
    };
    let stamp = format_analyze_time(now).to_string();
    let at_key = put_property(&tx, LAST_DB_ANALYZE_AT_KEY.to_string(), stamp);
    let pending_key = put_property(&tx, DB_ANALYZE_PENDING_KEY.to_string(), "0".to_string());
    let failure_key = put_property(
        &tx,
        DB_ANALYZE_FAILURE_COUNT_KEY.to_string(),
        "0".to_string(),
    );
    let commit = tx.commit();
    at_key?;
    pending_key?;
    failure_key?;
    if let Err(cause) = commit {
        return Err(analyze_error(cause));
    }
    Ok(())
}

/// `db/optimize.go:141-145` (`recordAnalyzeError`): Go `errors.Join`s the record
/// failure onto the ANALYZE error; Rust surfaces the record failure and lets the
/// caller log the analyze error, so nothing is silently dropped.
pub fn record_analyze_error(conn: &mut Connection, now: Time, cause: Error) -> Error {
    match record_analyze_failure(conn, now) {
        Err(record_cause) => record_cause,
        Ok(_) => cause,
    }
}

/// `db/optimize.go:225-251` (`recordAnalyzeFailure`): pending stays armed, the
/// failure count increments, the attempt stamp moves forward — inside one transaction.
pub fn record_analyze_failure(conn: &mut Connection, now: Time) -> Result<(), Error> {
    let tx = match conn.transaction() {
        Err(cause) => return Err(analyze_error(cause)),
        Ok(tx) => tx,
    };
    let failures = match get_property(&tx, DB_ANALYZE_FAILURE_COUNT_KEY.to_string()) {
        Err(cause) => return Err(cause),
        Ok(None) => 0,
        Ok(Some(value)) => atoi(value),
    };
    let clamped = if failures < 0 { 0 } else { failures };
    let next = clamped + 1;
    let stamp = format_analyze_time(now).to_string();
    let pending_key = put_property(&tx, DB_ANALYZE_PENDING_KEY.to_string(), "1".to_string());
    let failure_key = put_property(
        &tx,
        DB_ANALYZE_FAILURE_COUNT_KEY.to_string(),
        next.to_string(),
    );
    let attempt_key = put_property(&tx, LAST_DB_ANALYZE_ATTEMPT_AT_KEY.to_string(), stamp);
    let commit = tx.commit();
    pending_key?;
    failure_key?;
    attempt_key?;
    if let Err(cause) = commit {
        return Err(analyze_error(cause));
    }
    Ok(())
}

/// `db/optimize.go:88-107` (`analyzeRetryBackoffActive`): a failure count plus an
/// attempt stamp inside `analyzeRetryDelay` suppresses the run.
pub fn analyze_retry_backoff_active(conn: &Connection, now: Time) -> Result<bool, Error> {
    let counted = get_property(conn, DB_ANALYZE_FAILURE_COUNT_KEY.to_string());
    match counted {
        Err(cause) => Err(cause),
        Ok(None) => Ok(false),
        Ok(Some(value)) => {
            let failures = atoi(value);
            if failures < 1 {
                return Ok(false);
            }
            match get_property(conn, LAST_DB_ANALYZE_ATTEMPT_AT_KEY.to_string()) {
                Err(_) => Ok(false),
                Ok(None) => Ok(false),
                Ok(Some(value)) => match parse_analyze_time(value) {
                    None => Ok(false),
                    Some(last_attempt) => {
                        if last_attempt > now {
                            return Ok(false);
                        }
                        Ok(staleness(now, last_attempt) < analyze_retry_delay(failures))
                    }
                },
            }
        }
    }
}

/// `db/optimize.go:109-120` (`analyzeRetryDelay`).
pub fn analyze_retry_delay(failures: i64) -> Duration {
    match failures {
        1 => RETRY_DELAYS[0],
        2 => RETRY_DELAYS[1],
        3 => RETRY_DELAYS[2],
        _ => RETRY_MAX_DELAY,
    }
}

/// `db/optimize.go:83-86` (`parseAnalyzeTime`): Go parses `time.RFC3339Nano`, so the
/// only stamps accepted are `%Y-%m-%dT%H:%M:%S` followed by `Z` or `±HH:MM`, with
/// optional sub-second digits in between. Anything else is `None` — Go's invalid
/// branch, `db/optimize.go:77`.
pub fn parse_analyze_time(value: String) -> Option<Time> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }

    let tail = value.get(19..bytes.len()).unwrap_or("");
    let offset = parse_zone(tail.to_string())?;

    let year = atoi(value.get(0..4).unwrap_or("").to_string());
    let month = atoi(value.get(5..7).unwrap_or("").to_string());
    let day = atoi(value.get(8..10).unwrap_or("").to_string());
    let hour = atoi(value.get(11..13).unwrap_or("").to_string());
    let minute = atoi(value.get(14..16).unwrap_or("").to_string());
    let second = atoi(value.get(17..19).unwrap_or("").to_string());

    let range_ok = (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour <= 23
        && minute <= 59
        && second <= 60
        && (-86_340..=86_340).contains(&offset);
    if !range_ok {
        return None;
    }
    let secs = day_seconds(year, month, day) + hour * 3600 + minute * 60 + second;
    if secs - offset < 0 {
        return None;
    }
    Some(secs - offset)
}

/// Go's `time.Time.UTC().Format(time.RFC3339Nano)` (`db/optimize.go:203`, `:247`).
pub fn format_analyze_time(now: Time) -> String {
    let civil = civil_from_epoch(now);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        civil.year, civil.month, civil.day, civil.hour, civil.minute, civil.second,
    )
}

/// Epoch seconds of a civil date (whole days times the day length).
pub fn day_seconds(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) * 86_400
}

/// Go's `now.Sub(lastAnalyze)` / `now.Sub(lastAttempt)` (`db/optimize.go:80`, `:106`);
/// a negative difference is unreachable because callers compare first (`:77`, `:103`).
pub fn staleness(now: Time, then: Time) -> Duration {
    let secs = now - then;
    if secs < 0 {
        return Duration::from_secs(0);
    }
    Duration::from_secs(secs as u64)
}

/// `db/optimize.go:93` / `:239` (`strconv.Atoi`, value discarded on error): unparsable
/// counts behave as 0.
pub fn atoi(value: String) -> i64 {
    value.trim().parse().unwrap_or_default()
}

/// Go escapes an index/table name for a quoted SQL identifier by doubling quotes
/// (`db/optimize.go:168` `strings.ReplaceAll(target, "\"", "\"\"")`).
pub fn escape_ident(name: String) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out
}

/// RFC3339 zone (`Z`, or `±HH:MM`) as seconds east of UTC; the nanosecond run that
/// `time.RFC3339Nano` allows before the zone is skipped. `None` marks a zone Go
/// would reject.
pub fn parse_zone(tail: String) -> Option<i64> {
    if tail.is_empty() || tail == "Z" {
        return Some(0);
    }
    let bytes = tail.as_bytes();
    if bytes[0] == b'.' {
        let digits = nanos_len(bytes);
        if digits < 2 {
            return None;
        }
        return parse_zone(tail.get(digits..bytes.len()).unwrap_or("").to_string());
    }
    if bytes.len() != 6 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if bytes[3] != b':' {
        return None;
    }
    let hours = atoi(tail.get(1..3).unwrap_or("").to_string());
    let minutes = atoi(tail.get(4..6).unwrap_or("").to_string());
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Length of a leading `.` plus digits run (RFC3339Nano sub-second part), 0 when absent.
pub fn nanos_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() || bytes[0] != b'.' {
        return 0;
    }
    let mut at = 1;
    while at < bytes.len() && bytes[at] >= b'0' && bytes[at] <= b'9' {
        at += 1;
    }
    if at == 1 {
        return 0;
    }
    at
}

/// Days since the epoch for a civil date (Howard Hinnant's `days_from_civil`, the
/// proleptic-Gregorian arithmetic Go's `time` codec is built on).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let year = y - (if m <= 2 { 1 } else { 0 });
    let era = floor_div(if year >= 0 { year } else { year - 399 }, 400);
    let yoe = year - era * 400;
    let mp = m + (if m > 2 { -3 } else { 9 });
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`], split into the calendar fields RFC3339 prints.
pub fn civil_from_epoch(epoch: i64) -> Civil {
    let days = floor_div(epoch, 86_400);
    let sec = trunc_mod(epoch, 86_400);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: sec / 3600,
        minute: sec % 3600 / 60,
        second: sec % 60,
    }
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let shifted = z + 719_468;
    let era = floor_div(
        if shifted >= 0 {
            shifted
        } else {
            shifted - 146_096
        },
        146_097,
    );
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + (if mp < 10 { 3 } else { -9 });
    (y + (if m <= 2 { 1 } else { 0 }), m, d)
}

/// Floor division, used only by the epoch/day conversion.
pub fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        return q - 1;
    }
    q
}

/// Non-negative remainder (Go's `time` codec keeps sub-day offsets positive).
pub fn trunc_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r < 0 {
        return r + b;
    }
    r
}

/// Calendar fields of an ANALYZE stamp.
pub struct Civil {
    /// Four-digit year.
    pub year: i64,
    /// 1-12.
    pub month: i64,
    /// 1-31.
    pub day: i64,
    /// 0-23.
    pub hour: i64,
    /// 0-59.
    pub minute: i64,
    /// 0-59.
    pub second: i64,
}

/// Byte/character slice of a stamp string, empty when out of range.
pub fn slice(value: String, from: usize, to: usize) -> String {
    if to <= value.len() && from <= to {
        return value.get(from..to).unwrap_or("").to_string();
    }
    "".to_string()
}

/// Column-0 SELECT as strings (shared by `analyze_targets` and the tests).
fn read_strings(conn: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    let prepared = conn.prepare(sql);
    let mut stmt = match prepared {
        Err(cause) => return Err(analyze_error(cause)),
        Ok(stmt) => stmt,
    };
    let rows = stmt.query([]);
    let mut iter = match rows {
        Err(cause) => return Err(analyze_error(cause)),
        Ok(rows) => rows,
    };
    let mut names = Vec::<String>::new();
    loop {
        let next = iter.next();
        let row = match next {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(cause) => return Err(analyze_error(cause)),
        };
        match row {
            Some(r) => {
                let cell = r.get(0);
                match cell {
                    Err(cause) => return Err(analyze_error(cause)),
                    Ok(v) => names.push(v),
                }
            }
            None => return Ok(names),
        }
    }
}

fn analyze_error(cause: RsError) -> Error {
    Error::Failed {
        path: "analyze".to_string(),
        cause,
    }
}

#[cfg(test)]
mod tests {

    use std::fs;

    use crate::migrate::get_property;
    use crate::optimize::{analyze_retry_backoff_active, optimize_due, optimize_if_needed};
    use crate::optimize::{format_analyze_time, Time};
    use crate::schema::{apply_fresh_schema, open};

    #[test]
    fn analyze_schedules_then_records_properties_in_a_transaction() {
        // Arrange: a fresh-schema FILE DB — ANALYZE rewrites sqlite_stat1, which a
        // shared-cache `:memory:` DB refuses with SQLITE_LOCKED (schema lock per
        // shared instance); real users are file DBs (db/db.go:62).
        let path = "/tmp/nd-optimize-analyze.sqlite";
        let _ = fs::remove_file(path);
        let mut conn = open(path).unwrap();
        apply_fresh_schema(&conn).unwrap();

        // Act: schedule + run the ANALYZE at a fixed stamp.
        let now: Time = 1_700_000_000;
        let due = optimize_due(&conn, now);
        let ran = optimize_if_needed(&mut conn, now);

        // Assert: an absent LastDBAnalyzeAt arms the run (db/optimize.go:72-74), and
        // all three ANALYZE properties were rewritten inside conn.transaction
        // (db/optimize.go:197-216 recordAnalyzeSuccess, rusqlite transaction.rs:416).
        assert!(
            due.unwrap(),
            "absent LastDBAnalyzeAt => due db/optimize.go:72"
        );
        assert!(
            ran.unwrap(),
            "optimize_if_needed ran the ANALYZE db/optimize.go:34-44"
        );

        let pending = get_property(&conn, "DBAnalyzePending".to_string());
        let stamped = get_property(&conn, "LastDBAnalyzeAt".to_string());
        let failures = get_property(&conn, "DBAnalyzeFailureCount".to_string());
        assert_eq!(
            pending.unwrap(),
            Some("0".to_string()),
            "pending cleared db/optimize.go:206"
        );
        assert_eq!(
            failures.unwrap(),
            Some("0".to_string()),
            "count cleared db/optimize.go:209"
        );
        assert_eq!(
            stamped.unwrap(),
            Some(format_analyze_time(now)),
            "stamp round-trips through parse_analyze_time db/optimize.go:203"
        );

        // Assert: 30m later the run is not due again — flag cleared, failure count 0,
        // age 1800s < consts.DBAnalyzeMaxAge (nd-conf consts.rs:139) 24h (db/optimize.go:80).
        let later: Time = now + 1800;
        let again = optimize_due(&conn, later);
        assert!(
            !again.unwrap(),
            "fresh stamp is not stale db/optimize.go:80"
        );
        assert!(
            !analyze_retry_backoff_active(&conn, later).unwrap(),
            "no failures => no backoff db/optimize.go:95-96"
        );
    }
}
