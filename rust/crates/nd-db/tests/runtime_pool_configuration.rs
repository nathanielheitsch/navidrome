//! DB-03 (D-OPEN decision, option a) — every pool handle must hold the settings the
//! DSN resolves to, and the DB file SQLite names must be the requested path.
//!
//! The measured divergences this suite pins, each reproduced here against executed
//! SQLite state (go-sqlite3 v1.14.52 values are quoted from a Go run of
//! `sql.Open` + `SELECT 1` + `PRAGMA` read-back, command at the bottom of this
//! file; no Go binary is part of this suite):
//!   * handing SQLite Go's default DSN verbatim makes rusqlite create a file whose
//!     NAME contains the query string and leave the state at `delete`/5000/2 — so
//!     [`nd_dbschema::schema::parse_open_input`] strips the go-sqlite3 conventions
//!     and [`nd_dbschema::schema::apply_open_options`] applies explicit pragmas.
//!   * Navidrome's WAL/15000/FK-on preset belongs to its **default** DbPath only; a
//!     custom bare path gets go-sqlite3's driver state (`delete`-ish journal
//!     untouched, busy 5000, sync NORMAL, FK pragma untouched).
//!   * `cache=shared` reaches SQLite only through a `file:` URI.
//!
//! Non-vacuity per the DB-03 plan: every lease is checked out and held BEFORE any
//! assertion runs, so LIFO reuse of one already-configured handle cannot mask an
//! unconfigured sibling. Settings are read through SQLite's own pragmas
//! ([`nd_dbschema::schema::effective_state`]) rather than inferred from the input,
//! and an ignored custom timeout fails the exact read-back against rusqlite's
//! install-time 5000 (`rusqlite/src/inner_connection.rs:119`).
//! Non-vacuity per the DB-03 plan: every lease is checked out and held BEFORE any
//! assertion runs, so LIFO reuse of one already-configured handle cannot mask an
//! unconfigured sibling. Settings are read through SQLite's own pragmas
//! ([`nd_dbschema::schema::effective_state`]) rather than inferred from the input,
//! and an ignored custom timeout fails the exact read-back against rusqlite's
//! install-time 5000 (`rusqlite/src/inner_connection.rs:119`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nd_conf::consts::DEFAULT_DB_PATH;
use nd_db::pool::{Lease, Pool, PoolConfig};
use nd_db::store::is_busy;
use nd_dbschema::schema::{
    apply_fresh_schema, effective_state, open, open_with_options, parse_open_input,
    set_foreign_keys, DbOpenOptions, Error as SchemaError, JournalMode, Synchronous,
    DRIVER_BUSY_TIMEOUT_MS, MEMORY_DSN, NAVIDROME_BUSY_TIMEOUT_MS, SEEDEDRAND_SQL,
};

/// Root for this suite's temp dirs. File-backed on purpose: the DB-03 plan forbids
/// `:memory:` for the file/path assertions.
const TEMP_ROOT: &str = "/tmp/nd-db-runtime-pool-configuration";

/// A clean per-test temp directory, so the directory listing below proves which
/// files the pool actually created.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(TEMP_ROOT).join(tag);
    // A stale directory would make the "exactly one file" assertion meaningless.
    match fs::remove_dir_all(&dir) {
        Ok(()) => (),
        Err(_absent) => (),
    }
    if let Err(cause) = fs::create_dir_all(&dir) {
        panic!("cannot create temp dir {}: {cause}", dir.display());
    }
    dir
}

/// Sorted names currently in `dir`. Any name containing `?` is the
/// query-string-in-the-filename artifact DB-03 removes.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(item) => names.push(item.file_name().to_string_lossy().into_owned()),
                    Err(cause) => panic!("cannot read entry of {}: {cause}", dir.display()),
                }
            }
        }
        Err(cause) => panic!("cannot list {}: {cause}", dir.display()),
    }
    names.sort();
    names
}

/// `Config::normalize_and_validate`'s own construction (`nd-conf/src/config.rs:999-1010`):
/// the data folder joined to the DSN's path component plus its parameter string.
fn go_dsn(dir: &Path) -> String {
    let (head, params) = match DEFAULT_DB_PATH.find('?') {
        Some(at) => (&DEFAULT_DB_PATH[..at], &DEFAULT_DB_PATH[at..]),
        None => (DEFAULT_DB_PATH, ""),
    };
    let joined = dir.join(head);
    match joined.to_str() {
        Some(text) => format!("{text}{params}"),
        None => panic!("temp path is not valid UTF-8: {}", joined.display()),
    }
}

/// `dir/<name>` as a string, panicking on a non-UTF-8 path.
/// An ABSOLUTE `mode=memory` URI. A relative `file:name?mode=memory` still gives a
/// memory DB, but a journal-switch pragma on it is what can materialize `name` in
/// the process working directory (the crate directory for these binaries), so every
/// memory DB here is addressed absolutely inside the test's temp dir.
fn memory_uri(dir: &Path, name: &str, extra: &str) -> String {
    format!("file:{}?mode=memory{extra}", in_dir(dir, name))
}

fn in_dir(dir: &Path, name: &str) -> String {
    let joined = dir.join(name);
    match joined.to_str() {
        Some(text) => text.to_string(),
        None => panic!("temp path is not valid UTF-8: {}", joined.display()),
    }
}

/// Fail loudly if this filesystem cannot do WAL — the DB-03 plan forbids silently
/// downgrading the WAL assertion. The probe asks for WAL explicitly: a bare path now
/// resolves to go-sqlite3's driver state, which names no journal mode at all.
fn require_wal_support(dir: &Path) {
    let probe = in_dir(dir, "wal-support-probe.db");
    let wanted = DbOpenOptions {
        journal_mode: Some(JournalMode::Wal),
        ..DbOpenOptions::driver_defaults()
    };
    match open_with_options(&probe, Some(wanted)) {
        Ok(conn) => {
            let state = effective_state(&conn);
            drop(conn);
            match state {
                Ok(got) if got.journal_mode == JournalMode::Wal.sqlite_name() => (),
                Ok(got) => panic!(
                    "STOP AND REPORT: WAL is unavailable on {} (journal_mode reads {}). \
                     The DB-03 assertions must not be weakened.",
                    dir.display(),
                    got.journal_mode
                ),
                Err(cause) => {
                    panic!(
                        "STOP AND REPORT: WAL pragma failed on {}: {cause}",
                        dir.display()
                    )
                }
            }
        }
        Err(cause) => panic!(
            "STOP AND REPORT: cannot open a WAL probe db in {}: {cause}",
            dir.display()
        ),
    }
    match fs::remove_file(&probe) {
        Ok(()) => (),
        Err(_absent) => (),
    }
}

/// Check out and HOLD every lease, then run `check` on each. Nothing is released
/// between handles, so a pool that recycled one good handle cannot pass.
fn every_handle<F>(pool: &Pool, want_size: usize, mut check: F)
where
    F: FnMut(&rusqlite::Connection, usize),
{
    let mut leases: Vec<Lease<'_>> = Vec::new();
    while leases.len() < want_size {
        match pool.checkout() {
            Ok(lease) => leases.push(lease),
            Err(cause) => panic!(
                "pool must serve lease {} of {}: {cause}",
                leases.len() + 1,
                want_size
            ),
        }
    }
    match pool.idle_count() {
        Ok(idle) => assert_eq!(idle, 0, "all {want_size} leases are held simultaneously"),
        Err(cause) => panic!("idle_count: {cause}"),
    }
    for (index, lease) in leases.iter().enumerate() {
        check(lease.conn(), index);
    }
    // Each lease lends its own handle: a repeated address would mean double-lending.
    let mut addresses: Vec<usize> = Vec::new();
    for lease in leases.iter() {
        addresses.push(lease.conn() as *const rusqlite::Connection as usize);
    }
    addresses.sort();
    addresses.dedup();
    assert_eq!(
        addresses.len(),
        want_size,
        "each lease must lend a distinct connection handle"
    );
}

/// Requested-vs-effective comparison for one file-backed handle, including the
/// file name SQLite reports. Settings the DSN did not name (`None`) are asserted to
/// be untouched rather than to hold some particular value: go-sqlite3 execs no
/// pragma for them (`sqlite3.go:1851-1853`, `:1865-1867`), so a journal the file
/// already has, or this build's compiled-in FK default, are both acceptable.
fn assert_handle_matches(conn: &rusqlite::Connection, want: DbOpenOptions, index: usize) {
    let got = match effective_state(conn) {
        Ok(state) => state,
        Err(cause) => panic!("handle {index}: cannot read effective state: {cause}"),
    };
    if let Some(mode) = want.journal_mode {
        assert_eq!(
            got.journal_mode.to_lowercase(),
            mode.sqlite_name(),
            "handle {index}: journal_mode"
        );
    }
    assert_eq!(
        got.busy_timeout,
        requested_ms(want.busy_timeout),
        "handle {index}: busy_timeout"
    );
    assert_eq!(
        got.synchronous,
        want.synchronous.code(),
        "handle {index}: synchronous"
    );
    if let Some(enabled) = want.foreign_keys {
        assert_eq!(got.foreign_keys, enabled, "handle {index}: foreign_keys");
    }
    assert!(
        got.busy_timeout != 5000 || want.busy_timeout == 5000,
        "handle {index} still reports rusqlite's install-time 5000 — the requested \
         timeout was ignored, which is the divergence DB-03 removes"
    );
    match got.db_file {
        Some(name) => assert!(
            !name.contains('?') && !name.contains('&'),
            "handle {index} opened a file whose name carries the query string: {name}"
        ),
        // A memory DB names no file; every file-backed case asserts the name below.
        None => assert_eq!(
            got.journal_mode,
            JournalMode::Memory.sqlite_name(),
            "handle {index} reports no database file but is not a memory DB"
        ),
    }
}

/// The absolute path SQLite reports for `dir/<name>` (it canonicalizes the name it
/// was handed, so comparisons go through the filesystem).
fn canonical_of(dir: &Path, name: &str) -> String {
    let joined = dir.join(name);
    match fs::canonicalize(&joined) {
        Ok(real) => match real.to_str() {
            Some(text) => text.to_string(),
            None => panic!("canonical path is not valid UTF-8: {}", real.display()),
        },
        Err(cause) => panic!("cannot canonicalize {}: {cause}", joined.display()),
    }
}

/// SQLite reports `PRAGMA busy_timeout` as an int; the requested value is a u64.
fn requested_ms(value: u64) -> i64 {
    match i64::try_from(value) {
        Ok(ms) => ms,
        Err(_too_large) => panic!("requested busy_timeout {value} does not fit SQLite's int"),
    }
}

/// Read one handle's four settings, panicking on failure.
fn state_of(conn: &rusqlite::Connection, label: &str) -> nd_dbschema::schema::EffectiveState {
    match effective_state(conn) {
        Ok(state) => state,
        Err(cause) => panic!("{label}: cannot read effective state: {cause}"),
    }
}

/// Open a pool, panicking with `label` on failure. Returns pool + first handle.
fn build_pool(config: PoolConfig, label: &str) -> (Pool, nd_db::pool::Initializer) {
    match Pool::new(&config) {
        Ok(built) => built,
        Err(cause) => panic!("{label}: {cause}"),
    }
}

/// `journal=wal busy=15000 sync=1 fk=1` — the Go measurement for the default DSN.
const GO_DEFAULT_JOURNAL: &str = "wal";
const GO_DEFAULT_BUSY: i64 = 15000;
const GO_DEFAULT_SYNC: i64 = 1;
const GO_DEFAULT_FK: bool = true;

#[test]
fn every_pooled_handle_holds_the_default_dsn_settings_and_names_the_requested_file() {
    let dir = temp_dir("go-dsn");
    require_wal_support(&dir);
    let dsn = go_dsn(&dir);
    let want_size: usize = 4;

    let (pool, first) = build_pool(PoolConfig::with_size(dsn.clone(), want_size), "default DSN");
    // The pool still reports the configured input verbatim (Go's `db.Path`).
    assert_eq!(pool.path(), dsn, "Pool::path keeps the configured input");

    let want = DbOpenOptions::navidrome_default_options();
    let wanted_file = canonical_of(&dir, "navidrome.db");
    // The handle Pool::new held out is checked too, and put back afterwards.
    assert_handle_matches(&first, want, 0);
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, want_size, |conn, index| {
        assert_handle_matches(conn, want, index);
        let state = state_of(conn, &format!("handle {index}"));
        assert_eq!(
            state.db_file.as_deref(),
            Some(wanted_file.as_str()),
            "handle {index} opened a different file than the DSN names"
        );
    });

    // SQLite's own view spelled out once more, so a regression inside
    // `effective_state` cannot make every assertion above agree with nothing.
    let one = pool.checkout().unwrap_or_else(|cause| panic!("{cause}"));
    let state = state_of(one.conn(), "default DSN handle");
    assert_eq!(state.journal_mode, GO_DEFAULT_JOURNAL);
    assert_eq!(
        state.busy_timeout,
        requested_ms(NAVIDROME_BUSY_TIMEOUT_MS),
        "Go's _busy_timeout=15000"
    );
    assert_eq!(state.synchronous, GO_DEFAULT_SYNC);
    assert!(state.foreign_keys, "Go's DSN asks _foreign_keys=on");
}

#[test]
fn default_dsn_sqlite_arg_and_state_match_the_go_measurement_exactly() {
    // Go, measured (go-sqlite3 v1.14.52):
    //   /…/navidrome.db?cache=shared&_busy_timeout=15000&_journal_mode=WAL
    //     &_foreign_keys=on&synchronous=normal
    //   -> journal=wal busy=15000 sync=1 fk=1 file=/private/tmp/…/navidrome.db
    // and the SAME string minus `cache=shared` gives byte-identical state, because
    // go-sqlite3 strips the whole query for a non-`file:` DSN (sqlite3.go:1599-1601).
    let dir = temp_dir("default-dsn-exact");
    require_wal_support(&dir);

    // Resolved SQLite argument: a file: URI over the intended path carrying ONLY
    // cache=shared — no go-sqlite3 convention can leak into it.
    let dsn = go_dsn(&dir);
    let target = match parse_open_input(&dsn) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("the default DSN must parse: {cause}"),
    };
    assert_eq!(
        target.sqlite_arg,
        format!("file:{}?cache=shared", in_dir(&dir, "navidrome.db")),
        "exact SQLite argument"
    );
    assert_eq!(target.path, in_dir(&dir, "navidrome.db"));
    assert_eq!(
        target.options,
        DbOpenOptions::navidrome_default_options(),
        "Navidrome's preset"
    );
    assert!(!target.is_memory);
    // The preset's own fields, spelled out so a constructor change cannot silently
    // redefine "Navidrome's preset".
    assert_eq!(target.options.journal_mode, Some(JournalMode::Wal));
    assert_eq!(target.options.busy_timeout, NAVIDROME_BUSY_TIMEOUT_MS);
    assert_eq!(target.options.synchronous, Synchronous::Normal);
    assert_eq!(target.options.foreign_keys, Some(true));

    // Executed state and file name, on every handle of a real pool.
    let (pool, first) = build_pool(PoolConfig::with_size(go_dsn(&dir), 4), "default DSN pool");
    pool.put_back(first).expect("return configured handle");
    let wanted_file = canonical_of(&dir, "navidrome.db");
    every_handle(&pool, 4, |conn, index| {
        let state = state_of(conn, &format!("handle {index}"));
        assert_eq!(state.journal_mode, GO_DEFAULT_JOURNAL, "handle {index}");
        assert_eq!(state.busy_timeout, GO_DEFAULT_BUSY, "handle {index}");
        assert_eq!(state.synchronous, GO_DEFAULT_SYNC, "handle {index}");
        assert_eq!(state.foreign_keys, GO_DEFAULT_FK, "handle {index}");
        assert_eq!(
            state.db_file.as_deref(),
            Some(wanted_file.as_str()),
            "handle {index}: PRAGMA database_list must name the requested path"
        );
    });
}

#[test]
fn a_custom_bare_path_gets_the_go_driver_state_not_navidromes_preset() {
    // Go, measured: a bare path `/…/bare.db` -> journal=delete busy=5000 sync=1 fk=0,
    // because `journalMode` is empty and `foreignKeys` is -1 (`sqlite3.go:1234-1236`)
    // while `synchronousMode` starts at NORMAL (:1241). Navidrome's WAL/15000/FK-on
    // preset comes from DefaultDbPath, which configuration.go:404-406 only installs
    // when DbPath is empty — a custom DbPath must NOT inherit it.
    let dir = temp_dir("bare-path");
    require_wal_support(&dir);
    let path = in_dir(&dir, "custom.db");
    let (pool, first) = build_pool(PoolConfig::with_size(path.clone(), 3), "bare path");

    let want = DbOpenOptions::driver_defaults();
    assert_eq!(want.journal_mode, None, "journal mode is not named");
    assert_eq!(want.foreign_keys, None, "foreign keys are not set");
    assert_eq!(
        want.busy_timeout, DRIVER_BUSY_TIMEOUT_MS,
        "go-sqlite3's 5000"
    );
    assert_eq!(want.synchronous, Synchronous::Normal, "go-sqlite3's NORMAL");

    assert_handle_matches(&first, want, 0);
    let state = state_of(&first, "held-out handle");
    assert_eq!(
        state.journal_mode.to_lowercase(),
        "delete",
        "a fresh file that nobody configured stays on SQLite's default journal"
    );
    assert_eq!(state.busy_timeout, requested_ms(DRIVER_BUSY_TIMEOUT_MS));
    assert_eq!(state.synchronous, Synchronous::Normal.code());
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, 3, |conn, index| {
        assert_handle_matches(conn, want, index)
    });

    // No convention may reach SQLite as part of a filename: the directory holds only
    // the file the bare path names.
    let names = listing(&dir);
    assert_eq!(
        names,
        vec!["custom.db".to_string()],
        "a bare path must create exactly its own file"
    );
}

#[test]
fn dsn_shaped_input_creates_exactly_one_file_named_navidrome_db() {
    let dir = temp_dir("one-file");
    require_wal_support(&dir);
    let dsn = go_dsn(&dir);

    let (pool, first) = build_pool(PoolConfig::with_size(dsn, 4), "default DSN");
    // Migration + a write, so the DB is really used, not merely opened. Migration
    // runs on the handle Pool::new held out, outside the pool (DB-03 point 4).
    if let Err(cause) = apply_fresh_schema(&first) {
        panic!("fresh schema must apply on the held-out handle: {cause}");
    }
    if let Err(cause) = first.execute("INSERT INTO property(id, value) VALUES('db03','one')", []) {
        panic!("the migrated handle must accept a write: {cause}");
    }
    pool.put_back(first).expect("return configured handle");

    let names = listing(&dir);
    let with_query: Vec<&String> = names.iter().filter(|name| name.contains('?')).collect();
    assert!(
        with_query.is_empty(),
        "a DSN input created filename(s) carrying the query string: {with_query:?}"
    );
    // WAL's own -wal/-shm sidecars are legitimate; anything else named after the
    // DB (e.g. `navidrome.db?cache=shared`) is the bug.
    let database_files: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with("navidrome.db"))
        .cloned()
        .collect();
    let want: Vec<String> = vec![
        "navidrome.db".to_string(),
        "navidrome.db-shm".to_string(),
        "navidrome.db-wal".to_string(),
    ];
    assert_eq!(
        database_files, want,
        "exactly one database file plus its WAL sidecars; full listing: {names:?}"
    );
}

#[test]
fn unknown_and_invalid_parameters_fail_explicitly_and_open_nothing() {
    let dir = temp_dir("rejected");
    require_wal_support(&dir);
    let base = in_dir(&dir, "navidrome.db");

    // Rejected by this port's parser, before SQLite is ever asked. These are inputs
    // go-sqlite3 accepts and ignores (measured: `?cache`, `?_bogus=1`,
    // `?synchronous=maybe` all open); rejecting them is the documented fail-fast
    // divergence, so a handle can never silently hold a different setting than the
    // DSN appears to ask for.
    let rejected: Vec<(String, &str)> = vec![
        (format!("{base}?_bogus=1"), "_bogus"),
        (format!("{base}?_vacuum=full"), "_vacuum"),
        (format!("{base}?_mutex=no"), "_mutex"),
        (format!("{base}?_journal_mode=sidecar"), "_journal_mode"),
        (format!("{base}?_journal=sidecar"), "_journal"),
        (format!("{base}?_busy_timeout=abc"), "_busy_timeout"),
        (format!("{base}?_timeout=abc"), "_timeout"),
        (format!("{base}?_synchronous=maybe"), "_synchronous"),
        (format!("{base}?_sync=maybe"), "_sync"),
        (format!("{base}?_foreign_keys=maybe"), "_foreign_keys"),
        (format!("{base}?cache"), "no value"),
        (format!("{base}?cache="), "empty value"),
        (format!("{base}?=wal"), "empty parameter name"),
        (format!("{base}?%zz=1"), "invalid URL escape"),
        (format!("{base}?_journal_mode=%zz"), "invalid URL escape"),
        (format!("{base}?_busy_timeout=12%2"), "invalid URL escape"),
        (format!("file:{base}#tail"), "#"),
    ];
    for (input, needle) in rejected.clone() {
        let error = match open(&input) {
            Ok(conn) => {
                drop(conn);
                panic!("{input} must be rejected, but it opened a connection");
            }
            Err(cause) => cause,
        };
        match error {
            SchemaError::InvalidDsn {
                input: named,
                reason,
            } => {
                assert_eq!(named, input, "the error names the rejected input");
                assert!(
                    reason.contains(needle),
                    "reason must name the rejected part {needle}: {reason}"
                );
            }
            other => panic!("{input} must fail as InvalidDsn, got {other}"),
        }
    }

    // Rejected by SQLite itself, because the parameter is genuine but its value is
    // not: still an explicit failure with SQLite's own reason, never a silently
    // ignored parameter.
    //
    // `mode=ro` combined with a WAL request is the same shape: SQLite refuses the
    // journal switch on a read-only handle at open time, so the port reports
    // SQLite's own failure rather than handing out a handle that silently keeps
    // `delete`. Asserted in `read_back_mismatch_is_reported_as_open_configuration`.
    let sqlite_rejected: Vec<(String, &str)> = vec![
        (format!("file:{base}?mode=sidecar"), "no such access mode"),
        (format!("file:{base}?cache=sidecar"), "no such cache mode"),
    ];
    for (input, needle) in sqlite_rejected {
        match open(&input) {
            Ok(conn) => {
                drop(conn);
                panic!("{input} must be refused by SQLite, but it opened a connection");
            }
            Err(SchemaError::Failed { path, cause }) => {
                assert_eq!(path, input, "the failure names the input");
                assert!(
                    cause.to_string().contains(needle),
                    "SQLite's reason must surface: {cause}"
                );
            }
            Err(other) => panic!("{input} must fail as Failed, got {other}"),
        }
    }

    // Nothing was created for any rejected input: the WAL probe is removed and the
    // directory is still empty.
    let before = listing(&dir);
    assert!(
        before.is_empty(),
        "rejected inputs must create no file, directory holds: {before:?}"
    );
    let accepted = match open(&base) {
        Ok(conn) => conn,
        Err(cause) => panic!("a bare path must open: {cause}"),
    };
    drop(accepted);
    let after = listing(&dir);
    assert_eq!(
        after,
        vec!["navidrome.db".to_string()],
        "only the accepted path created a file"
    );
}

#[test]
fn go_ignored_bare_names_and_txlock_open_without_changing_any_setting() {
    // Measured in Go: `?journal_mode=wal`, `?journal=wal`, `?busy_timeout=9999`,
    // `?timeout=9999`, `?synchronous=full`, `?sync=full`, `?foreign_keys=on`,
    // `?fk=on` all leave the driver state at journal=delete busy=5000 sync=1 fk=0,
    // and `?_txlock=immediate` opens fine. SQLite reads `journal_mode`/`busy_timeout`
    // only as URI parameters of a `file:` URI, never as query parameters of a bare
    // filename, so accepting them as settings here would invent state Go never has.
    // They are accepted and IGNORED, exactly as Go ignores them.
    let dir = temp_dir("go-ignored-names");
    require_wal_support(&dir);
    for params in [
        "journal_mode=wal",
        "journal=wal",
        "busy_timeout=9999",
        "timeout=9999",
        "synchronous=full",
        "sync=full",
        "foreign_keys=on",
        "fk=on",
        "_txlock=immediate",
    ] {
        let path = format!("{}?{}", in_dir(&dir, "ignored.db"), params);
        let target = match parse_open_input(&path) {
            Ok(parsed) => parsed,
            Err(cause) => panic!("{params} must be accepted as Go ignores it: {cause}"),
        };
        assert_eq!(
            target.options,
            DbOpenOptions::driver_defaults(),
            "{params} must not change any setting"
        );
        assert_eq!(
            target.sqlite_arg,
            in_dir(&dir, "ignored.db"),
            "{params} must not reach SQLite at all"
        );
    }
    let (pool, first) = build_pool(
        PoolConfig::with_size(
            format!(
                "{}?journal_mode=wal&_txlock=immediate",
                in_dir(&dir, "ignored.db")
            ),
            2,
        ),
        "ignored names",
    );
    let want = DbOpenOptions::driver_defaults();
    assert_handle_matches(&first, want, 0);
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, 2, |conn, index| {
        assert_handle_matches(conn, want, index)
    });
    let state = state_of(pool.checkout().unwrap().conn(), "ignored-names handle");
    assert_ne!(
        state.journal_mode.to_lowercase(),
        "wal",
        "a bare journal_mode= name must not switch the journal: {}",
        state.journal_mode
    );
    assert_eq!(
        state.busy_timeout,
        requested_ms(DRIVER_BUSY_TIMEOUT_MS),
        "a bare busy_timeout= name must not change the timeout"
    );
}

#[test]
fn a_custom_busy_timeout_is_applied_on_every_handle_not_ignored() {
    let dir = temp_dir("custom-timeout");
    require_wal_support(&dir);

    // Route 1: the setting comes from the DSN.
    let dsn = format!(
        "{}?_journal_mode=delete&_busy_timeout=2500",
        in_dir(&dir, "dsn.db")
    );
    let want = DbOpenOptions {
        journal_mode: Some(JournalMode::Delete),
        busy_timeout: 2500,
        ..DbOpenOptions::driver_defaults()
    };
    let (from_dsn, first) = build_pool(PoolConfig::with_size(dsn, 3), "custom _busy_timeout");
    // G10: the DDL pass toggles `PRAGMA foreign_keys`, so it belongs on the handle
    // `Pool::new` held out, before it joins the idle queue.
    apply_fresh_schema(&first).unwrap_or_else(|cause| panic!("fresh schema on DSN pool: {cause}"));
    pool_check(&from_dsn, first, 3, want);

    // Route 2: explicit options, including a non-default synchronous and FK off.
    let explicit = DbOpenOptions {
        journal_mode: Some(JournalMode::Delete),
        busy_timeout: 7777,
        synchronous: Synchronous::Full,
        foreign_keys: Some(false),
        cache_size: None,
    };
    let config = PoolConfig::with_size(in_dir(&dir, "explicit.db"), 3).with_open_options(explicit);
    let (from_options, first) = build_pool(config, "explicit options");
    apply_fresh_schema(&first)
        .unwrap_or_else(|cause| panic!("fresh schema on options pool: {cause}"));
    pool_check(&from_options, first, 3, explicit);

    // The configured timeout — not rusqlite's 5000 — governs a real lock wait: one
    // handle holds a write transaction, another's write waits out 7777ms and then
    // reports a busy-class failure.
    let mut held = from_options
        .checkout()
        .unwrap_or_else(|cause| panic!("{cause}"));
    let mut blocked = from_options
        .checkout()
        .unwrap_or_else(|cause| panic!("{cause}"));
    assert_eq!(
        from_options.idle_count().unwrap_or_else(|cause| panic!("idle_count: {cause}")),
        1,
        "G10: the pool must hold all 3 connections after initialization, so two concurrent leases leave exactly one idle"
    );
    let insert = "INSERT INTO property(id, value) VALUES('db03-timeout', 'held')";
    let writer = match held.transaction() {
        Ok(tx) => tx,
        Err(cause) => panic!("first handle must open a transaction: {cause}"),
    };
    if let Err(cause) = writer.execute(insert, []) {
        panic!("first handle must write inside its transaction: {cause}");
    }
    let started = Instant::now();
    let refused = match blocked.transaction() {
        Ok(tx) => tx.execute(insert, []).map(|_rows| ()),
        Err(cause) => Err(cause),
    };
    let waited = started.elapsed();
    if let Err(cause) = writer.finish() {
        panic!("first handle must finish its transaction: {cause}");
    }

    match refused {
        Ok(()) => panic!("a second writer must be refused while the write lock is held"),
        Err(cause) => {
            assert!(is_busy(&cause), "refusal must be busy-class: {cause}");
            assert!(
                waited >= Duration::from_millis(7000),
                "the wait must last the configured 7777ms busy_timeout, not rusqlite's \
                 5000 install default (measured {waited:?})"
            );
        }
    }
}

/// Put the held-out handle back, then check every pooled lease against `want`.
fn pool_check(pool: &Pool, first: nd_db::pool::Initializer, want_size: usize, want: DbOpenOptions) {
    assert_handle_matches(&first, want, 0);
    pool.put_back(first).expect("return configured handle");
    // G10: handing the migration handle back must leave the pool at its FULL
    // configured size. Dropping it instead (the earlier fixture shape) silently left
    // `size - 1` connections, which every later lease-count assertion would hide.
    assert_eq!(
        pool.idle_count()
            .unwrap_or_else(|cause| panic!("idle_count: {cause}")),
        want_size,
        "the pool must hold config.size idle connections after initialization"
    );
    every_handle(pool, want_size, |conn, index| {
        assert_handle_matches(conn, want, index);
    });
}

#[test]
fn memory_db_still_works_and_is_not_forced_to_wal() {
    // `:memory:` deliberately shared: Go's rewrite uses `cache=shared`
    // (`db/db.go:54-55`), so cross-handle visibility is what is being proved here.
    // Every other test in this binary is file-backed and in its own directory.
    let (pool, first) = build_pool(PoolConfig::with_size(":memory:", 3), ":memory:");
    assert_eq!(pool.path(), ":memory:", "the configured input is kept");

    let want = DbOpenOptions {
        journal_mode: Some(JournalMode::Memory),
        ..DbOpenOptions::driver_defaults()
    };
    assert_handle_matches(&first, want, 0);
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, 3, |conn, index| {
        let got = state_of(conn, &format!("handle {index}"));
        assert_eq!(
            got.journal_mode.to_lowercase(),
            JournalMode::Memory.sqlite_name(),
            "handle {index}: a memory DB keeps the memory journal; WAL is not forced"
        );
        assert_eq!(
            got.busy_timeout,
            requested_ms(DRIVER_BUSY_TIMEOUT_MS),
            "handle {index}: busy_timeout"
        );
        assert_eq!(
            got.synchronous,
            Synchronous::Normal.code(),
            "handle {index}: synchronous"
        );
        assert!(
            got.foreign_keys,
            "handle {index}: Go's memory DSN sets FK on"
        );
        assert_eq!(
            got.db_file, None,
            "handle {index}: a memory DB names no file"
        );
    });

    // Usable, and shared across handles. Both leases are held at the same time and
    // the writer is dropped only AFTER the reader has read, so a LIFO reuse of the
    // writing handle cannot make this pass vacuously.
    let create = "CREATE TABLE IF NOT EXISTS db03_memory_probe (id TEXT PRIMARY KEY, value TEXT)";
    let insert = "INSERT INTO db03_memory_probe(id, value) VALUES('k', 'v')";
    let writer = pool.checkout().unwrap_or_else(|cause| panic!("{cause}"));
    let reader = pool.checkout().unwrap_or_else(|cause| panic!("{cause}"));
    assert_eq!(
        pool.idle_count().unwrap(),
        1,
        "writer and reader are two different live handles"
    );
    assert_ne!(
        writer.conn() as *const rusqlite::Connection,
        reader.conn() as *const rusqlite::Connection,
        "the two leases must lend distinct handles"
    );
    if let Err(cause) = writer.conn().execute(create, []) {
        panic!("memory db must accept DDL: {cause}");
    }
    if let Err(cause) = writer.conn().execute(insert, []) {
        panic!("memory db must accept a write: {cause}");
    }
    let read = reader.conn().query_row(
        "SELECT value FROM db03_memory_probe WHERE id = 'k'",
        [],
        |row| row.get::<_, String>(0),
    );
    match read {
        Ok(value) => assert_eq!(
            value, "v",
            "shared-cache memory DB is visible across handles"
        ),
        Err(cause) => panic!("the write must be readable from another handle: {cause}"),
    }
    drop(writer);
}

#[test]
fn migration_then_pooling_leaves_foreign_keys_on_for_every_handle() {
    let dir = temp_dir("migration-fk");
    require_wal_support(&dir);
    let dsn = go_dsn(&dir);
    let want_size: usize = 4;

    // `Pool::new` hands the freshly-opened handle to the caller precisely so
    // migration runs on a connection the pool does not own: a failed
    // `PRAGMA foreign_keys` restore cannot then poison a pooled lease.
    let (pool, migrating) = build_pool(PoolConfig::with_size(dsn, want_size), "default DSN");
    let before = state_of(&migrating, "held-out handle before migration");
    assert!(before.foreign_keys, "the default DSN asks FK on");

    // Go disables FK around migrations (`db/db.go:85-91`). Capture the state DURING
    // the DDL pass through a callback that runs mid-migration, to prove the toggle
    // really happens rather than assuming it.
    let mid_fk = std::cell::Cell::new(None);
    if let Err(cause) = apply_fresh_schema_with_probe(&migrating, &mid_fk) {
        panic!("fresh schema must apply on the held-out handle: {cause}");
    }
    assert_eq!(
        mid_fk.get(),
        Some(false),
        "the DDL pass must run with foreign_keys off (Go's db/db.go:85-91)"
    );
    let after = state_of(&migrating, "held-out handle after migration");
    assert!(
        after.foreign_keys,
        "the handle came back from migration with foreign_keys off"
    );
    pool.put_back(migrating).expect("return configured handle");

    let want = DbOpenOptions::navidrome_default_options();
    every_handle(&pool, want_size, |conn, index| {
        let state = state_of(conn, &format!("handle {index}"));
        assert!(
            state.foreign_keys,
            "handle {index} came back from migration with foreign_keys off"
        );
        assert_eq!(state.journal_mode.to_lowercase(), "wal", "handle {index}");
        assert_eq!(
            state.busy_timeout,
            requested_ms(NAVIDROME_BUSY_TIMEOUT_MS),
            "handle {index}"
        );
        assert_eq!(
            state.synchronous,
            Synchronous::Normal.code(),
            "handle {index}"
        );
        assert_handle_matches(conn, want, index);
    });

    // Non-vacuity: FK off is a real, readable state, so the `true` above is the
    // pragma working and not merely this build's compile-time default
    // (`libsqlite3-sys` 0.30.1 build.rs:123 sets SQLITE_DEFAULT_FOREIGN_KEYS=1).
    let (control, first) = build_pool(
        PoolConfig::with_size(in_dir(&dir, "fk-off.db"), 2).with_open_options(DbOpenOptions {
            foreign_keys: Some(false),
            ..DbOpenOptions::driver_defaults()
        }),
        "FK-off control",
    );
    pool_check(
        &control,
        first,
        2,
        DbOpenOptions {
            foreign_keys: Some(false),
            ..DbOpenOptions::driver_defaults()
        },
    );
    let handle = control.checkout().unwrap_or_else(|cause| panic!("{cause}"));
    if let Err(cause) = set_foreign_keys(handle.conn(), true, "db03 control restore") {
        panic!("FK must be restorable on the control handle: {cause}");
    }
    assert!(
        state_of(handle.conn(), "control handle").foreign_keys,
        "FK restore must read back on"
    );
}

/// Run the PRODUCTION migration entry point and sample `PRAGMA foreign_keys`
/// while its DDL is running, through `apply_fresh_schema_with`'s probe. The probe is
/// the only way to observe the toggle: rusqlite's authorizer needs its `hooks`
/// feature (not enabled here) and the DDL never touches the pragma itself, so
/// sampling after the fact would only show the restore.
fn apply_fresh_schema_with_probe(
    conn: &rusqlite::Connection,
    mid_fk: &std::cell::Cell<Option<bool>>,
) -> Result<(), nd_dbschema::schema::Error> {
    nd_dbschema::schema::apply_fresh_schema_with(
        conn,
        Some(&mut |handle: &rusqlite::Connection| {
            if mid_fk.get().is_none() {
                mid_fk.set(Some(effective_state(handle).unwrap().foreign_keys));
            }
        }),
    )
}

#[test]
fn migration_restores_the_handles_own_fk_state_not_a_hardcoded_on() {
    // Go's `defer` always execs `PRAGMA foreign_keys=on` (`db/db.go:85-91`), because
    // Go's DSN always asked for FK on. This port restores the state the handle was
    // configured with instead, so a caller that deliberately requested FK off keeps
    // it off after migration rather than silently getting enforcement back. That is
    // the difference between `want` being the captured pragma and being `true`.
    let dir = temp_dir("migration-fk-off");
    require_wal_support(&dir);
    let dsn = format!(
        "{}?_foreign_keys=off&_journal_mode=WAL",
        in_dir(&dir, "fkoff.db")
    );

    let (pool, migrating) = build_pool(PoolConfig::with_size(dsn, 2), "FK off DSN");
    let before = state_of(&migrating, "handle before migration");
    assert!(!before.foreign_keys, "the DSN asked for FK off");
    if let Err(cause) = apply_fresh_schema(&migrating) {
        panic!("fresh schema must apply on the held-out handle: {cause}");
    }
    let after = state_of(&migrating, "handle after migration");
    assert!(
        !after.foreign_keys,
        "migration must restore the configured state, not force foreign_keys on"
    );
    pool.put_back(migrating).expect("return configured handle");
    every_handle(&pool, 2, |conn, index| {
        assert!(
            !state_of(conn, &format!("handle {index}")).foreign_keys,
            "handle {index}: FK must stay off as configured"
        );
    });

    // The same handle asked for WAL, and migration must not have disturbed it.
    assert_eq!(
        state_of(pool.checkout().unwrap().conn(), "wal check after migration").journal_mode,
        "wal",
        "the journal setting survives the DDL pass"
    );
}

#[test]
fn explicit_options_override_the_dsn_and_unset_fields_emit_no_pragma() {
    // `PoolConfig::with_open_options` is the override path. An unset
    // journal_mode/foreign_keys must emit no pragma at all — the state then comes
    // from the file or from SQLite's compiled default, exactly like go-sqlite3's
    // conditional execs (`sqlite3.go:1851-1853`, `:1865-1867`).
    let dir = temp_dir("explicit-options");
    require_wal_support(&dir);
    let wal_path = in_dir(&dir, "wal.db");

    // First create a WAL file at that very path (the default parameter set, which
    // asks for WAL).
    let created_dsn = format!("{wal_path}?_journal_mode=WAL");
    let (created, creator) = build_pool(PoolConfig::with_size(created_dsn, 1), "create WAL");
    assert_eq!(state_of(&creator, "creator").journal_mode, "wal");
    created.put_back(creator).expect("return configured handle");
    every_handle(&created, 1, |conn, _index| {
        assert_eq!(state_of(conn, "creator handle").journal_mode, "wal");
    });
    drop(created);

    // Re-open it with journal_mode: None: no pragma, so WAL survives.
    let (pool, first) = build_pool(
        PoolConfig::with_size(wal_path.clone(), 2).with_open_options(DbOpenOptions {
            journal_mode: None,
            busy_timeout: 1234,
            synchronous: Synchronous::Extra,
            foreign_keys: None,
            cache_size: None,
        }),
        "unset journal",
    );
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, 2, |conn, index| {
        let got = state_of(conn, &format!("handle {index}"));
        assert_eq!(
            got.journal_mode, "wal",
            "handle {index}: an unnamed journal must not overwrite the file's WAL"
        );
        assert_eq!(
            got.busy_timeout, 1234,
            "handle {index}: explicit busy_timeout"
        );
        assert_eq!(
            got.synchronous,
            Synchronous::Extra.code(),
            "handle {index}: explicit synchronous"
        );
    });

    // Release the readers first: SQLite cannot change the journal of a database
    // other handles still have open — demoting WAL while the pool above is alive
    // fails with `database is locked` (measured), so the pool is dropped here.
    drop(pool);

    // Explicit values win over the DSN's own conventions, and an explicit
    // journal_mode=delete really demotes a WAL file (measured on this build).
    let (explicit_pool, first) = build_pool(
        PoolConfig::with_size(wal_path, 1).with_open_options(DbOpenOptions {
            journal_mode: Some(JournalMode::Delete),
            busy_timeout: 777,
            synchronous: Synchronous::Off,
            foreign_keys: Some(false),
            cache_size: None,
        }),
        "override DSN",
    );
    let got = state_of(&first, "overridden handle");
    assert_eq!(
        got.journal_mode, "delete",
        "explicit journal_mode beats the file's WAL"
    );
    assert_eq!(
        got.busy_timeout, 777,
        "explicit busy_timeout beats the DSN's 15000"
    );
    assert_eq!(got.synchronous, Synchronous::Off.code());
    assert!(
        !got.foreign_keys,
        "explicit FK off beats the DSN's _foreign_keys=on"
    );
    explicit_pool
        .put_back(first)
        .expect("return configured handle");
}

#[test]
fn a_handle_that_cannot_hold_the_settings_fails_pool_open() {
    let dir = temp_dir("setup-failure");
    require_wal_support(&dir);
    let existing = in_dir(&dir, "readonly.db");
    // Created WITHOUT WAL, so a read-only reopen genuinely has to switch journal
    // modes — on an already-WAL file `journal_mode=wal` is a no-op and would pass.
    match open_with_options(
        &existing,
        Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Delete),
            ..DbOpenOptions::driver_defaults()
        }),
    ) {
        Ok(conn) => drop(conn),
        Err(cause) => panic!("must create the db the read-only case reopens: {cause}"),
    }
    let created = listing(&dir);
    assert_eq!(
        state_of(&open(&existing).unwrap(), "existing file").journal_mode,
        "delete",
        "the control file really starts on the delete journal"
    );

    // `mode=ro` is a genuine SQLite URI parameter that survives translation, and
    // SQLite refuses `journal_mode=wal` on it. The pool must report that instead of
    // handing out a handle that silently keeps `delete`.
    let config = PoolConfig::with_size(format!("file:{existing}?mode=ro"), 2).with_open_options(
        DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            ..DbOpenOptions::navidrome_default_options()
        },
    );
    match Pool::new(&config) {
        Ok(_pool) => panic!("a read-only handle cannot hold journal_mode=wal"),
        Err(cause) => {
            let text = cause.to_string();
            assert!(
                text.contains("readonly") || text.contains("wal"),
                "the open failure must name the configuration problem, SQLite's own \
                 refusal included: {text}"
            );
        }
    }
    assert_eq!(
        listing(&dir),
        created,
        "a failed pool open must not leave extra files behind"
    );
}

#[test]
fn a_requested_journal_that_reads_back_as_memory_is_reported() {
    // The memory exception is narrow: only a WAL request on a memory DB is accepted
    // (SQLite keeps `memory`, `sqlite3.c:64434-64436`). A DELETE/TRUNCATE/PERSIST
    // request that reads back `memory` must be a hard OpenConfiguration failure.
    // SQLite allows MEMORY and OFF on an in-memory DB (`sqlite3.c:64459-64464`), so
    // only the file journals can genuinely read back `memory`.
    let wanted = ["delete", "truncate", "persist"];
    let dir = temp_dir("journal-reads-back-memory");
    for journal in wanted {
        let uri = memory_uri(
            &dir,
            &format!("db03_mem_{journal}"),
            &format!("&_journal_mode={journal}"),
        );
        match open(&uri) {
            Ok(conn) => {
                let got = state_of(&conn, &uri);
                panic!(
                    "{journal} on a memory DB reads back {} and must be reported as a \
                     mismatch, not accepted: {got:?}",
                    got.journal_mode
                );
            }
            Err(SchemaError::OpenConfiguration { path, reason }) => {
                assert_eq!(path, uri, "the error names the input");
                assert!(
                    reason.contains("journal_mode") && reason.contains(journal),
                    "the reason must name the requested journal {journal}: {reason}"
                );
                assert!(
                    reason.contains("memory"),
                    "the reason must name what SQLite reported: {reason}"
                );
            }
            Err(other) => panic!("{uri} must fail as OpenConfiguration, got {other}"),
        }
    }
    // WAL, by contrast, is accepted on a memory DB and reports the journal SQLite
    // actually keeps.
    let wal = open(&memory_uri(&dir, "db03_mem_wal", "&_journal_mode=wal"));
    match wal {
        Ok(conn) => assert_eq!(
            state_of(&conn, "memory WAL").journal_mode,
            JournalMode::Memory.sqlite_name(),
            "SQLite keeps the memory journal for a WAL request"
        ),
        Err(cause) => panic!(
            "SQLite answers a WAL request on a memory DB with `memory` and no error: {cause}"
        ),
    }
}

#[test]
fn busy_timeout_zero_and_negative_are_applied_as_sqlite_clamps_them() {
    // Go: `?_busy_timeout=-5` opens and the handle reports busy_timeout 0, because
    // the negative value is handed to SQLite, which clamps it (measured). 0 means
    // "do not wait at all", so a busy refusal must come back immediately.
    let dir = temp_dir("busy-zero");
    require_wal_support(&dir);
    for (params, want_ms) in [("_busy_timeout=0", 0i64), ("_busy_timeout=-5", 0i64)] {
        let dsn = format!("{}?{}", in_dir(&dir, "zero.db"), params);
        let (pool, first) = build_pool(PoolConfig::with_size(dsn, 2), params);
        let state = state_of(&first, params);
        assert_eq!(state.busy_timeout, want_ms, "{params}: busy_timeout");
        pool.put_back(first).expect("return configured handle");
        every_handle(&pool, 2, |conn, index| {
            assert_eq!(
                state_of(conn, &format!("handle {index}")).busy_timeout,
                want_ms,
                "{params}: handle {index} busy_timeout"
            );
        });
    }
}

#[test]
fn dsn_translation_maps_settings_and_keeps_genuine_uri_parameters() {
    // The reference input: nd-conf's DEFAULT_DB_PATH must parse to `navidrome.db`
    // plus exactly Navidrome's preset.
    let target = match parse_open_input(DEFAULT_DB_PATH) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("Go's default DSN must parse: {cause}"),
    };
    assert_eq!(target.path, "navidrome.db", "path component only");
    assert_eq!(target.options, DbOpenOptions::navidrome_default_options());
    assert!(!target.is_memory);
    assert_eq!(
        target.sqlite_arg, "file:navidrome.db?cache=shared",
        "the go-sqlite3 conventions leave the argument; cache=shared is SQLite's own"
    );

    // A bare path gets go-sqlite3's driver state, NOT Navidrome's preset.
    let bare = match parse_open_input("/var/lib/navidrome/navidrome.db") {
        Ok(parsed) => parsed,
        Err(cause) => panic!("a bare path must parse: {cause}"),
    };
    assert_eq!(bare.sqlite_arg, "/var/lib/navidrome/navidrome.db");
    assert_eq!(bare.options, DbOpenOptions::driver_defaults());

    // A custom path that happens to be named navidrome.db but carries no query is
    // still not the default DSN.
    let custom = match parse_open_input("/data/navidrome.db") {
        Ok(parsed) => parsed,
        Err(cause) => panic!("a custom path must parse: {cause}"),
    };
    assert_eq!(custom.options, DbOpenOptions::driver_defaults());

    // `:memory:` keeps Go's shared-cache rewrite, minus the driver convention.
    let memory = match parse_open_input(":memory:") {
        Ok(parsed) => parsed,
        Err(cause) => panic!(":memory: must parse: {cause}"),
    };
    assert_eq!(memory.sqlite_arg, "file::memory:?cache=shared");
    assert!(memory.is_memory);
    assert_eq!(
        memory.options,
        DbOpenOptions {
            journal_mode: Some(JournalMode::Memory),
            foreign_keys: Some(true),
            ..DbOpenOptions::driver_defaults()
        }
    );
    assert_eq!(
        memory.options.foreign_keys,
        Some(true),
        "Go's memory rewrite sets FK"
    );

    // Go's own memory DSN string parses to the same thing.
    let memory_dsn = match parse_open_input(MEMORY_DSN) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("Go's memory DSN must parse: {cause}"),
    };
    assert_eq!(memory_dsn.sqlite_arg, MEMORY_DSN);
    assert_eq!(memory_dsn.options, memory.options);

    // TWO grammars: Go conventions keep the first value, but SQLite receives
    // every URI occurrence verbatim and applies cache/mode/vfs progressively.
    let uri_repeat = "file:repeated.db?cache=shared&cache=private&_timeout=111&_timeout=222";
    let repeat = match parse_open_input(uri_repeat) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("a repeated parameter must parse: {cause}"),
    };
    assert_eq!(
        repeat.sqlite_arg, uri_repeat,
        "the ordered URI query is untouched"
    );
    assert_eq!(
        repeat.options.busy_timeout, 111,
        "the first alias value wins"
    );

    // (b) the same parameters on a NON-URI input: go-sqlite3 strips the whole query
    // (`sqlite3.go:1599-1601`), so SQLite receives no URI parameter at all and only
    // the conventions become options.
    let plain_repeat = "repeated.db?cache=shared&cache=private&_timeout=111&_timeout=222";
    let stripped = match parse_open_input(plain_repeat) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("a repeated parameter must parse: {cause}"),
    };
    assert_eq!(
        stripped.sqlite_arg, "repeated.db",
        "a non-URI input reaches SQLite with no parameters, exactly like Go's stripped DSN"
    );
    assert_eq!(stripped.options.busy_timeout, 111);

    // go-sqlite3 aliases.
    let aliases = match parse_open_input("x.db?_journal=PERSIST&_timeout=8000&_sync=2&_fk=0") {
        Ok(parsed) => parsed,
        Err(cause) => panic!("go-sqlite3 aliases must parse: {cause}"),
    };
    assert_eq!(
        aliases.options,
        DbOpenOptions {
            journal_mode: Some(JournalMode::Persist),
            busy_timeout: 8000,
            synchronous: Synchronous::Full,
            foreign_keys: Some(false),
            cache_size: None,
        }
    );

    // A `mode=memory` URI is a memory DB even when it asks for WAL.
    let uri_memory =
        match parse_open_input("file:scratch?mode=memory&cache=shared&_journal_mode=WAL") {
            Ok(parsed) => parsed,
            Err(cause) => panic!("a mode=memory URI must parse: {cause}"),
        };
    assert!(uri_memory.is_memory);
    assert_eq!(uri_memory.options.journal_mode, Some(JournalMode::Wal));
    assert_eq!(
        uri_memory.sqlite_arg,
        "file:scratch?mode=memory&cache=shared&_journal_mode=WAL"
    );
}

#[test]
fn a_file_uri_keeps_its_own_query_and_cache_shared_reaches_sqlite() {
    let dir = temp_dir("file-uri-cache");
    require_wal_support(&dir);
    let uri = format!(
        "file:{}?cache=shared&_journal_mode=wal&_busy_timeout=1234",
        in_dir(&dir, "shared.db")
    );
    let target = match parse_open_input(&uri) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("{uri}: {cause}"),
    };
    assert_eq!(
        target.sqlite_arg, uri,
        "a file: URI keeps the entire original query, including conventions"
    );
    assert_eq!(target.path, in_dir(&dir, "shared.db"));
    assert_eq!(target.options.journal_mode, Some(JournalMode::Wal));
    assert_eq!(target.options.busy_timeout, 1234);

    let (pool, first) = build_pool(PoolConfig::with_size(uri, 2), "file: URI with cache=shared");
    pool_check(
        &pool,
        first,
        2,
        DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            busy_timeout: 1234,
            ..DbOpenOptions::driver_defaults()
        },
    );
}

#[test]
fn a_shared_cache_exposes_sqlite_locked_sharedcache_between_handles() {
    // Measured with this build: two handles on `file:…?cache=shared` writing the
    // same table get `SQLITE_LOCKED_SHAREDCACHE` ("database table is locked",
    // extended 262) immediately, with no busy_timeout wait at all, while two handles
    // WITHOUT the shared cache wait out the timeout and report SQLITE_BUSY
    // (extended 5). This is the tradeoff of forwarding cache=shared, stated in
    // `schema.rs` and the D-OPEN decision record; the port pins it rather than
    // pretending the parameter is inert.
    let dir = temp_dir("shared-cache-lock");
    require_wal_support(&dir);
    let create = "CREATE TABLE db03_shared_lock (k TEXT PRIMARY KEY, v TEXT)";

    let shared = format!("file:{}?cache=shared", in_dir(&dir, "shared.db"));
    let unshared = in_dir(&dir, "unshared.db");
    for (label, input) in [
        ("cache=shared", shared.as_str()),
        ("no cache", unshared.as_str()),
    ] {
        let (pool, first) = build_pool(PoolConfig::with_size(input.to_string(), 2), label);
        pool.put_back(first).expect("return configured handle");
        let mut a = pool.checkout().unwrap_or_else(|cause| panic!("{cause}"));
        let mut b = pool.checkout().unwrap_or_else(|cause| panic!("{cause}"));
        if let Err(cause) = a.conn().execute(create, []) {
            panic!("{label}: DDL must apply: {cause}");
        }
        let tx_a = match a.transaction() {
            Ok(tx) => tx,
            Err(cause) => panic!("{label}: transaction: {cause}"),
        };
        if let Err(cause) = tx_a.execute("INSERT INTO db03_shared_lock(k, v) VALUES('k', 'a')", [])
        {
            panic!("{label}: insert: {cause}");
        }
        let started = Instant::now();
        let refused: Result<(), rusqlite::Error> = b
            .transaction()
            .and_then(|tx| {
                tx.execute("INSERT INTO db03_shared_lock(k, v) VALUES('k', 'b')", [])
                    .map(|_| ())
            })
            .map(|_| ());
        let waited = started.elapsed();
        drop(tx_a);
        let code = match &refused {
            Ok(()) => panic!("{label}: a second writer must be refused"),
            Err(rusqlite::Error::SqliteFailure(failure, _)) => failure.extended_code,
            Err(other) => panic!("{label}: expected an SQLite failure, got {other}"),
        };
        if label == "cache=shared" {
            assert!(
                !is_busy(refused.as_ref().unwrap_err()),
                "LOCKED is not retryable BUSY"
            );
        } else {
            assert!(
                is_busy(refused.as_ref().unwrap_err()),
                "BUSY must be retryable"
            );
        }
        if label == "cache=shared" {
            assert_eq!(
                code,
                rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE,
                "{label} must report the shared-cache table lock ({label}): {refused:?}"
            );
            assert!(
                waited < Duration::from_millis(1000),
                "{label} refuses immediately, without waiting out the timeout ({waited:?})"
            );
        } else {
            assert_eq!(code, rusqlite::ffi::SQLITE_BUSY, "{label}: {refused:?}");
            assert!(
                waited >= Duration::from_millis(4000),
                "{label} waits out the 5000ms driver default timeout ({waited:?})"
            );
        }
    }
}

#[test]
fn metacharacter_paths_open_the_intended_file_and_re_emitted_uris_are_encoded() {
    // Two distinct behaviors, both measured on this build:
    //   * a NON-URI input is handed to SQLite verbatim, and SQLite does not URI-parse
    //     it, so `%20`, `#`, `&`, `:` and spaces keep their literal meaning —
    //     `…/hash#name.db` really opens `…/hash#name.db`.
    //   * once a `file:` URI has to be re-emitted (Navidrome's default DSN forwards
    //     `cache=shared`, and any memory DB needs the URI form), the path MUST be
    //     percent-encoded, or SQLite's decoder lands elsewhere. Measured:
    //       file:…/%20lit.db   -> …/ lit.db      (decoded!)
    //       file:…/pct%2520lit -> …/pct%20lit    (correct way to name a literal %20)
    //       file:…/hash#name   -> …/hash         (fragment truncation)
    let dir = temp_dir("metacharacter-paths");
    require_wal_support(&dir);
    for name in [
        "plain.db",
        "%20literal.db",
        "with space.db",
        "hash#name.db",
        "amp&sem.db",
        "colon:name.db",
    ] {
        let path = in_dir(&dir, name);
        let target = match parse_open_input(&path) {
            Ok(parsed) => parsed,
            Err(cause) => panic!("{name}: {cause}"),
        };
        assert_eq!(
            target.sqlite_arg, path,
            "{name}: a non-URI input with no parameters is handed to SQLite verbatim"
        );
        assert_eq!(
            target.options,
            DbOpenOptions::driver_defaults(),
            "{name}: no parameter, so driver state"
        );
        let conn = match open(&path) {
            Ok(conn) => conn,
            Err(cause) => panic!("{name} must open the intended file: {cause}"),
        };
        assert_eq!(
            state_of(&conn, name).db_file.as_deref(),
            Some(canonical_of(&dir, name).as_str()),
            "{name}: PRAGMA database_list must name the intended file"
        );
        drop(conn);
    }
    // Exactly one file per requested name: nothing truncated at '#', nothing decoded.
    let names = listing(&dir);
    let want: Vec<String> = vec![
        "%20literal.db".to_string(),
        "amp&sem.db".to_string(),
        "colon:name.db".to_string(),
        "hash#name.db".to_string(),
        "plain.db".to_string(),
        "with space.db".to_string(),
    ];
    assert_eq!(names, want, "each path created exactly its own file");

    // Re-emission case: a directory whose name carries a space and a literal `%20`
    // opened through the default DSN (which forwards cache=shared, so it must become
    // a URI). The file SQLite names is still the intended one.
    let tricky = PathBuf::from(TEMP_ROOT).join("uri re%20mission");
    match fs::remove_dir_all(&tricky) {
        Ok(()) => (),
        Err(_absent) => (),
    }
    fs::create_dir_all(&tricky)
        .unwrap_or_else(|cause| panic!("create {}: {cause}", tricky.display()));
    let dsn = go_dsn(&tricky);
    let target = match parse_open_input(&dsn) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("{dsn}: {cause}"),
    };
    assert!(
        target.sqlite_arg.starts_with("file:"),
        "forwarding cache=shared requires a file: URI: {}",
        target.sqlite_arg
    );
    assert!(
        target.sqlite_arg.contains("%20") && target.sqlite_arg.contains("%25"),
        "the path must be percent-encoded so SQLite decodes it back: {}",
        target.sqlite_arg
    );
    let (pool, first) = build_pool(PoolConfig::with_size(dsn, 2), "tricky directory");
    pool.put_back(first).expect("return configured handle");
    let wanted = canonical_of(&tricky, "navidrome.db");
    every_handle(&pool, 2, |conn, index| {
        assert_eq!(
            state_of(conn, &format!("handle {index}"))
                .db_file
                .as_deref(),
            Some(wanted.as_str()),
            "handle {index}: an encoded path must still name navidrome.db"
        );
    });

    // A caller-supplied `file:` URI is kept verbatim (re-encoding would double-decode).
    let already = format!("file:{}?cache=shared", in_dir(&dir, "with%20space.db"));
    let target = match parse_open_input(&already) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("{already}: {cause}"),
    };
    assert_eq!(target.sqlite_arg, already, "a file: URI is kept verbatim");
    assert_eq!(target.path, in_dir(&dir, "with%20space.db"));
    let conn = open(&already).unwrap_or_else(|cause| panic!("{already}: {cause}"));
    assert_eq!(
        state_of(&conn, "encoded uri").db_file.as_deref(),
        Some(canonical_of(&dir, "with space.db").as_str()),
        "a caller-encoded URI must decode to the intended file"
    );
    drop(conn);

    // '#' inside a `file:` URI is rejected, because SQLite truncates the name there.
    match open(&format!("file:{}#tail", in_dir(&dir, "plain.db"))) {
        Ok(conn) => {
            let got = state_of(&conn, "fragment uri");
            panic!(
                "a '#' in a file: URI must be rejected, it opened {:?}",
                got.db_file
            );
        }
        Err(SchemaError::InvalidDsn { reason, .. }) => assert!(
            reason.contains('#'),
            "the reason must name the '#': {reason}"
        ),
        Err(other) => panic!("must be InvalidDsn, got {other}"),
    }
}

#[test]
fn alias_and_repeat_matrix_matches_the_measured_go_values() {
    // Every row below is the state go-sqlite3 v1.14.52 reports for that query,
    // reproduced by:
    //   cd /tmp/gosql2 && go build -o gopragma2 . && ./gopragma2 /tmp/x/caseN.db?QUERY
    // (sql.Open + SELECT 1 + `PRAGMA journal_mode|busy_timeout|synchronous|foreign_keys`).
    // Rust must agree, on executed SQLite state, for each of them.
    let dir = temp_dir("alias-matrix");
    require_wal_support(&dir);

    // (query, journal, busy, sync, fk) exactly as Go reported them. `None` for fk
    // means "the DSN named nothing": go-sqlite3 then execs no `PRAGMA foreign_keys`
    // and Go's SQLite reports 0, while THIS build's SQLite is compiled with
    // `-DSQLITE_DEFAULT_FOREIGN_KEYS=1` (libsqlite3-sys-0.30.1/build.rs:123) and
    // reports 1. That is a build difference, not a parsing one: the port leaves the
    // compiled default untouched, so the fk column is only compared where the DSN
    // named it. `None` for journal means "not named": the fresh file stays on
    // SQLite's own `delete`, never WAL.
    /// (query, effective journal, effective busy, effective sync, effective FK).
    /// `None` journal = "DSN named nothing", so a fresh file stays on SQLite's own
    /// `delete`; `None` FK = "named nothing", where this build's compiled
    /// `SQLITE_DEFAULT_FOREIGN_KEYS=1` differs from Go's 0 (a build difference, not a
    /// parsing one), so that column is only compared where the DSN named it.
    type Row = (&'static str, Option<&'static str>, i64, i64, Option<bool>);
    let matrix: &[Row] = &[
        // alias wins over canonical wherever it appears, whatever the order, and its
        // FIRST value is used (`sqlite3.go:1338-1345` and friends).
        ("_busy_timeout=111&_timeout=222", None, 222, 1, None),
        ("_timeout=222&_busy_timeout=111", None, 222, 1, None),
        ("_journal_mode=wal&_journal=delete", Some("delete"), 5000, 1, None),
        ("_journal=delete&_journal_mode=wal", Some("delete"), 5000, 1, None),
        ("_synchronous=FULL&_sync=OFF", None, 5000, 0, None),
        ("_foreign_keys=on&_fk=off", None, 5000, 1, None),
        ("_foreign_keys=off&_fk=on", None, 5000, 1, Some(true)),
        // an alias that is PRESENT but EMPTY leaves the option unset.
        ("_journal_mode=wal&_journal=", None, 5000, 1, None),
        ("_busy_timeout=111&_timeout=", None, 5000, 1, None),
        ("_foreign_keys=on&_fk=", None, 5000, 1, None),
        ("_synchronous=FULL&_sync=", None, 5000, 1, None),
        // first occurrence of the winning name wins over later repeats.
        ("_journal_mode=wal&_journal_mode=delete", Some("wal"), 5000, 1, None),
        ("_timeout=333&_timeout=444", None, 333, 1, None),
        // alias-only spellings.
        ("_journal=wal", Some("wal"), 5000, 1, None),
        ("_timeout=333", None, 333, 1, None),
        ("_fk=on", None, 5000, 1, Some(true)),
        ("_sync=EXTRA", None, 5000, 3, None),
        ("_sync=FULL", None, 5000, 2, None),
        // bare SQLite names are ignored by the driver, whatever their order, and
        // lose to an underscore name when both appear.
        ("journal_mode=wal", None, 5000, 1, None),
        ("busy_timeout=9999", None, 5000, 1, None),
        ("synchronous=full", None, 5000, 1, None),
        ("foreign_keys=on", None, 5000, 1, None),
        ("_journal_mode=&journal_mode=wal", None, 5000, 1, None),
        ("_busy_timeout=&busy_timeout=9999", None, 5000, 1, None),
        ("_journal_mode=wal&journal_mode=delete", Some("wal"), 5000, 1, None),
        ("_busy_timeout=111&busy_timeout=9999", None, 111, 1, None),
        ("_timeout=5&busy_timeout=9999", None, 5, 1, None),
        ("journal_mode=wal&_journal_mode=delete", Some("delete"), 5000, 1, None),
        // percent-decoded names and values are matched after decoding.
        ("_journal%5Fmode=WAL", Some("wal"), 5000, 1, None),
        ("%5Fjournal_mode=WAL", Some("wal"), 5000, 1, None),
        ("_journal_mode=%77al", Some("wal"), 5000, 1, None),
        // `_%5Ftimeout` decodes to `__timeout`, an unknown name: Go ignores it
        // (busy stays 5000), the port rejects it as the fail-fast divergence.
        ("_%5Ftimeout=333", None, 5000, 1, None),
        // WAL with an explicit synchronous: go-sqlite3's WAL branch is a no-op.
        ("_journal_mode=wal&_synchronous=FULL", Some("wal"), 5000, 2, None),
        ("_journal_mode=wal&_sync=3", Some("wal"), 5000, 3, None),
        ("_journal_mode=wal&_sync=0", Some("wal"), 5000, 0, None),
        ("_journal_mode=wal", Some("wal"), 5000, 1, None),
        // Navidrome's default parameter set on a custom directory.
        (
            "cache=shared&_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal",
            Some("wal"),
            15000,
            1,
            Some(true),
        ),
        // the same minus cache=shared: identical state (Go strips the query anyway).
        (
            "_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal",
            Some("wal"),
            15000,
            1,
            Some(true),
        ),
    ];
    // Rows whose decoded name is unknown are asserted as InvalidDsn: Go ignores
    // them, the port refuses to hand out a handle for a parameter nobody reads.
    let rejected_here: &[(&str, &str)] = &[("_%5Ftimeout=333", "unknown parameter")];

    for (index, (query, journal, busy, sync, fk)) in matrix.iter().enumerate() {
        let name = format!("matrix{index}.db");
        let input = format!("{}?{}", in_dir(&dir, &name), query);
        if rejected_here.iter().any(|(q, _)| q == query) {
            match open(&input) {
                Ok(conn) => {
                    drop(conn);
                    panic!("{query} must be rejected, its decoded name names no setting");
                }
                Err(SchemaError::InvalidDsn { reason, .. }) => {
                    assert!(reason.contains("unknown parameter"), "{query}: {reason}")
                }
                Err(other) => panic!("{query}: expected InvalidDsn, got {other}"),
            }
            continue;
        }
        let (pool, first) = build_pool(PoolConfig::with_size(input.clone(), 2), query);
        pool.put_back(first).expect("return configured handle");
        every_handle(&pool, 2, |conn, handle| {
            let state = state_of(conn, &format!("handle {handle} of {query}"));
            match journal {
                Some(want) => assert_eq!(
                    state.journal_mode.to_lowercase(),
                    *want,
                    "{query}: handle {handle} journal_mode"
                ),
                None => assert_ne!(
                    state.journal_mode.to_lowercase(),
                    "wal",
                    "{query}: handle {handle} must not end up in WAL: {}",
                    state.journal_mode
                ),
            }
            assert_eq!(
                state.busy_timeout, *busy,
                "{query}: handle {handle} busy_timeout"
            );
            assert_eq!(
                state.synchronous, *sync,
                "{query}: handle {handle} synchronous"
            );
            if let Some(enabled) = fk {
                assert_eq!(
                    state.foreign_keys, *enabled,
                    "{query}: handle {handle} foreign_keys"
                );
            }
            assert_eq!(
                state.db_file.as_deref(),
                Some(canonical_of(&dir, &name).as_str()),
                "{query}: handle {handle} file name"
            );
        });
    }
    // No filename anywhere carries a query string.
    let stray: Vec<String> = listing(&dir)
        .into_iter()
        .filter(|name| name.contains('?'))
        .collect();
    assert!(
        stray.is_empty(),
        "matrix created query-string filenames: {stray:?}"
    );
}

#[test]
fn wal_persists_across_handles_and_a_reopened_file_reports_wal() {
    // WAL lives in the file, so a handle that never names a journal mode still
    // reports `wal` — the state go-sqlite3's handle has for an existing WAL DB.
    let dir = temp_dir("wal-persistence");
    require_wal_support(&dir);
    let path = in_dir(&dir, "wal.db");
    let created = match open(&go_dsn_named(&dir, "wal.db")) {
        Ok(conn) => conn,
        Err(cause) => panic!("{path}: {cause}"),
    };
    assert_eq!(state_of(&created, "creator").journal_mode, "wal");
    drop(created);

    let reopened = match open(&path) {
        Ok(conn) => conn,
        Err(cause) => panic!("reopen of an existing WAL file: {cause}"),
    };
    assert_eq!(
        state_of(&reopened, "reopened").journal_mode,
        "wal",
        "an existing WAL file reports wal even when the DSN names no journal mode"
    );
    let (pool, first) = build_pool(PoolConfig::with_size(path, 3), "existing WAL");
    pool.put_back(first).expect("return configured handle");
    every_handle(&pool, 3, |conn, index| {
        assert_eq!(
            state_of(conn, &format!("handle {index}")).journal_mode,
            "wal",
            "handle {index}"
        );
    });
}

/// `dir/<name>` carrying Navidrome's default parameter set.
fn go_dsn_named(dir: &Path, name: &str) -> String {
    let params = match DEFAULT_DB_PATH.find('?') {
        Some(at) => &DEFAULT_DB_PATH[at..],
        None => "",
    };
    format!("{}{}", in_dir(dir, name), params)
}

#[test]
fn wal_with_explicit_synchronous_full_reports_full_exactly_like_go() {
    // go-sqlite3's journal branch sets synchronous to NORMAL for WAL
    // (v1.14.52 sqlite3.go:1448-1454), but that is a no-op: NORMAL is already the
    // starting value (:1241) and the synchronous block runs afterwards and wins
    // whenever `_synchronous`/`_sync` is present (:1545-1549). Measured against the
    // driver: `?_journal_mode=wal&_synchronous=FULL` -> sync=2, `_sync=3` -> sync=3,
    // `?_journal_mode=wal` -> sync=1. So a DSN asking FULL under WAL ends at FULL,
    // including Navidrome's own `synchronous=normal` spelling, which Go reads as a
    // bare SQLite name and ignores. The port applies the two pragmas independently
    // and must agree — measured, not assumed.
    let dir = temp_dir("wal-full");
    require_wal_support(&dir);
    let dsn = format!(
        "{}?_journal_mode=wal&_synchronous=full",
        in_dir(&dir, "walfull.db")
    );
    let (pool, first) = build_pool(PoolConfig::with_size(dsn, 3), "WAL + FULL");
    pool_check(
        &pool,
        first,
        3,
        DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            synchronous: Synchronous::Full,
            ..DbOpenOptions::driver_defaults()
        },
    );

    // Navidrome's DefaultDbPath spells synchronous WITHOUT the underscore, and
    // go-sqlite3 never reads that spelling (measured: `?synchronous=full` leaves
    // sync=1; the port agrees). The bare name is accepted and ignored, which is also
    // what makes the default DSN parseable.
    let bare = format!(
        "{}?_journal_mode=wal&synchronous=full",
        in_dir(&dir, "barefull.db")
    );
    let target = match parse_open_input(&bare) {
        Ok(parsed) => parsed,
        Err(cause) => panic!("{bare}: {cause}"),
    };
    assert_eq!(
        target.options.synchronous,
        Synchronous::Normal,
        "the bare `synchronous` spelling is ignored, exactly as go-sqlite3 ignores it"
    );
    let (bare_pool, bare_first) = build_pool(PoolConfig::with_size(bare, 2), "bare synchronous");
    pool_check(
        &bare_pool,
        bare_first,
        2,
        DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            ..DbOpenOptions::driver_defaults()
        },
    );

    // Go's alias spellings, measured: `_synchronous=FULL` -> 2 and `_sync=3` -> 3
    // even with WAL, so the alias route must agree.
    for (params, expected) in [
        ("_journal_mode=wal&_synchronous=FULL", Synchronous::Full),
        ("_journal_mode=wal&_sync=3", Synchronous::Extra),
        ("_journal_mode=wal&_sync=0", Synchronous::Off),
    ] {
        let alias_dir = temp_dir(&format!("wal-sync-{}", expected.code()));
        let alias_dsn = format!("{}?{}", in_dir(&alias_dir, "alias.db"), params);
        let (alias_pool, alias_first) = build_pool(PoolConfig::with_size(alias_dsn, 2), params);
        pool_check(
            &alias_pool,
            alias_first,
            2,
            DbOpenOptions {
                journal_mode: Some(JournalMode::Wal),
                synchronous: expected,
                ..DbOpenOptions::driver_defaults()
            },
        );
    }
}

#[test]
fn a_read_back_mismatch_names_request_and_effective_state() {
    // F8(7). Two honest failure shapes, both reproduced:
    //   * SQLite accepts `PRAGMA journal_mode=delete` on an in-memory DB and answers
    //     `memory` with no error — a genuine read-back mismatch, reported as
    //     `OpenConfiguration` naming both values.
    //   * `mode=ro` plus a WAL request fails inside SQLite, so the port surfaces
    //     SQLite's own reason (`Failed`) rather than pretending the pragma worked.
    let dir = temp_dir("readback-message");
    require_wal_support(&dir);

    let uri = memory_uri(&dir, "db03_readback", "&_journal_mode=delete");
    match open(&uri) {
        Ok(conn) => {
            let got = state_of(&conn, &uri);
            panic!(
                "{uri} reads back {} and must be reported: {got:?}",
                got.journal_mode
            );
        }
        Err(SchemaError::OpenConfiguration { path, reason }) => {
            assert_eq!(path, uri);
            assert!(
                reason.contains("journal_mode requested delete"),
                "must name the request: {reason}"
            );
            assert!(
                reason.contains("SQLite reports memory"),
                "must name the effective state: {reason}"
            );
        }
        Err(other) => panic!("must be OpenConfiguration, got {other}"),
    }

    let path = in_dir(&dir, "ro.db");
    match open_with_options(
        &path,
        Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Delete),
            ..DbOpenOptions::driver_defaults()
        }),
    ) {
        Ok(conn) => drop(conn),
        Err(cause) => panic!("create the delete-journal file: {cause}"),
    }
    let ro = format!("file:{path}?mode=ro");
    match open_with_options(
        &ro,
        Some(DbOpenOptions {
            journal_mode: Some(JournalMode::Wal),
            ..DbOpenOptions::driver_defaults()
        }),
    ) {
        Ok(conn) => {
            let got = state_of(&conn, "read-only WAL handle");
            panic!(
                "a read-only handle cannot hold WAL, it reports {}",
                got.journal_mode
            );
        }
        // SQLite refuses the switch itself; the port reports SQLite's reason.
        Err(SchemaError::Failed { path, cause }) => {
            assert_eq!(path, ro, "the failure names the input");
            assert!(
                cause.to_string().contains("readonly"),
                "SQLite's own reason must surface: {cause}"
            );
        }
        Err(SchemaError::OpenConfiguration { reason, .. }) => assert!(
            reason.contains("journal_mode requested wal")
                && reason.contains("SQLite reports delete"),
            "a read-back mismatch must name both sides: {reason}"
        ),
        Err(other) => panic!("must be Failed or OpenConfiguration, got {other}"),
    }
}

#[test]
fn r6_f2_put_back_refuses_foreign_owner_and_updates_live_counts() {
    // This reaches ownership rejection, NOT a capacity arithmetic branch.
    // Real capacity is structural and exercised by the blocked-checkout test.
    let dir = temp_dir("r6-foreign-owner-accounting");
    let path = in_dir(&dir, "pool.db");
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    pool.put_back(first).unwrap();
    let (other, extra) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    assert!(pool
        .put_back(extra)
        .unwrap_err()
        .to_string()
        .contains("does not belong"));
    assert_eq!(other.live_count(), 0);
    assert_eq!(pool.live_count(), 1);
    assert_eq!(pool.idle_count().unwrap(), 1);
    pool.checkout()
        .unwrap()
        .conn()
        .query_row(SEEDEDRAND_SQL, [], |row| row.get::<_, i64>(0))
        .unwrap();
}

#[test]
fn r4_fix10_put_back_refuses_changed_state() {
    let dir = temp_dir("r4-admission-state");
    let path = in_dir(&dir, "pool.db");
    for pragma in [
        "busy_timeout=17",
        "synchronous=OFF",
        "foreign_keys=OFF",
        "journal_mode=MEMORY",
        "cache_size=17",
    ] {
        let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
        first.execute_batch(&format!("PRAGMA {pragma}")).unwrap();
        assert!(
            pool.put_back(first)
                .unwrap_err()
                .to_string()
                .contains("state differs"),
            "{pragma}"
        );
        assert_eq!(pool.idle_count().unwrap(), 0);
        assert_eq!(pool.live_count(), 0);
    }
}

#[test]
fn r4_fix10_put_back_requires_extensions_even_when_state_matches() {
    let dir = temp_dir("r4-admission-extensions");
    let path = in_dir(&dir, "pool.db");
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    first.remove_function("SEEDEDRAND", 2).unwrap();
    assert!(pool
        .put_back(first)
        .unwrap_err()
        .to_string()
        .contains("SEEDEDRAND"));
    assert_eq!(pool.live_count(), 0);
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    first.create_collation("NATSORT", |a, b| a.cmp(b)).unwrap();
    assert!(pool
        .put_back(first)
        .unwrap_err()
        .to_string()
        .contains("NATSORT"));
    assert_eq!(pool.idle_count().unwrap(), 0);
    assert_eq!(pool.live_count(), 0);
}

#[tokio::test]
async fn r5_h2_store_public_initialization_restores_all_slots() {
    for size in [1, 3] {
        let dir = temp_dir(&format!("r5-store-init-{size}"));
        let (store, first) =
            nd_db::store::SQLStore::open(PoolConfig::with_size(in_dir(&dir, "store.db"), size))
                .await
                .unwrap();
        store
            .initialize(first, |conn| {
                apply_fresh_schema(conn).map_err(|e| nd_db::store::StoreError::Open(e.to_string()))
            })
            .unwrap();
        assert_eq!(store.idle_count().unwrap(), size);
        assert_store_usable(store);
    }
}

#[tokio::test]
async fn r5_h2_store_checked_return_is_public() {
    let dir = temp_dir("r5-store-return");
    let (store, first) =
        nd_db::store::SQLStore::open(PoolConfig::with_size(in_dir(&dir, "store.db"), 1))
            .await
            .unwrap();
    apply_fresh_schema(&first).unwrap();
    store.put_back(first).unwrap();
    assert_eq!(store.idle_count().unwrap(), 1);
    assert_store_usable(store);
}

fn assert_store_usable(store: nd_db::store::SQLStore) {
    // A native thread avoids Tokio shutdown waiting forever on a regressed,
    // blocked spawn_blocking task. Both success and failure finish this test.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = store.with_connection_sync(|conn| {
            conn.query_row("SELECT count(*) FROM property", [], |row| {
                row.get::<_, i64>(0)
            })
        });
        let _ = done_tx.send(result);
    });
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap(),
        0
    );
}

#[test]
fn r5_h3_live_capacity_includes_lease_and_transaction() {
    let dir = temp_dir("r5-live-capacity");
    let path = in_dir(&dir, "pool.db");
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    assert_eq!(pool.live_count(), 1);
    pool.put_back(first).unwrap();
    let pool = std::sync::Arc::new(pool);
    let mut lease = pool.checkout().unwrap();
    assert_eq!(pool.idle_count().unwrap(), 0);
    {
        let tx = lease.transaction().unwrap();
        tx.execute("CREATE TABLE r5_uncommitted (id INTEGER)", [])
            .unwrap();
        assert_eq!(pool.live_count(), 1);
        assert_eq!(pool.idle_count().unwrap(), 0);
    } // transaction rolls back before lease returns
    let (other, extra) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    assert!(pool
        .put_back(extra)
        .unwrap_err()
        .to_string()
        .contains("does not belong"));
    assert_eq!(other.live_count(), 0);
    assert_eq!(pool.live_count(), 1);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let waiting_pool = std::sync::Arc::clone(&pool);
    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let second = waiting_pool.checkout().unwrap();
        let tables: i64 = second
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='r5_uncommitted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        done_tx.send(tables).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    drop(lease);
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(3)).unwrap(), 0);
    waiter.join().unwrap();
    assert_eq!(pool.live_count(), 1);
    assert_eq!(pool.idle_count().unwrap(), 1);
}

#[test]
fn r5_h3_tokens_are_pool_bound_even_with_identical_path_and_state() {
    let dir = temp_dir("r5-token-identity");
    let path = in_dir(&dir, "pool.db");
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    let (other, foreign) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    assert_eq!(
        effective_state(&first).unwrap(),
        effective_state(&foreign).unwrap()
    );
    assert!(pool
        .put_back(foreign)
        .unwrap_err()
        .to_string()
        .contains("does not belong"));
    assert_eq!(other.live_count(), 0);
    assert_eq!(pool.live_count(), 1);
    assert_eq!(pool.idle_count().unwrap(), 0);
    pool.put_back(first).unwrap();
    assert_eq!(pool.idle_count().unwrap(), 1);
}

#[test]
fn r5_h3_readonly_and_private_memory_tokens_cannot_enter_another_pool() {
    let dir = temp_dir("r5-foreign-modes");
    let path = in_dir(&dir, "pool.db");
    for (wanted, foreign) in [
        (path.clone(), format!("file:{path}?mode=ro")),
        (
            format!("file:{path}-memory?mode=memory&cache=shared"),
            format!("file:{path}-memory?mode=memory&cache=private"),
        ),
    ] {
        let (pool, first) = Pool::new(&PoolConfig::with_size(wanted, 1)).unwrap();
        first
            .execute("CREATE TABLE r5_identity (id INTEGER)", [])
            .unwrap();
        let (other, foreign) = Pool::new(&PoolConfig::with_size(foreign, 1)).unwrap();
        assert_eq!(
            effective_state(&first).unwrap(),
            effective_state(&foreign).unwrap()
        );
        assert!(pool
            .put_back(foreign)
            .unwrap_err()
            .to_string()
            .contains("does not belong"));
        assert_eq!(other.live_count(), 0);
        pool.put_back(first).unwrap();
        pool.checkout()
            .unwrap()
            .conn()
            .execute("INSERT INTO r5_identity VALUES (1)", [])
            .unwrap();
        assert_eq!(pool.live_count(), 1);
    }
}

#[test]
fn r5_h3_dropped_or_failed_initializers_close_and_decrement() {
    let dir = temp_dir("r5-failed-init");
    let path = in_dir(&dir, "pool.db");
    let (pool, first) = Pool::new(&PoolConfig::with_size(&path, 3)).unwrap();
    assert_eq!(pool.live_count(), 3);
    assert_eq!(pool.idle_count().unwrap(), 2);
    drop(first);
    assert_eq!(pool.live_count(), 2);
    assert_eq!(pool.idle_count().unwrap(), 2);
    let (failed, first) = Pool::new(&PoolConfig::with_size(&path, 1)).unwrap();
    let error = failed
        .initialize(first, |conn| {
            conn.execute_batch("CREATE TABLE r5_partial (id INTEGER)")?;
            Err(nd_db::store::StoreError::Open(
                "r5 initializer failed".to_string(),
            ))
        })
        .unwrap_err();
    assert!(error.to_string().contains("r5 initializer failed"));
    assert_eq!(failed.live_count(), 0);
    assert_eq!(failed.idle_count().unwrap(), 0);
}

#[test]
fn r5_h5_cache_size_is_configured_and_verified_on_every_handle() {
    let dir = temp_dir("r5-cache-pool");
    let path = format!("{}?_cache_size=2000", in_dir(&dir, "pool.db"));
    let (pool, first) = Pool::new(&PoolConfig::with_size(path, 3)).unwrap();
    assert_eq!(effective_state(&first).unwrap().cache_size, 2000);
    pool.put_back(first).unwrap();
    every_handle(&pool, 3, |conn, _| {
        assert_eq!(effective_state(conn).unwrap().cache_size, 2000)
    });
}

#[test]
fn r6_f1_overflow_cache_size_snapshot_still_refuses_a_changed_token() {
    let dir = temp_dir("r6-resolved-cache-snapshot");
    let path = format!("{}?_cache_size=-2147483649", in_dir(&dir, "pool.db"));
    let (pool, first) = Pool::new(&PoolConfig::with_size(path, 2)).unwrap();
    assert_eq!(effective_state(&first).unwrap().cache_size, 0);
    first.execute_batch("PRAGMA cache_size=17").unwrap();
    let error = pool.put_back(first).unwrap_err().to_string();
    assert!(error.contains("state differs"), "{error}");
    assert!(error.contains("cache_size: 0"), "{error}");
    assert!(error.contains("cache_size: 17"), "{error}");
    assert_eq!(pool.live_count(), 1);
    assert_eq!(pool.idle_count().unwrap(), 1);
    assert_eq!(
        effective_state(pool.checkout().unwrap().conn())
            .unwrap()
            .cache_size,
        0
    );
}

#[test]
fn r6_f3_disable_failure_preserves_message_and_closes_failed_token() {
    let dir = temp_dir("r6-disable-failure-mapping");
    let path = format!("{}?_fk=on", in_dir(&dir, "pool.db"));
    let (pool, first) = Pool::new(&PoolConfig::with_size(path, 1)).unwrap();
    let error = pool.initialize(first, |conn| {
        // BEGIN BEFORE apply: OFF cannot take effect, so restore-to-ON succeeds.
        conn.execute_batch("BEGIN")?;
        let cause = apply_fresh_schema(conn).unwrap_err();
        assert!(matches!(&cause, SchemaError::OpenConfiguration { path, .. } if path == "fresh_schema: disable foreign_keys"));
        assert!(effective_state(conn).unwrap().foreign_keys, "no FK leak on disable failure");
        conn.execute_batch("ROLLBACK")?;
        assert!(effective_state(conn).unwrap().foreign_keys);
        let objects: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
        assert_eq!(objects, 0, "DDL never started");
        println!("F3 disable mismatch: {cause}; FK=true; objects={objects}");
        Err(nd_db::store::StoreError::Open(cause.to_string()))
    }).unwrap_err();
    match error {
        nd_db::store::StoreError::Open(message) => {
            assert!(
                message.contains("fresh_schema: disable foreign_keys"),
                "{message}"
            );
            assert!(
                message.contains("foreign_keys requested false but SQLite reports true"),
                "{message}"
            );
        }
        other => panic!("accepted schema-error erasure must remain Open: {other}"),
    }
    assert_eq!(pool.live_count(), 0);
    assert_eq!(pool.idle_count().unwrap(), 0);
}

#[test]
fn r6_f3_restore_leak_preserves_message_and_closes_failed_token() {
    let dir = temp_dir("r6-restore-leak-mapping");
    let path = format!("{}?_fk=on", in_dir(&dir, "pool.db"));
    let (pool, first) = Pool::new(&PoolConfig::with_size(path, 1)).unwrap();
    let error = pool.initialize(first, |conn| {
        let mut began = false;
        let mut probe = |handle: &rusqlite::Connection| {
            // BEGIN AFTER OFF and a real DDL statement: restoring ON is ignored.
            // This explicitly mutating test probe needs no rusqlite hooks feature.
            if !began {
                assert!(!effective_state(handle).unwrap().foreign_keys);
                handle.execute_batch("BEGIN").unwrap();
                began = true;
            }
        };
        let cause = nd_dbschema::schema::apply_fresh_schema_with(conn, Some(&mut probe)).unwrap_err();
        assert!(began);
        assert!(matches!(&cause, SchemaError::OpenConfiguration { path, .. } if path == "fresh_schema"));
        assert!(!effective_state(conn).unwrap().foreign_keys, "restore failure really leaked OFF");
        let message = cause.to_string();
        assert!(message.contains("could not be restored to true"), "{message}");
        assert!(message.contains("foreign_keys=false"), "{message}");
        println!("F3 restore mismatch via mid-DDL probe: {cause}; FK=false");
        conn.execute_batch("ROLLBACK")?;
        Err(nd_db::store::StoreError::Open(message))
    }).unwrap_err();
    match error {
        nd_db::store::StoreError::Open(message) => {
            assert!(message.contains("foreign_keys=false"), "{message}");
            assert!(message.contains("could not be restored"), "{message}");
        }
        other => panic!("accepted schema-error erasure must remain Open: {other}"),
    }
    assert_eq!(pool.live_count(), 0);
    assert_eq!(pool.idle_count().unwrap(), 0);
    // DSN rejection also maps to Open: callers must not infer a discriminant.
    let rejected = Pool::new(&PoolConfig::with_size("file::memory:?_bogus=1", 1)).unwrap_err();
    assert!(
        matches!(rejected, nd_db::store::StoreError::Open(ref text) if text.contains("unknown parameter"))
    );
}
