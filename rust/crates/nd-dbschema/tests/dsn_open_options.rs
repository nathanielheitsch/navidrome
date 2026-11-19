//! DB-03 review F8 — the go-sqlite3 DSN grammar, parsed with the driver's own
//! precedence, plus the SQLite state each resolution actually produces.
//!
//! Every go-sqlite3 value quoted below was measured by running the driver
//! (v1.14.52, `sql.Open` + `SELECT 1` + `PRAGMA journal_mode|busy_timeout|
//! synchronous|foreign_keys` read-back):
//!
//! ```text
//!   go run . /tmp/x/case.db?QUERY      # see the reproduction at the bottom
//! ```
//!
//! and the Rust side is asserted against executed SQLite state, never against a
//! stringified expectation of what the pragma "should" say.
//!
//! Reproduction (outside this suite; no Go binary is part of it):
//! ```go
//!   db, _ := sql.Open("sqlite3", dsn)
//!   db.Exec("SELECT 1") // ConnectHook + PRAGMA pass happens on first use
//!   db.QueryRow("PRAGMA journal_mode").Scan(&j) // etc.
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use nd_conf::consts::DEFAULT_DB_PATH;
use nd_dbschema::schema::{
    effective_state, open, parse_open_input, DbOpenOptions, Error, JournalMode, Synchronous,
    DRIVER_BUSY_TIMEOUT_MS, MEMORY_DSN, NAVIDROME_BUSY_TIMEOUT_MS,
};

const TEMP_ROOT: &str = "/tmp/nd-dbschema-dsn-open-options";

/// The bare `:memory:` rewrite is ONE process-wide database
/// (`file::memory:?cache=shared`, `db/db.go:54-55`), and a shared cache serializes
/// DDL: two handles writing `sqlite_master` at once fail with
/// `database table is locked` (SQLite_LOCKED_SHAREDCACHE, extended 262) instead of
/// waiting out the busy timeout. So the tests that write through the shared DB are
/// serialized against each other; the private shapes need no lock, which is itself
/// the point of the G2 assertions.
static SHARED_MEMORY_DB: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn shared_memory_lock() -> std::sync::MutexGuard<'static, ()> {
    SHARED_MEMORY_DB
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(TEMP_ROOT).join(tag);
    match fs::remove_dir_all(&dir) {
        Ok(()) => (),
        Err(_absent) => (),
    }
    fs::create_dir_all(&dir).unwrap_or_else(|cause| panic!("create {}: {cause}", dir.display()));
    dir
}

/// Fail loudly — never weaken an assertion — if this filesystem cannot do WAL.
/// The probe file lives in its OWN directory, so a test that asserts an exact
/// directory listing is not disturbed by it.
fn require_wal_support(tag: &str) {
    let dir = temp_dir(&format!("{tag}-wal-probe"));
    let probe = in_dir(&dir, "wal-support-probe.db");
    let wanted = DbOpenOptions {
        journal_mode: Some(JournalMode::Wal),
        ..DbOpenOptions::driver_defaults()
    };
    match nd_dbschema::schema::open_with_options(&probe, Some(wanted)) {
        Ok(conn) => {
            let got = state(&conn, "WAL probe");
            drop(conn);
            assert_eq!(
                got.journal_mode,
                JournalMode::Wal.sqlite_name(),
                "STOP AND REPORT: WAL is unavailable on {} (journal_mode reads {}). \
                 The DB-03 assertions must not be weakened.",
                dir.display(),
                got.journal_mode
            );
        }
        Err(cause) => panic!(
            "STOP AND REPORT: WAL pragma failed on {}: {cause}",
            dir.display()
        ),
    }
}

/// An ABSOLUTE `mode=memory` URI. A relative `file:name?mode=memory` still gives a
/// memory DB, but the journal-switch pragma on it is what can materialize `name` in
/// the process working directory (the crate directory for these binaries), so every
/// memory DB here is addressed absolutely inside the test's temp dir.
fn memory_uri(dir: &Path, name: &str, extra: &str) -> String {
    format!("file:{}?mode=memory{extra}", in_dir(dir, name))
}

fn in_dir(dir: &Path, name: &str) -> String {
    let joined = dir.join(name);
    match joined.to_str() {
        Some(text) => text.to_string(),
        None => panic!("path is not valid UTF-8: {}", joined.display()),
    }
}

fn parse(input: &str) -> nd_dbschema::schema::OpenTarget {
    match parse_open_input(input) {
        Ok(target) => target,
        Err(cause) => panic!("{input} must parse: {cause}"),
    }
}

/// SQLite reports `PRAGMA busy_timeout` as an int; the requested value is a u64.
fn ms(value: u64) -> i64 {
    match i64::try_from(value) {
        Ok(n) => n,
        Err(_too_large) => panic!("busy_timeout {value} does not fit SQLite's int"),
    }
}

fn state(conn: &rusqlite::Connection, label: &str) -> nd_dbschema::schema::EffectiveState {
    match effective_state(conn) {
        Ok(state) => state,
        Err(cause) => panic!("{label}: {cause}"),
    }
}

/// `dir/<name>` carrying Navidrome's default parameter set — the shape
/// `Config::normalize_and_validate` builds (`nd-conf/src/config.rs:999-1010`).
fn default_dsn_in(dir: &Path, name: &str) -> String {
    let query = match DEFAULT_DB_PATH.find('?') {
        Some(at) => &DEFAULT_DB_PATH[at..],
        None => "",
    };
    format!("{}{}", in_dir(dir, name), query)
}

#[test]
fn navidrome_default_dsn_resolves_to_the_preset_and_only_forwards_cache_shared() {
    // Bare constant (as `Config` stores it before joining the data folder).
    let bare = parse(DEFAULT_DB_PATH);
    assert_eq!(bare.path, "navidrome.db");
    assert_eq!(
        bare.sqlite_arg, "file:navidrome.db?cache=shared",
        "cache is the only SQLite parameter forwarded, and it needs a file: URI"
    );
    assert_eq!(bare.options, DbOpenOptions::navidrome_default_options());
    assert!(!bare.is_memory);
    assert_eq!(bare.options.journal_mode, Some(JournalMode::Wal));
    assert_eq!(bare.options.busy_timeout, NAVIDROME_BUSY_TIMEOUT_MS);
    assert_eq!(bare.options.synchronous, Synchronous::Normal);
    assert_eq!(bare.options.foreign_keys, Some(true));

    // Same parameter set on a real directory, in a different order.
    let dir = temp_dir("default-dsn");
    require_wal_support("default-dsn");
    let reordered = format!(
        "{}?_foreign_keys=on&_journal_mode=WAL&synchronous=normal&_busy_timeout=15000&cache=shared",
        in_dir(&dir, "navidrome.db")
    );
    let target = parse(&reordered);
    assert_eq!(
        target.options,
        DbOpenOptions::navidrome_default_options(),
        "the preset match is order-insensitive"
    );
    assert_eq!(
        target.sqlite_arg,
        format!("file:{}?cache=shared", in_dir(&dir, "navidrome.db"))
    );

    // Executed state, on a real file: exactly the Go measurement
    // journal=wal busy=15000 sync=1 fk=1, file = the requested path with no query.
    let conn =
        open(&default_dsn_in(&dir, "navidrome.db")).unwrap_or_else(|cause| panic!("{cause}"));
    let got = state(&conn, "default DSN handle");
    assert_eq!(got.journal_mode, "wal");
    assert_eq!(got.busy_timeout, 15000);
    assert_eq!(got.synchronous, 1);
    assert!(got.foreign_keys);
    let wanted = fs::canonicalize(dir.join("navidrome.db")).unwrap();
    assert_eq!(
        got.db_file.as_deref(),
        wanted.to_str(),
        "PRAGMA database_list must name the requested file, query string removed"
    );
    drop(conn);
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|name| !name.contains('?')),
        "no filename may carry the query string: {names:?}"
    );
}

#[test]
fn any_other_input_gets_the_driver_state_not_navidromes_preset() {
    // Go: journalMode empty (`sqlite3.go:1236`) => no `PRAGMA journal_mode`;
    // foreignKeys = -1 (`:1234`) => no `PRAGMA foreign_keys`; busyTimeout 5000
    // (`:1231`); synchronousMode NORMAL (`:1241`, always exec'd `:1906`).
    let driver = DbOpenOptions::driver_defaults();
    assert_eq!(driver.journal_mode, None);
    assert_eq!(driver.foreign_keys, None);
    assert_eq!(driver.busy_timeout, DRIVER_BUSY_TIMEOUT_MS);
    assert_eq!(driver.synchronous, Synchronous::Normal);

    for input in [
        "/var/lib/navidrome/navidrome.db",
        "/data/custom.db",
        "relative.db",
    ] {
        let target = parse(input);
        assert_eq!(
            target.sqlite_arg, input,
            "no parameters, so the input verbatim"
        );
        assert_eq!(target.options, driver, "{input} must get driver state");
    }

    // Executed: a fresh file nobody configured stays on SQLite's default journal,
    // and a custom file named navidrome.db does NOT inherit WAL/15000.
    let dir = temp_dir("driver-state");
    require_wal_support("driver-state");
    let custom = in_dir(&dir, "navidrome.db");
    let conn = open(&custom).unwrap_or_else(|cause| panic!("{cause}"));
    let got = state(&conn, "custom navidrome.db");
    assert_eq!(got.journal_mode, "delete", "SQLite's own default journal");
    assert_eq!(got.busy_timeout, ms(DRIVER_BUSY_TIMEOUT_MS));
    assert_eq!(got.synchronous, Synchronous::Normal.code());
    drop(conn);
}

#[test]
fn memory_inputs_keep_gos_shared_cache_uri() {
    // `:memory:` is rewritten at `db/db.go:54-55` to
    // `file::memory:?cache=shared&_foreign_keys=on`. The URI form matters: a bare
    // `:memory:` is a PRIVATE database per handle, while the shared-cache URI is one
    // shared named DB (measured with this build: bare name → the second handle gets
    // an empty DB).
    let memory = parse(":memory:");
    assert_eq!(memory.sqlite_arg, "file::memory:?cache=shared");
    assert!(memory.is_memory);
    assert_eq!(memory.options.journal_mode, Some(JournalMode::Memory));
    assert_eq!(memory.options.foreign_keys, Some(true));

    let go_dsn = parse(MEMORY_DSN);
    assert_eq!(go_dsn.sqlite_arg, MEMORY_DSN);
    assert_eq!(go_dsn.options, memory.options);

    // `mode=memory` is a genuine SQLite URI parameter and survives verbatim.
    let uri = parse("file:scratch?mode=memory&cache=shared&_journal_mode=WAL");
    assert!(uri.is_memory);
    assert_eq!(
        uri.sqlite_arg,
        "file:scratch?mode=memory&cache=shared&_journal_mode=WAL"
    );
    assert_eq!(uri.options.journal_mode, Some(JournalMode::Wal));

    // A `mode=memory` URI without a cache keeps its `file:` form: dropping the
    // prefix would silently turn a named shared-cache DB into a private one.
    let no_cache = parse("file:private_mem?mode=memory&_busy_timeout=1234");
    assert!(no_cache.is_memory);
    assert_eq!(
        no_cache.sqlite_arg,
        "file:private_mem?mode=memory&_busy_timeout=1234"
    );
    assert_eq!(no_cache.options.busy_timeout, 1234);

    // Executed: the shared-cache URI really is shared across handles.
    let _serialized = shared_memory_lock();
    let a = open(":memory:").unwrap_or_else(|cause| panic!("{cause}"));
    let b = open(":memory:").unwrap_or_else(|cause| panic!("{cause}"));
    a.execute("CREATE TABLE db03_shared (k TEXT PRIMARY KEY, v TEXT)", [])
        .unwrap_or_else(|cause| panic!("DDL: {cause}"));
    a.execute("INSERT INTO db03_shared(k, v) VALUES('k', 'v')", [])
        .unwrap_or_else(|cause| panic!("insert: {cause}"));
    let read = b.query_row("SELECT v FROM db03_shared WHERE k = 'k'", [], |row| {
        row.get::<_, String>(0)
    });
    match read {
        Ok(value) => assert_eq!(value, "v", "the URI rewrite must share one DB"),
        Err(cause) => panic!("cross-handle visibility: {cause}"),
    }
    for handle in [&a, &b] {
        let got = state(handle, "memory handle");
        assert_eq!(got.journal_mode, "memory");
        assert_eq!(got.busy_timeout, ms(DRIVER_BUSY_TIMEOUT_MS));
        assert!(got.foreign_keys, "Go's memory rewrite sets FK on");
        assert_eq!(got.db_file, None);
    }
}

#[test]
fn a_file_uri_keeps_its_path_and_parameters_verbatim() {
    let input = "file:/data/x.db?cache=shared&_journal_mode=WAL&_busy_timeout=2500&_fk=on";
    let target = parse(input);
    assert_eq!(target.path, "/data/x.db", "the caller's own path spelling");
    assert_eq!(
        target.sqlite_arg, input,
        "SQLite receives the entire original URI, including inert conventions"
    );
    assert_eq!(target.options.journal_mode, Some(JournalMode::Wal));
    assert_eq!(target.options.busy_timeout, 2500);
    assert_eq!(target.options.foreign_keys, Some(true));

    // An already-encoded path is not decoded twice.
    let encoded = parse("file:/data/with%20space.db?cache=private");
    assert_eq!(
        encoded.sqlite_arg,
        "file:/data/with%20space.db?cache=private"
    );
    assert_eq!(encoded.path, "/data/with%20space.db");

    // Executed: SQLite really lands on `with space.db`.
    let dir = temp_dir("file-uri-encoded");
    require_wal_support("file-uri-encoded");
    let uri = format!("file:{}?cache=shared", in_dir(&dir, "with%20space.db"));
    let conn = open(&uri).unwrap_or_else(|cause| panic!("{uri}: {cause}"));
    let wanted = fs::canonicalize(dir.join("with space.db")).unwrap();
    assert_eq!(
        state(&conn, "encoded uri").db_file.as_deref(),
        wanted.to_str(),
        "a caller-encoded URI decodes to the intended file"
    );
}

#[test]
fn non_uri_inputs_reach_sqlite_with_no_parameters_at_all() {
    // go-sqlite3 strips the whole query for a DSN that does not start with `file:`
    // (`sqlite3.go:1599-1601`), so SQLite never sees `cache`, `mode`, or any
    // convention. Only Navidrome's default DSN gets `cache=shared` forwarded.
    let dir = temp_dir("stripped-query");
    require_wal_support("stripped-query");
    let plain = format!(
        "{}?cache=shared&_journal_mode=wal&_busy_timeout=222&_fk=on",
        in_dir(&dir, "plain.db")
    );
    let target = parse(&plain);
    assert_eq!(
        target.sqlite_arg,
        in_dir(&dir, "plain.db"),
        "SQLite receives only the filename"
    );
    assert_eq!(target.options.busy_timeout, 222);
    assert_eq!(target.options.journal_mode, Some(JournalMode::Wal));
    let conn = open(&plain).unwrap_or_else(|cause| panic!("{cause}"));
    let wanted = fs::canonicalize(dir.join("plain.db")).unwrap();
    let got = state(&conn, "stripped DSN");
    assert_eq!(got.db_file.as_deref(), wanted.to_str());
    assert_eq!(got.busy_timeout, 222);
    assert_eq!(got.journal_mode, "wal");
    drop(conn);
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["plain.db".to_string()],
        "one file, no query name"
    );
}

#[test]
fn option_precedence_matrix_matches_the_measured_driver() {
    /// (query, effective journal, effective busy, effective sync, effective FK).
    /// `None` journal means "the DSN named nothing", so a fresh file stays on
    /// SQLite's own `delete`. `None` FK means "named nothing": go-sqlite3 then execs
    /// no `PRAGMA foreign_keys` and Go's SQLite reports 0, while THIS build's SQLite
    /// is compiled with `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`
    /// (`libsqlite3-sys-0.30.1/build.rs:123`) and reports 1 — a build difference, not
    /// a parsing one, so the port leaves the compiled default alone and the column is
    /// only compared where the DSN named it.
    type Row = (&'static str, Option<&'static str>, i64, i64, Option<bool>);
    let matrix: &[Row] = &[
        // alias beats canonical wherever the alias appears, whatever the order, and
        // its FIRST value is used (`sqlite3.go:1338-1345` and friends).
        ("_busy_timeout=111&_timeout=222", None, 222, 1, None),
        ("_timeout=222&_busy_timeout=111", None, 222, 1, None),
        ("_journal_mode=wal&_journal=delete", Some("delete"), 5000, 1, None),
        ("_journal=delete&_journal_mode=wal", Some("delete"), 5000, 1, None),
        ("_synchronous=FULL&_sync=OFF", None, 5000, 0, None),
        ("_foreign_keys=on&_fk=off", None, 5000, 1, Some(false)),
        ("_foreign_keys=off&_fk=on", None, 5000, 1, Some(true)),
        // an alias that is PRESENT but EMPTY leaves the option unset.
        ("_journal_mode=wal&_journal=", None, 5000, 1, None),
        ("_busy_timeout=111&_timeout=", None, 5000, 1, None),
        ("_foreign_keys=on&_fk=", None, 5000, 1, None),
        ("_synchronous=FULL&_sync=", None, 5000, 1, None),
        // first occurrence of the winning name beats later repeats.
        ("_journal_mode=wal&_journal_mode=delete", Some("wal"), 5000, 1, None),
        ("_timeout=333&_timeout=444", None, 333, 1, None),
        // alias-only spellings.
        ("_journal=wal", Some("wal"), 5000, 1, None),
        ("_timeout=333", None, 333, 1, None),
        ("_fk=on", None, 5000, 1, Some(true)),
        ("_fk=off", None, 5000, 1, Some(false)),
        ("_sync=EXTRA", None, 5000, 3, None),
        ("_sync=FULL", None, 5000, 2, None),
        // bare SQLite names are not go-sqlite3 options at all: every option block
        // resets `pkey` to "" and only ever assigns an underscore name
        // (`sqlite3.go:1338-1345`, `:1401-1408`, `:1438-1445`, `:1538-1545`), so there
        // is no bare-name fallback. Measured: each leaves the driver state untouched.
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
        ("_%5Ftimeout=333", None, 5000, 1, None),
        // Option VALUES are matched case-insensitively, like Go's ToUpper/ToLower in
        // the option blocks (measured: `_fk=ON`, `_foreign_keys=True`, `_sync=full` all
        // apply), while NAMES are case-sensitive (see the rejected rows below).
        ("_fk=ON", None, 5000, 1, Some(true)),
        ("_foreign_keys=True", None, 5000, 1, Some(true)),
        ("_sync=full", None, 5000, 2, None),
        ("_journal_mode=Wal", Some("wal"), 5000, 1, None),
        ("_journal_mode=WAL", Some("wal"), 5000, 1, None),
        // percent-decoded names and values are matched after decoding.
        ("_journal%5Fmode=WAL", Some("wal"), 5000, 1, None),
        ("%5Fjournal_mode=WAL", Some("wal"), 5000, 1, None),
        ("_journal_mode=%77al", Some("wal"), 5000, 1, None),
        ("_busy_timeout=1%32%33%34", None, 1234, 1, None),
        // WAL with an explicit synchronous: go-sqlite3's WAL branch forces NORMAL
        // (`sqlite3.go:1448-1454`) but the synchronous block runs later and wins.
        ("_journal_mode=wal&_synchronous=FULL", Some("wal"), 5000, 2, None),
        ("_journal_mode=wal&_sync=3", Some("wal"), 5000, 3, None),
        ("_journal_mode=wal&_sync=0", Some("wal"), 5000, 0, None),
        ("_journal_mode=wal", Some("wal"), 5000, 1, None),
        // Option NAMES are matched case-sensitively by `params.Get` on the exact
        // literal, so these do nothing at all in Go (measured: each leaves
        // journal=delete/busy=5000/sync=1/fk=0). They are asserted as rejections via
        // `rejected_here` below rather than as driver-state rows: the port refuses to
        // open a handle whose requested setting was silently ignored.
        ("_JOURNAL_MODE=wal", None, 5000, 1, None),
        ("_Busy_Timeout=111", None, 5000, 1, None),
        ("_FK=on", None, 5000, 1, None),
        ("_SYNC=FULL", None, 5000, 1, None),
        ("_busy_timeout=+1234", None, 5000, 1, None),
        // Navidrome's default parameter set on a custom path.
        (
            "cache=shared&_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal",
            Some("wal"),
            15000,
            1,
            Some(true),
        ),
        (
            "_busy_timeout=15000&_journal_mode=WAL&_foreign_keys=on&synchronous=normal",
            Some("wal"),
            15000,
            1,
            Some(true),
        ),
    ];
    // Two shapes Go also refuses: a decoded name nobody reads, and a value its
    // `strconv.ParseInt` rejects (`+` decodes to a space, so `+1234` becomes " 1234"
    // and Go errors on first use with `Invalid _busy_timeout`).
    let rejected_here: &[(&str, &str)] = &[
        ("_%5Ftimeout=333", "unknown parameter"),
        ("_busy_timeout=+1234", "invalid _busy_timeout"),
        // go-sqlite3 matches option NAMES case-sensitively (`params.Get` on the
        // exact literal), so these silently do nothing in Go (measured: all four
        // leave journal=delete/busy=5000/sync=1/fk=0). The port refuses them instead
        // of opening a handle whose requested setting was ignored.
        ("_JOURNAL_MODE=wal", "unknown parameter"),
        ("_Busy_Timeout=111", "unknown parameter"),
        ("_FK=on", "unknown parameter"),
        ("_SYNC=FULL", "unknown parameter"),
    ];

    let dir = temp_dir("precedence-matrix");
    require_wal_support("precedence-matrix");
    for (index, (query, journal, busy, sync, fk)) in matrix.iter().enumerate() {
        let name = format!("matrix{index}.db");
        let input = format!("{}?{}", in_dir(&dir, &name), query);
        if rejected_here.iter().any(|(q, _)| q == query) {
            match open(&input) {
                Ok(conn) => {
                    drop(conn);
                    panic!("{query} must be rejected: its decoded name names no setting");
                }
                Err(Error::InvalidDsn { reason, .. }) => {
                    let needle = rejected_here
                        .iter()
                        .find(|(q, _)| q == query)
                        .map(|(_, needle)| *needle)
                        .unwrap_or("unknown parameter");
                    assert!(reason.contains(needle), "{query}: {reason}");
                }
                Err(other) => panic!("{query}: expected InvalidDsn, got {other}"),
            }
            continue;
        }
        let conn = open(&input).unwrap_or_else(|cause| panic!("{query}: {cause}"));
        let got = state(&conn, query);
        match journal {
            Some(wanted) => assert_eq!(
                got.journal_mode.to_lowercase(),
                *wanted,
                "{query}: journal_mode"
            ),
            None => assert_eq!(
                got.journal_mode, "delete",
                "{query}: an unnamed journal must stay on SQLite's default, got {}",
                got.journal_mode
            ),
        }
        assert_eq!(got.busy_timeout, *busy, "{query}: busy_timeout");
        assert_eq!(got.synchronous, *sync, "{query}: synchronous");
        if let Some(enabled) = fk {
            assert_eq!(got.foreign_keys, *enabled, "{query}: foreign_keys");
        }
        let wanted_file = fs::canonicalize(dir.join(&name)).unwrap();
        assert_eq!(
            got.db_file.as_deref(),
            wanted_file.to_str(),
            "{query}: file name"
        );
        drop(conn);
    }
    let stray: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains('?'))
        .collect();
    assert!(
        stray.is_empty(),
        "query-string filenames created: {stray:?}"
    );
}

#[test]
fn unknown_malformed_and_empty_names_fail_with_an_explicit_error() {
    // Measured in Go, these OPEN with a silently wrong or unchanged setting; the
    // port refuses them up front instead of handing out a mis-configured handle.
    let dir = temp_dir("rejections");
    let base = in_dir(&dir, "reject.db");
    let rejected: Vec<(String, &str)> = vec![
        (format!("{base}?_bogus=1"), "unknown parameter '_bogus'"),
        (
            format!("{base}?verbose=true"),
            "unknown parameter 'verbose'",
        ),
        (
            format!("{base}?_txlock=invalid"),
            "invalid _txlock: 'invalid', expecting 'deferred immediate exclusive'",
        ),
        (format!("{base}?_loc=invalid zone"), "invalid _loc"),
        // go-sqlite3 rejects these values too; the port refuses the parameter only
        // after validating it, so it never accepts a value Go would reject.
        (
            format!("{base}?_mutex=default"),
            "invalid _mutex: 'default'",
        ),
        (
            format!("{base}?_secure_delete=maybe"),
            "expecting boolean value of '0 1 false true no yes off on fast'",
        ),
        (
            format!("{base}?_auto_vacuum=wal"),
            "invalid _auto_vacuum: 'wal', expecting value of '0 NONE 1 FULL 2 INCREMENTAL'",
        ),
        (
            format!("{base}?_locking_mode=immediate"),
            "invalid _locking_mode: 'immediate', expecting value of 'NORMAL EXCLUSIVE'",
        ),
        (
            format!("{base}?_stmt_cache_size=-1"),
            "invalid _stmt_cache_size: '-1', expecting non-negative integer",
        ),
        (
            format!("{base}?_cache_size=abc"),
            "invalid _cache_size: 'abc'",
        ),
        (
            format!("{base}?_rt=maybe"),
            "invalid _recursive_triggers: 'maybe'",
        ),
        (
            format!("{base}?_cslike=maybe"),
            "invalid _case_sensitive_like: 'maybe'",
        ),
        (
            format!("{base}?_query_only=maybe"),
            "invalid _query_only: 'maybe'",
        ),
        (
            format!("{base}?_auth="),
            "'_auth=' is well-formed but cannot be applied",
        ),
        // A raw `;` anywhere in the query is what Go's first statement trips over
        // (`invalid semicolon separator in query`); a percent-encoded %3B is not a
        // separator, it decodes into the value and fails value validation instead.
        (
            format!("{base}?busy_timeout=111;_foreign_keys=on"),
            "invalid semicolon separator in query",
        ),
        (
            format!("{base}?_fk=on;"),
            "invalid semicolon separator in query",
        ),
        (
            format!("{base}?;_fk=on"),
            "invalid semicolon separator in query",
        ),
        (
            format!("{base}?_fk=on&;"),
            "invalid semicolon separator in query",
        ),
        (
            format!("{base}?_fk;o=n"),
            "invalid semicolon separator in query",
        ),
        (
            format!("{base}?_fk=on%3B_journal_mode=wal"),
            "invalid _fk: 'on;_journal_mode=wal'",
        ),
        // go-sqlite3 options that DO change connection state are validated with the
        // driver's grammar and then refused, never silently ignored (G7): rusqlite
        // 0.32.1 has no per-handle equivalent for them.
        (
            format!("{base}?_vacuum=full"),
            "'_vacuum=full' is well-formed but cannot be applied",
        ),
        (
            format!("{base}?_mutex=no"),
            "'_mutex=no' is well-formed but cannot be applied",
        ),
        (
            format!("{base}?_auto_vacuum=1"),
            "'_auto_vacuum=1' is well-formed but cannot be applied",
        ),
        (
            format!("{base}?locking_mode=immediate"),
            "unknown parameter",
        ),
        (format!("{base}?_journal_mode=sidecar"), "_journal_mode"),
        (format!("{base}?_journal=sidecar"), "_journal"),
        (format!("{base}?_busy_timeout=abc"), "_busy_timeout"),
        (format!("{base}?_timeout=abc"), "_timeout"),
        (format!("{base}?_synchronous=maybe"), "_synchronous"),
        (format!("{base}?_sync=maybe"), "_sync"),
        (format!("{base}?_foreign_keys=maybe"), "_foreign_keys"),
        (format!("{base}?cache"), "no value"),
        (format!("{base}?cache="), "empty value"),
        (format!("{base}?mode="), "empty value"),
        (format!("{base}?=wal"), "empty parameter name"),
        (format!("{base}?%zz=1"), "invalid URL escape"),
        (format!("{base}?_journal_mode=%zz"), "invalid URL escape"),
        (format!("{base}?_busy_timeout=12%2"), "invalid URL escape"),
        (format!("{base}?_journal_mode=%5"), "invalid URL escape"),
        (format!("file:{base}#tail"), "#"),
    ];
    for (input, needle) in rejected {
        match open(&input) {
            Ok(conn) => {
                drop(conn);
                panic!("{input} must be rejected, but it opened a connection");
            }
            Err(Error::InvalidDsn {
                input: named,
                reason,
            }) => {
                assert_eq!(named, input, "the error names the rejected input");
                assert!(
                    reason.contains(needle),
                    "reason must name {needle}: {reason}"
                );
            }
            Err(other) => panic!("{input} must fail as InvalidDsn, got {other}"),
        }
    }
    // Driver-only go-sqlite3 options (`_txlock`, `_loc`) configure no SQLite state,
    // so they are validated and dropped exactly as Go drops them. `verbose` is NOT a
    // go-sqlite3 parameter at all — v1.14.52 has no such key (`sqlite3.go:1247-1597`
    // is the complete list) — so it is an unknown parameter here, which is asserted
    // in the rejected list below.
    for params in ["_txlock=immediate", "_loc=UTC", "_loc=auto", "_txlock="] {
        let input = format!("{base}?{params}");
        let target = parse(&input);
        assert_eq!(
            target.options,
            DbOpenOptions::driver_defaults(),
            "{params} must configure nothing"
        );
        assert_eq!(target.sqlite_arg, base, "{params} must not reach SQLite");
    }

    // The one place the port cannot match Go exactly: `_loc` is validated by SHAPE
    // because the port has no zoneinfo access. A well-formed name that is simply not
    // installed — measured with the driver as
    // `_loc=UTC/Paris` -> `Invalid _loc: UTC/Paris: not a directory` — is accepted
    // here. It is asserted as harmless, because `_loc` configures only the driver's
    // row scanning and nothing observable on the handle.
    let uninstalled = format!("{base}?_loc=UTC/Paris");
    let target = parse(&uninstalled);
    assert_eq!(
        target.options,
        DbOpenOptions::driver_defaults(),
        "_loc configures no SQLite state, so an accepted-but-uninstalled name changes nothing"
    );
    assert_eq!(target.sqlite_arg, base, "_loc must not reach SQLite");
    // Executed in its own directory: the rejection test below asserts that this
    // directory stays empty.
    let live_dir = temp_dir("loc-uninstalled");
    let live = format!(
        "{}?_loc=UTC/Paris&_busy_timeout=777",
        in_dir(&live_dir, "loc.db")
    );
    let conn = open(&live).unwrap_or_else(|cause| panic!("{live}: {cause}"));
    let got = state(&conn, "_loc=UTC/Paris");
    assert_eq!(
        got.busy_timeout, 777,
        "the accepted _loc must not disturb any pragma"
    );
    assert_eq!(
        got.journal_mode, "delete",
        "and must not change the journal"
    );
    drop(conn);

    // Nothing was created by any rejected input.
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.is_empty(),
        "rejected inputs must create no file: {names:?}"
    );
}

#[test]
fn busy_timeout_clamps_and_zero_are_applied() {
    // Go: `?_busy_timeout=-5` opens and the handle reports 0 (SQLite clamps it).
    // 0 means "do not wait", so the value is applied, not rejected.
    let dir = temp_dir("busy-clamp");
    for (params, wanted) in [
        ("_busy_timeout=0", 0i64),
        ("_busy_timeout=-5", 0i64),
        ("_busy_timeout=250", 250i64),
    ] {
        let input = format!("{}?{params}", in_dir(&dir, "clamp.db"));
        let target = parse(&input);
        assert_eq!(
            target.options.busy_timeout,
            u64::try_from(wanted).unwrap_or(0),
            "{params}: parsed option"
        );
        let conn = open(&input).unwrap_or_else(|cause| panic!("{params}: {cause}"));
        assert_eq!(
            state(&conn, params).busy_timeout,
            wanted,
            "{params} applied"
        );
    }
}

#[test]
fn an_unnamed_journal_on_an_existing_wal_file_stays_wal() {
    // WAL lives in the file, so a handle that names no journal mode reports `wal` —
    // exactly the state go-sqlite3's handle has for an existing WAL database, and
    // why `journal_mode: None` must emit no pragma rather than `journal_mode=delete`.
    let dir = temp_dir("wal-reopen");
    require_wal_support("wal-reopen");
    let dsn = default_dsn_in(&dir, "wal.db");
    let created = open(&dsn).unwrap_or_else(|cause| panic!("{cause}"));
    assert_eq!(state(&created, "creator").journal_mode, "wal");
    drop(created);

    let reopened = open(&in_dir(&dir, "wal.db")).unwrap_or_else(|cause| panic!("{cause}"));
    assert_eq!(
        state(&reopened, "reopened").journal_mode,
        "wal",
        "an unnamed journal must not overwrite the file's WAL"
    );
}

#[test]
fn a_journal_that_reads_back_as_memory_is_reported() {
    // SQLite only allows MEMORY/OFF on an in-memory DB (`sqlite3.c:64459-64464`) and
    // answers a file journal with `memory` and NO error — so the read-back is the
    // only thing that can catch it.
    let dir = temp_dir("memory-journal");
    for journal in ["delete", "truncate", "persist"] {
        let uri = memory_uri(
            &dir,
            &format!("db03_mem_{journal}"),
            &format!("&_journal_mode={journal}"),
        );
        match open(&uri) {
            Ok(conn) => {
                let got = state(&conn, &uri);
                panic!(
                    "{journal} on a memory DB reads back {} and must be reported",
                    got.journal_mode
                );
            }
            Err(Error::OpenConfiguration { path, reason }) => {
                assert_eq!(path, uri);
                assert!(
                    reason.contains(&format!("journal_mode requested {journal}")),
                    "{reason}"
                );
                assert!(reason.contains("SQLite reports memory"), "{reason}");
            }
            Err(other) => panic!("{uri} must fail as OpenConfiguration, got {other}"),
        }
    }
    // OFF and MEMORY are legal there, and WAL is the accepted downgrade.
    for journal in ["off", "memory", "wal"] {
        let uri = memory_uri(
            &dir,
            &format!("db03_mem_ok_{journal}"),
            &format!("&_journal_mode={journal}"),
        );
        let conn = open(&uri).unwrap_or_else(|cause| panic!("{uri}: {cause}"));
        let wanted = if journal == "wal" { "memory" } else { journal };
        assert_eq!(state(&conn, &uri).journal_mode, wanted, "{uri}");
    }
}

// --------------------------------------------------------------------------------
// Round 3 (G1-G8): divergences measured against go-sqlite3 v1.14.52, each
// reproduced here against executed SQLite state.
// --------------------------------------------------------------------------------

fn canonical(dir: &Path, name: &str) -> String {
    let wanted = fs::canonicalize(dir.join(name)).unwrap();
    match wanted.to_str() {
        Some(text) => text.to_string(),
        None => panic!("path is not valid UTF-8: {}", wanted.display()),
    }
}

fn listing(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Permission bits of a file, via the unix extension trait imported once below.
fn file_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).unwrap_or_else(|cause| {
        panic!("mode of {}: {cause}", path.display());
    });
    metadata.permissions().mode() & 0o777
}

fn set_mode(dir: &Path, name: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode))
        .unwrap_or_else(|cause| panic!("chmod {name}: {cause}"));
}

#[test]
fn a_non_uri_mode_memory_is_a_real_file_not_a_memory_db() {
    // G1. go-sqlite3 strips the whole query of a DSN that does not start with
    // `file:` (`sqlite3.go:1599-1601`), so SQLite's plain-filename path runs and its
    // option reader (`sqlite3.c:182927-182930`) never sees `mode`. Measured against
    // the driver: journal=delete and `PRAGMA database_list` names the file. Treating
    // it as a memory DB would invent an option Go never passes and would silently
    // move Navidrome's database out of the file.
    let dir = temp_dir("non-uri-mode-memory");
    require_wal_support("non-uri-mode-memory");
    let input = format!(
        "{}?mode=memory&cache=shared&_journal_mode=wal&_fk=on",
        in_dir(&dir, "disk.db")
    );
    let target = parse(&input);
    assert!(
        !target.is_memory,
        "a non-URI mode=memory is not a memory DB: {target:?}"
    );
    assert_eq!(
        target.sqlite_arg,
        in_dir(&dir, "disk.db"),
        "SQLite receives the stripped filename, with no mode= at all"
    );
    assert!(
        !target.sqlite_arg.contains("mode="),
        "no URI option may survive a non-URI input: {}",
        target.sqlite_arg
    );
    assert_eq!(
        target.options.journal_mode,
        Some(JournalMode::Wal),
        "the go-sqlite3 convention is still a setting, unlike SQLite's own mode="
    );

    let conn = open(&input).unwrap_or_else(|cause| panic!("{cause}"));
    let got = state(&conn, "non-URI mode=memory handle");
    assert_ne!(
        got.journal_mode,
        JournalMode::Memory.sqlite_name(),
        "a real file cannot report the memory journal"
    );
    assert_eq!(got.journal_mode, "wal");
    assert_eq!(
        got.db_file.as_deref(),
        Some(canonical(&dir, "disk.db").as_str()),
        "PRAGMA database_list must name the file, not a memory DB"
    );
    drop(conn);
    assert_eq!(listing(&dir), vec!["disk.db".to_string()]);
}

#[test]
fn only_the_bare_memory_path_gets_gos_shared_cache_rewrite() {
    // G2. `db/db.go:53-56` is the exact test `Path == ":memory:"`, so the rewrite —
    // and with it the injected `cache=shared` and the FK-on default — applies to that
    // string alone. Measured with go-sqlite3, two handles per DSN: `:memory:`
    // (rewritten) is SHARED; `file::memory:` and `:memory:?_fk=off` are NOT-SHARED.
    // (a) Bare `:memory:`: rewritten, shared, FK on.
    let bare = parse(":memory:");
    assert_eq!(bare.sqlite_arg, "file::memory:?cache=shared");
    assert_eq!(bare.options.foreign_keys, Some(true));

    // (b) `file::memory:` stays the caller's own URI: no injected cache, and no FK
    // default, because the rewrite never fires.
    let uri_private = parse("file::memory:");
    assert_eq!(uri_private.sqlite_arg, "file::memory:");
    assert_eq!(
        uri_private.options.foreign_keys, None,
        "no rewrite means no FK default"
    );
    assert!(uri_private.is_memory, "SQLite itself keeps it in memory");

    // (c) `:memory:?_fk=off` does not match the rewrite: FK off, no injected cache,
    // and the query stripped like any other non-URI DSN.
    let fk_off = parse(":memory:?_fk=off");
    assert_eq!(
        fk_off.sqlite_arg, ":memory:",
        "a non-URI input loses its query, exactly like Go's stripped DSN, and SQLite reads the bare name as a PRIVATE memory DB"
    );
    assert_eq!(fk_off.options.foreign_keys, Some(false));
    assert!(
        !fk_off.sqlite_arg.contains("cache="),
        "Go's rewrite must not fire on a path with a query: {}",
        fk_off.sqlite_arg
    );

    // Executed, two handles per shape, both held at the same time. The shared shape
    // writes through the process-wide memory DB, so it takes the serialization lock;
    // the private shapes below do not need it.
    let _serialized = shared_memory_lock();
    let shared_a = open(":memory:").unwrap_or_else(|cause| panic!("{cause}"));
    let shared_b = open(":memory:").unwrap_or_else(|cause| panic!("{cause}"));
    shared_a
        .execute(
            "CREATE TABLE db03_g2_shared(k TEXT PRIMARY KEY, v TEXT)",
            [],
        )
        .unwrap_or_else(|cause| panic!("shared DDL: {cause}"));
    shared_a
        .execute("INSERT INTO db03_g2_shared(k, v) VALUES('k', 'v')", [])
        .unwrap_or_else(|cause| panic!("shared insert: {cause}"));
    let seen = shared_b.query_row("SELECT v FROM db03_g2_shared WHERE k = 'k'", [], |row| {
        row.get::<_, String>(0)
    });
    match seen {
        Ok(value) => assert_eq!(value, "v", "the bare `:memory:` rewrite must share"),
        Err(cause) => panic!("bare :memory: must be shared: {cause}"),
    }

    for dsn in ["file::memory:", ":memory:?_fk=off"] {
        let writer = open(dsn).unwrap_or_else(|cause| panic!("{dsn}: {cause}"));
        let reader = open(dsn).unwrap_or_else(|cause| panic!("{dsn}: {cause}"));
        writer
            .execute(
                "CREATE TABLE db03_g2_private(k TEXT PRIMARY KEY, v TEXT)",
                [],
            )
            .unwrap_or_else(|cause| panic!("{dsn} DDL: {cause}"));
        let refused = reader.query_row("SELECT v FROM db03_g2_private WHERE k = 'k'", [], |row| {
            row.get::<_, String>(0)
        });
        match refused {
            Ok(value) => panic!("{dsn} must stay PRIVATE, but the second handle read {value}"),
            Err(cause) => assert!(
                cause.to_string().contains("no such table"),
                "{dsn} must be a separate DB: {cause}"
            ),
        }
        if dsn == ":memory:?_fk=off" {
            assert!(
                !state(&writer, dsn).foreign_keys,
                "{dsn}: the DSN names FK off and no rewrite may override it"
            );
        } else {
            // `file::memory:` names no FK at all, so no pragma is emitted and the
            // handle keeps this build's compiled default
            // (`-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, libsqlite3-sys build.rs:123) —
            // NOT Go's 0, and not something the port asserts as a request.
            assert!(
                state(&writer, dsn).foreign_keys,
                "{dsn}: nothing named FK, so the compiled default stands"
            );
        }
        drop((writer, reader));
    }
    drop((shared_a, shared_b));
}

#[test]
fn re_emitted_uri_parameters_are_percent_encoded_exactly_once() {
    // G4. SQLite percent-decodes URI option values while parsing
    // (`sqlite3.c:182900-182960`), so re-emitting an already-decoded value lets it
    // decode a SECOND time. Measured against go-sqlite3:
    //   `file:…/perm.db?modeof=/tmp/x/mask%26a` opens with `modeof` pointing at the
    //   directory literally named `mask%26a`; an unescaped re-emit split the value on
    //   `&` and failed with `disk I/O error`, and with a stray `mask` directory
    //   present it applied the WRONG permission mask (0644 instead of 0600).
    // `modeof` copies the named directory's permission bits onto the DB file, so the
    // file's mode is the executed proof of which directory SQLite was told to use.
    let dir = temp_dir("uri-re-encode");
    // SQLite decodes a URI option value EXACTLY ONCE, so a caller who wants the
    // directory literally named `mask%26a` must write `mask%2526a`. Measured against
    // go-sqlite3 on the same directory set: `modeof=…/mask%26a` decodes to `mask&a`
    // and fails with `disk I/O error` when that directory is absent, while
    // `modeof=…/mask%2526a` opens. The stray `mask` directory below is the trap: an
    // unescaped re-emit makes SQLite split the value on `&` and apply the WRONG mask.
    for (name, mode) in [
        ("mask&a", 0o700),
        ("mask%26a", 0o640),
        ("mask space", 0o755),
        ("mask%20space", 0o600),
        ("mask", 0o644),
    ] {
        fs::create_dir(dir.join(name)).unwrap_or_else(|cause| panic!("mkdir {name}: {cause}"));
        set_mode(&dir, name, mode);
    }

    // (1) `%26` in the caller's spelling decodes to `&`: SQLite names `mask&a`.
    let once = format!(
        "file:{}?modeof={}",
        in_dir(&dir, "perm1.db"),
        in_dir(&dir, "mask%26a")
    );
    let target = parse(&once);
    assert_eq!(
        target.sqlite_arg.matches('&').count(),
        0,
        "a decoded `&` must never be handed back to SQLite to split the query on: {}",
        target.sqlite_arg
    );
    assert!(
        target.sqlite_arg.contains("mask%26a"),
        "the decoded value is re-encoded once, so SQLite's single decode gives `mask&a`: {}",
        target.sqlite_arg
    );
    let conn = open(&once).unwrap_or_else(|cause| panic!("{once}: {cause}"));
    assert_eq!(
        state(&conn, "modeof once").db_file.as_deref(),
        Some(canonical(&dir, "perm1.db").as_str())
    );
    drop(conn);
    assert_eq!(
        file_mode(&dir.join("perm1.db")),
        0o700,
        "modeof must apply `mask&a` (0700), which is what SQLite decodes `%26` to — never the stray `mask` (0644)"
    );

    // (2) `%2526` stays a literal `%26`: SQLite names the directory `mask%26a`.
    let twice = format!(
        "file:{}?modeof={}",
        in_dir(&dir, "perm2.db"),
        in_dir(&dir, "mask%2526a")
    );
    let target = parse(&twice);
    assert_eq!(
        target.sqlite_arg.matches('&').count(),
        0,
        "no separator may reach SQLite: {}",
        target.sqlite_arg
    );
    assert!(
        target.sqlite_arg.contains("mask%2526a"),
        "%25 survives exactly one re-encode, so the value is not double-decoded: {}",
        target.sqlite_arg
    );
    let conn = open(&twice).unwrap_or_else(|cause| panic!("{twice}: {cause}"));
    drop(conn);
    assert_eq!(
        file_mode(&dir.join("perm2.db")),
        0o640,
        "`%2526` must name the directory literally called `mask%26a`"
    );

    // (3) `%20` decodes to a space.
    let spaced = format!(
        "file:{}?modeof={}",
        in_dir(&dir, "perm3.db"),
        in_dir(&dir, "mask%20space")
    );
    let conn = open(&spaced).unwrap_or_else(|cause| panic!("{spaced}: {cause}"));
    drop(conn);
    assert_eq!(
        file_mode(&dir.join("perm3.db")),
        0o755,
        "`%20` must name the directory called `mask space`"
    );

    // (4) `%2520` stays a literal `%20` and must NOT become a space.
    let literal = format!(
        "file:{}?modeof={}",
        in_dir(&dir, "perm4.db"),
        in_dir(&dir, "mask%2520space")
    );
    let conn = open(&literal).unwrap_or_else(|cause| panic!("{literal}: {cause}"));
    drop(conn);
    assert_eq!(
        file_mode(&dir.join("perm4.db")),
        0o600,
        "`%2520` must name the directory literally called `mask%20space`"
    );

    let mut names = listing(&dir);
    names.sort();
    assert_eq!(
        names,
        vec![
            "mask".to_string(),
            "mask space".to_string(),
            "mask%20space".to_string(),
            "mask%26a".to_string(),
            "mask&a".to_string(),
            "perm1.db".to_string(),
            "perm2.db".to_string(),
            "perm3.db".to_string(),
            "perm4.db".to_string(),
        ],
        "no stray file, no query-string name"
    );
}

#[test]
fn a_double_slash_path_is_handed_to_sqlite_as_a_plain_filename() {
    // G5. go-sqlite3 never builds a URI for a DSN that does not start with `file:`,
    // so `//tmp/x/navidrome.db?<query>` opens `/tmp/x/navidrome.db` — measured
    // against the driver: journal=wal, busy=15000, sync=1, fk=1, and
    // `PRAGMA database_list` = `/private/tmp/…/navidrome.db`. Turning it into
    // `file://…` instead makes SQLite read `tmp` as a URI authority
    // (`sqlite3.c:182878-182896`, "invalid uri authority") and the open fails.
    let dir = temp_dir("double-slash");
    require_wal_support("double-slash");
    let text = dir
        .join("navidrome.db")
        .to_str()
        .unwrap_or_else(|| panic!("non-UTF-8 temp path"))
        .to_string();
    let doubled = format!("//{text}");
    assert!(doubled.starts_with("//"), "the probe needs a leading //");
    let query = match DEFAULT_DB_PATH.find('?') {
        Some(at) => &DEFAULT_DB_PATH[at..],
        None => "",
    };
    let input = format!("{doubled}{query}");
    let target = parse(&input);
    assert_eq!(
        target.sqlite_arg, doubled,
        "no file: form may be built for a path SQLite would read as an authority"
    );
    assert_eq!(
        target.options,
        DbOpenOptions::navidrome_default_options(),
        "the preset is still recognized on the doubled spelling"
    );
    let conn = open(&input).unwrap_or_else(|cause| panic!("{input}: {cause}"));
    let got = state(&conn, "double-slash handle");
    assert_eq!(got.journal_mode, "wal");
    assert_eq!(got.busy_timeout, 15000);
    assert_eq!(got.synchronous, Synchronous::Normal.code());
    assert!(got.foreign_keys);
    assert_eq!(
        got.db_file.as_deref(),
        Some(canonical(&dir, "navidrome.db").as_str()),
        "SQLite must land on the intended file"
    );
    drop(conn);
    assert_eq!(listing(&dir), vec!["navidrome.db".to_string()]);
}

#[test]
fn the_preset_matches_the_percent_encoded_canonical_query() {
    // G6. go-sqlite3 percent-decodes names through `url.ParseQuery`, so
    // `_busy%5Ftimeout=15000` IS `_busy_timeout=15000` — measured against the driver,
    // the encoded canonical query gives byte-identical pragma state to the plain one.
    // Matching the preset on RAW items missed it, which dropped `cache=shared` from
    // the SQLite argument and gave the same configuration a different lock class
    // (SQLITE_LOCKED_SHAREDCACHE, extended 262, instead of SQLITE_BUSY, extended 5).
    let dir = temp_dir("preset-encoded");
    require_wal_support("preset-encoded");
    let encoded = format!(
        "{}?cache=shared&_busy%5Ftimeout=15000&_journal%5Fmode=WAL&_foreign%5Fkeys=on&synchronous=normal",
        in_dir(&dir, "navidrome.db")
    );
    let plain = default_dsn_in(&dir, "navidrome_plain.db");
    let target = parse(&encoded);
    let plain_target = parse(&plain);
    assert_eq!(
        target.options,
        DbOpenOptions::navidrome_default_options(),
        "the encoded canonical query IS the preset"
    );
    assert_eq!(
        target.options, plain_target.options,
        "encoded and plain resolve identically"
    );
    assert_eq!(
        target.sqlite_arg,
        format!("file:{}?cache=shared", in_dir(&dir, "navidrome.db")),
        "cache=shared is forwarded for the encoded form too"
    );

    // Encoded and reordered together still matches.
    let shuffled = format!(
        "{}?synchronous=normal&_foreign%5Fkeys=on&cache=shared&_journal%5Fmode=WAL&_busy%5Ftimeout=15000",
        in_dir(&dir, "navidrome.db")
    );
    assert_eq!(
        parse(&shuffled).options,
        DbOpenOptions::navidrome_default_options(),
        "decoded and order-insensitive"
    );

    let conn = open(&encoded).unwrap_or_else(|cause| panic!("{encoded}: {cause}"));
    let got = state(&conn, "encoded preset");
    assert_eq!(got.journal_mode, "wal");
    assert_eq!(got.busy_timeout, 15000);
    assert_eq!(got.synchronous, 1);
    assert!(got.foreign_keys);
    drop(conn);
}

#[test]
fn a_busy_timeout_outside_sqlites_domain_resolves_to_zero() {
    // G8. go-sqlite3 sends the timeout as pragma TEXT (`sqlite3.go:1665`), and
    // SQLite's `sqlite3Atoi` yields 0 — with no error — when the magnitude exceeds
    // INT_MAX or the digits exceed 10 (`sqlite3.c:141340`, `:35858-35910`). Such a
    // DSN OPENS and reports 0, so failing the read-back here would reject an input
    // Go accepts. Measured on both sides; the table is the driver's own output.
    let dir = temp_dir("busy-domain");
    let cases: &[(i64, i64)] = &[
        (2_147_483_647, 2_147_483_647),
        (2_147_483_648, 0),
        (4_294_967_296, 0),
        (4_294_967_297, 0),
        (10_000_000_000, 0),
        (9_223_372_036_854_775_807, 0),
        (-1, 0),
        (-2_147_483_649, 0),
        (0, 0),
        (777, 777),
    ];
    for (requested, expected) in cases {
        let name = format!("bt{requested}.db");
        let input = format!("{}?_busy_timeout={requested}", in_dir(&dir, &name));
        let target = parse(&input);
        assert_eq!(
            target.options.busy_timeout,
            u64::try_from(*expected).unwrap_or(0),
            "{requested} must resolve to SQLite's domain before it is applied"
        );
        let conn = open(&input).unwrap_or_else(|cause| panic!("{input} must open: {cause}"));
        assert_eq!(
            state(&conn, &name).busy_timeout,
            *expected,
            "{requested}: busy_timeout read back"
        );
        drop(conn);
    }
    // A percent-encoded `+` is NOT a space, so `%2B5` decodes to `+5`, which Go's
    // ParseInt accepts (a leading sign is legal) — measured `…?_busy_timeout=%2B5`
    // -> busy=5. A RAW `+` decodes to a space and fails on both sides
    // (`strconv.ParseInt: parsing " 5": invalid syntax`), so the two spellings differ
    // exactly as they do in Go.
    let encoded_sign = format!("{}?_busy_timeout=%2B5", in_dir(&dir, "sign.db"));
    let conn =
        open(&encoded_sign).unwrap_or_else(|cause| panic!("{encoded_sign} must open: {cause}"));
    assert_eq!(
        state(&conn, "%2B5").busy_timeout,
        5,
        "a decoded leading sign is a valid integer for Go's ParseInt"
    );
    drop(conn);

    // Go's own parser rejects these, so the port rejects them too.
    for text in ["9223372036854775808", "0x10", "1_000", "12.5", "+5", "abc"] {
        let input = format!("{}?_busy_timeout={text}", in_dir(&dir, "reject.db"));
        match open(&input) {
            Ok(conn) => {
                drop(conn);
                panic!("{text} must be rejected — go-sqlite3's ParseInt rejects it too");
            }
            Err(Error::InvalidDsn { reason, .. }) => {
                assert!(reason.contains("invalid _busy_timeout"), "{reason}");
            }
            Err(other) => panic!("{text}: expected InvalidDsn, got {other}"),
        }
    }
    // Leading zeros are normalized by Go's parser, so SQLite sees the small value.
    let padded = format!(
        "{}?_busy_timeout=000000000000000000005",
        in_dir(&dir, "padded.db")
    );
    let conn = open(&padded).unwrap_or_else(|cause| panic!("{padded}: {cause}"));
    assert_eq!(state(&conn, "padded").busy_timeout, 5);
    drop(conn);
    let names: Vec<String> = listing(&dir)
        .into_iter()
        .filter(|name| name.contains('?'))
        .collect();
    assert!(names.is_empty(), "query-string filenames: {names:?}");
}

#[test]
fn vfs_is_handed_to_sqlite_for_uri_and_plain_inputs_alike() {
    // G3. go-sqlite3 extracts `vfs` BEFORE stripping the query
    // (`sqlite3.go:1595-1597`) and passes it as its own argument to
    // `sqlite3_open_v2` (`:1607-1614`), so it is honored for a NON-URI DSN too and a
    // bad name fails on first use: measured
    // `/tmp/…/x.db?vfs=not-a-vfs` -> `no such vfs: not-a-vfs`. rusqlite exposes the
    // same argument (`Connection::open_with_flags_and_vfs`, rusqlite-0.32.1
    // src/lib.rs:492-500 -> src/inner_connection.rs:69-88), so the port passes it
    // through in both shapes instead of opening on the default VFS.
    let dir = temp_dir("vfs");

    // URI form: SQLite applies it, and a bogus name fails at open.
    let good = format!("file:{}?vfs=unix", in_dir(&dir, "vfs-ok.db"));
    let target = parse(&good);
    assert_eq!(target.vfs.as_deref(), Some("unix"));
    let conn = open(&good).unwrap_or_else(|cause| panic!("{good}: {cause}"));
    assert_eq!(
        state(&conn, "vfs=unix").db_file.as_deref(),
        Some(canonical(&dir, "vfs-ok.db").as_str())
    );
    drop(conn);

    let bad_uri = format!("file:{}?vfs=not-a-vfs", in_dir(&dir, "vfs-bad-uri.db"));
    match open(&bad_uri) {
        Ok(conn) => {
            drop(conn);
            panic!("a bogus vfs must fail, exactly like go-sqlite3's `no such vfs`");
        }
        Err(cause) => assert!(
            cause.to_string().contains("no such vfs"),
            "must be SQLite's own VFS error: {cause}"
        ),
    }

    // Non-URI form: the parameter is stripped from the filename AND still applied,
    // which is only observable through the open argument. Ignoring it would have
    // opened this on the default VFS with no error at all.
    let plain = parse(&format!(
        "{}?vfs=unix&_journal_mode=wal&_fk=on",
        in_dir(&dir, "vfs-plain.db")
    ));
    assert_eq!(
        plain.sqlite_arg,
        in_dir(&dir, "vfs-plain.db"),
        "vfs must not survive in the filename, exactly like Go's stripped DSN"
    );
    assert_eq!(
        plain.vfs.as_deref(),
        Some("unix"),
        "and must still be applied"
    );
    let conn = open(&format!(
        "{}?vfs=unix&_journal_mode=wal&_fk=on",
        in_dir(&dir, "vfs-plain.db")
    ))
    .unwrap_or_else(|cause| panic!("non-URI vfs=unix: {cause}"));
    let got = state(&conn, "non-URI vfs=unix");
    assert_eq!(
        got.db_file.as_deref(),
        Some(canonical(&dir, "vfs-plain.db").as_str())
    );
    assert_eq!(
        got.journal_mode, "wal",
        "the go-sqlite3 pragmas still apply"
    );
    assert!(got.foreign_keys);
    drop(conn);

    // A bad name on a plain input fails with SQLite's own error, as Go's does.
    let bad_plain = format!("{}?vfs=not-a-vfs", in_dir(&dir, "vfs-bad-plain.db"));
    match open(&bad_plain) {
        Ok(conn) => {
            drop(conn);
            panic!("a bogus vfs on a non-URI input must fail, like go-sqlite3: {bad_plain}");
        }
        Err(cause) => assert!(
            cause.to_string().contains("no such vfs"),
            "must be SQLite's own VFS error, not a silently ignored parameter: {cause}"
        ),
    }

    assert_eq!(
        listing(&dir),
        vec!["vfs-ok.db".to_string(), "vfs-plain.db".to_string()],
        "no file whose name carries the query, and none created by a rejected open"
    );
}

// Round 4: regression tests for the two independent query grammars.
#[test]
fn r4_fix1_only_exact_bare_memory_is_rewritten() {
    let _serialized = shared_memory_lock();
    for (input, shared) in [
        (":memory:", true),
        (":memory:?cache=shared", false),
        (":memory:?_fk=off&cache=shared", false),
        (":memory:?mode=ro", false),
        ("file::memory:", false),
        ("file::memory:?cache=shared", true),
    ] {
        let a = open(input).unwrap();
        let b = open(input).unwrap();
        a.execute("CREATE TABLE r4_visibility (id INTEGER)", [])
            .unwrap();
        a.execute("INSERT INTO r4_visibility VALUES (1)", [])
            .unwrap();
        let seen = b.query_row("SELECT count(*) FROM r4_visibility", [], |row| {
            row.get::<_, i64>(0)
        });
        if shared {
            assert_eq!(seen.unwrap(), 1, "{input}");
        } else {
            assert!(
                seen.unwrap_err().to_string().contains("no such table"),
                "{input}"
            );
            if !input.starts_with("file:") {
                assert_eq!(parse(input).sqlite_arg, ":memory:");
            }
        }
        if input == ":memory:" {
            assert!(state(&a, input).foreign_keys);
        }
    }
    let ro = open("file::memory:?mode=ro").unwrap();
    assert!(ro
        .execute("CREATE TABLE r4_readonly (id INTEGER)", [])
        .unwrap_err()
        .to_string()
        .contains("readonly"));
}

#[test]
fn r4_fix2_ordered_mode_readonly_is_not_deduplicated() {
    let dir = temp_dir("r4-mode-readonly");
    let path = in_dir(&dir, "existing.db");
    open(&path)
        .unwrap()
        .execute("CREATE TABLE r4_data (id INTEGER)", [])
        .unwrap();
    let uri = format!("file:{path}?mode=rwc&mode=ro");
    let ro = open(&uri).unwrap();
    assert!(ro
        .execute("INSERT INTO r4_data VALUES (1)", [])
        .unwrap_err()
        .to_string()
        .contains("readonly"));
    let denied = open(&format!("file:{path}?mode=ro&mode=rwc")).unwrap_err();
    assert!(denied.to_string().contains("access mode not allowed: rwc"));
    assert_eq!(parse(&uri).sqlite_arg, uri);
}

#[test]
fn r4_fix2_ordered_mode_memory_and_file_are_distinct() {
    let dir = temp_dir("r4-mode-memory-file");
    for (index, query, journal, memory) in [
        (0, "mode=memory&mode=rwc", "delete", false),
        (1, "mode=rwc&mode=memory", "memory", true),
    ] {
        let path = in_dir(&dir, &format!("order{index}.db"));
        let conn = open(&format!("file:{path}?{query}")).unwrap();
        conn.execute("CREATE TABLE r4_order (id INTEGER)", [])
            .unwrap();
        let got = state(&conn, query);
        assert_eq!(got.journal_mode, journal);
        assert_eq!(got.db_file.is_none(), memory);
        assert_eq!(Path::new(&path).exists(), !memory);
    }
}

#[test]
fn r4_fix2_ordered_cache_controls_cross_handle_visibility() {
    let dir = temp_dir("r4-cache-order");
    for (index, query, shared) in [
        (0, "cache=shared&cache=private", false),
        (1, "cache=private&cache=shared", true),
    ] {
        let uri = format!(
            "file:{}?mode=memory&{query}",
            in_dir(&dir, &format!("order{index}"))
        );
        let a = open(&uri).unwrap();
        let b = open(&uri).unwrap();
        a.execute("CREATE TABLE r4_cache (id INTEGER)", []).unwrap();
        let seen = b.query_row("SELECT count(*) FROM r4_cache", [], |row| {
            row.get::<_, i64>(0)
        });
        if shared {
            assert_eq!(seen.unwrap(), 0);
        } else {
            assert!(seen.unwrap_err().to_string().contains("no such table"));
        }
    }
}

#[test]
fn r4_fix2_and_fix8_uri_vfs_overwrites_driver_zvfs_in_order() {
    let dir = temp_dir("r4-vfs-order");
    let path = in_dir(&dir, "vfs.db");
    let bad = format!("file:{path}?vfs=unix&vfs=not-a-vfs");
    assert_eq!(parse(&bad).vfs.as_deref(), Some("unix"));
    assert!(open(&bad).unwrap_err().to_string().contains("no such vfs"));
    let good = format!("file:{path}?vfs=not-a-vfs&vfs=unix");
    assert_eq!(parse(&good).vfs.as_deref(), Some("not-a-vfs"));
    let conn = open(&good).unwrap();
    conn.execute("CREATE TABLE r4_vfs (id INTEGER)", [])
        .unwrap();
    assert!(Path::new(&path).exists());
}

#[cfg(unix)]
#[test]
fn r4_fix3_uri_plus_and_percent_escapes_are_decoded_only_by_sqlite() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("r4-modeof-grammar");
    for (name, mode) in [
        ("mask+a", 0o711),
        ("mask a", 0o755),
        ("mask&a", 0o700),
        ("mask", 0o755),
    ] {
        let mask = dir.join(name);
        fs::create_dir(&mask).unwrap();
        fs::set_permissions(&mask, fs::Permissions::from_mode(mode)).unwrap();
    }
    for (index, spelling, mode) in [
        (0, "mask+a", 0o711),
        (1, "mask%2Ba", 0o711),
        (2, "mask%26a", 0o700),
    ] {
        let path = dir.join(format!("perm{index}.db"));
        let uri = format!(
            "file:{}?modeof={}/{}",
            path.display(),
            dir.display(),
            spelling
        );
        let conn = open(&uri).unwrap();
        conn.execute("CREATE TABLE r4_permissions (id INTEGER)", [])
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            mode,
            "{spelling}"
        );
        assert_eq!(parse(&uri).sqlite_arg, uri);
    }
    let missing = format!(
        "file:{}/missing.db?modeof={}/mask%2526a",
        dir.display(),
        dir.display()
    );
    assert!(open(&missing)
        .unwrap_err()
        .to_string()
        .contains("disk I/O error"));
}

#[test]
fn r4_fix4_empty_unapplied_driver_values_configure_nothing() {
    let dir = temp_dir("r4-empty-values");
    for name in nd_dbschema::schema::GO_REJECTED_PARAMS {
        let input = format!("{}?{name}=", in_dir(&dir, "empty.db"));
        if *name == "_auth" {
            assert!(open(&input)
                .unwrap_err()
                .to_string()
                .contains("cannot be applied"));
        } else {
            let conn = open(&input).unwrap();
            assert_eq!(state(&conn, name).busy_timeout, 5000);
            assert_eq!(state(&conn, name).journal_mode, "delete");
        }
    }
    let input = format!("{}?_query_only=nonsense", in_dir(&dir, "invalid.db"));
    let error = open(&input).unwrap_err().to_string();
    assert!(error.contains("invalid _query_only"), "{error}");
    assert!(
        !error.contains("cannot be applied"),
        "validation precedes policy rejection"
    );
}

#[test]
fn r4_fix5_encoded_memory_path_accepts_wal_downgrade() {
    let uri = "file:%3Amemory%3A?_journal_mode=wal";
    assert_eq!(parse(uri).sqlite_arg, uri);
    let conn = open(uri).unwrap();
    let got = state(&conn, uri);
    assert_eq!(got.journal_mode, "memory");
    assert_eq!(got.db_file, None);
    conn.execute("CREATE TABLE r4_encoded_memory (id INTEGER)", [])
        .unwrap();
}

#[test]
fn r4_fix6_leading_question_mark_is_a_filename_not_a_query() {
    let target = parse("?_fk=off");
    assert_eq!(target.path, "?_fk=off");
    assert_eq!(target.sqlite_arg, "?_fk=off");
    assert_eq!(target.options.foreign_keys, None);
    assert!(!target.is_memory);
}

#[test]
fn r4_fix7_non_utf8_query_restriction_is_explicit() {
    for query in ["_loc=%FF", "journal_mode=%FF", "modeof=%FF"] {
        let input = format!("/tmp/r4-never-opened.db?{query}");
        let error = open(&input).unwrap_err().to_string();
        assert!(error.contains("not valid UTF-8"), "{error}");
    }
    let source = include_str!("../src/schema.rs");
    let docs = include_str!("../../../../docs/rust-port/subplans/db-runtime.md");
    assert!(source.contains("percent-decoded query names/values must be UTF-8"));
    assert!(docs.contains("percent-decoded query names/values must be UTF-8"));
}

#[test]
fn r4_fix8_and_fix11_policy_documents_the_two_grammars_and_correct_cites() {
    let source = include_str!("../src/schema.rs");
    let docs = include_str!("../../../../docs/rust-port/subplans/db-runtime.md");
    for text in [source, docs] {
        assert!(text.contains("182815-182824"), "authority cite");
        assert!(
            text.contains("182839-182870"),
            "single URI percent-decode cite"
        );
        assert!(!text.contains("182878-182896"), "obsolete authority cite");
        assert!(
            !text.contains("182900-182960"),
            "obsolete percent-decode cite"
        );
        assert!(
            !text.contains("inside double quotes"),
            "unsupported quote grammar"
        );
        assert!(text.contains("two grammars") || text.contains("TWO grammars"));
        assert!(
            text.contains("except `_auth`"),
            "empty-value presence exception"
        );
    }
}

#[test]
fn r4_fix9_panicking_probe_restores_fk_and_preserves_original_payload() {
    for fk in [false, true] {
        let conn = open("file::memory:").unwrap();
        nd_dbschema::schema::set_foreign_keys(&conn, fk, "r4 setup").unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            nd_dbschema::schema::apply_fresh_schema_with(
                &conn,
                Some(&mut |_| {
                    std::panic::panic_any(749usize);
                }),
            )
            .unwrap();
        }))
        .unwrap_err();
        assert_eq!(
            *panic.downcast::<usize>().expect("original payload type"),
            749
        );
        assert_eq!(state(&conn, "after probe panic").foreign_keys, fk);
    }
}

#[test]
fn r4_fix9_failed_restore_preserves_panic_and_names_leaked_state() {
    let conn = open("file::memory:?_fk=on").unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        nd_dbschema::schema::apply_fresh_schema_with(
            &conn,
            Some(&mut |handle| {
                handle.execute_batch("BEGIN").unwrap(); // FK restore is ignored inside a transaction.
                std::panic::panic_any(750usize);
            }),
        )
        .unwrap();
    }))
    .unwrap_err();
    type FailedRestore = (Box<dyn std::any::Any + Send>, Error);
    let failure = panic
        .downcast::<FailedRestore>()
        .expect("panic plus restore error tuple");
    assert_eq!(*failure.0.downcast::<usize>().unwrap(), 750);
    match failure.1 {
        Error::OpenConfiguration { reason, .. } => {
            assert!(reason.contains("foreign_keys=false"), "{reason}");
            assert!(reason.contains("probe panicked"), "{reason}");
        }
        other => panic!("leaked state must be OpenConfiguration: {other}"),
    }
    assert!(!state(&conn, "failed restore").foreign_keys);
    conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn r5_h1_empty_winning_alias_suppresses_every_unsupported_loser() {
    let dir = temp_dir("r5-empty-alias");
    for (canonical, alias) in [
        ("_auto_vacuum", "_vacuum"),
        ("_case_sensitive_like", "_cslike"),
        ("_defer_foreign_keys", "_defer_fk"),
        ("_locking_mode", "_locking"),
        ("_recursive_triggers", "_rt"),
    ] {
        for query in [
            format!("{canonical}=nonsense&{alias}="),
            format!("{alias}=&{canonical}=nonsense"),
            format!("{alias}=&{alias}=nonsense&{canonical}=nonsense"),
        ] {
            let conn = open(&format!("{}?{query}", in_dir(&dir, "empty.db"))).unwrap();
            let got = state(&conn, &query);
            assert_eq!(got.journal_mode, "delete");
            assert_eq!(got.busy_timeout, 5000);
            assert_eq!(got.synchronous, 1);
        }
        for query in [
            format!("{alias}=nonsense&{canonical}="),
            format!("{canonical}=&{alias}=nonsense"),
        ] {
            let error = open(&format!("{}?{query}", in_dir(&dir, "bad.db")))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("invalid {canonical}")),
                "{query}: {error}"
            );
            assert!(!error.contains("cannot be applied"));
        }
    }
    open(&format!(
        "{}?_vacuum=&_auto_vacuum=full",
        in_dir(&dir, "full.db")
    ))
    .unwrap();
}

#[test]
fn r5_h4_uri_decoded_nul_truncates_memory_components_only_for_classification() {
    let dir = temp_dir("r5-uri-nul");
    for uri in [
        format!(
            "file:{}?mode=memory%00extra&_journal_mode=wal",
            in_dir(&dir, "nul.db")
        ),
        "file:%3Amemory%3A%00extra?_journal_mode=wal".to_string(),
    ] {
        let conn = open(&uri).unwrap();
        conn.execute("CREATE TABLE r5_nul (id INTEGER)", [])
            .unwrap();
        assert_eq!(state(&conn, &uri).journal_mode, "memory");
        assert_eq!(state(&conn, &uri).db_file, None);
        assert_eq!(parse(&uri).sqlite_arg, uri);
        assert!(parse(&uri).is_memory);
    }
}

#[test]
fn r5_h5_cache_size_is_a_fifth_validated_open_option() {
    let dir = temp_dir("r5-cache-option");
    for (query, expected) in [
        ("_cache_size=2000", Some(2000)),
        ("_cache_size=-321", Some(-321)),
        ("_cache_size=%2B2000", Some(2000)),
        ("_cache_size=2000&_cache_size=321", Some(2000)),
        ("_cache_size=", None),
    ] {
        let input = format!("{}?{query}", in_dir(&dir, "cache.db"));
        assert_eq!(parse(&input).options.cache_size, expected);
        let conn = open(&input).unwrap();
        if let Some(expected) = expected {
            assert_eq!(state(&conn, query).cache_size, expected);
        }
    }
    for value in [
        "nonsense",
        "1_000",
        "0x10",
        "1.5",
        "+2000",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        let input = format!("{}?_cache_size={value}", in_dir(&dir, "invalid.db"));
        let error = open(&input).unwrap_err().to_string();
        assert!(error.contains("invalid _cache_size: '"), "{value}: {error}");
        assert!(error.contains("expecting an int"), "{value}: {error}");
    }
    let conn = open("file::memory:").unwrap();
    conn.execute_batch("PRAGMA cache_size=1777").unwrap();
    nd_dbschema::schema::apply_open_options(
        &conn,
        DbOpenOptions::driver_defaults(),
        true,
        "unset cache",
    )
    .unwrap();
    assert_eq!(
        state(&conn, "unset cache").cache_size,
        1777,
        "None emits no cache pragma"
    );
}

#[test]
fn r5_h5_policy_cites_actual_option_readers_and_bundled_build() {
    let source = include_str!("../src/schema.rs");
    let docs = include_str!("../../../../docs/rust-port/subplans/db-runtime.md");
    for cite in ["1917-1921", "182763", "51816", "223262"] {
        assert!(source.contains(cite), "source missing {cite}");
        assert!(docs.contains(cite), "D-OPEN missing {cite}");
    }
    assert!(!source.contains("`openDirectory`"));
    assert!(!docs.contains("**non-bundled**"));
    assert!(docs.contains("bare `synchronous` is ignored"));
}

fn assert_cache_size_resolution(label: &str, requested: i64, resolved: i64) {
    let dir = temp_dir(&format!("r6-cache-resolution-{label}"));
    let input = format!("{}?_cache_size={requested}", in_dir(&dir, "cache.db"));
    assert_eq!(parse(&input).options.cache_size, Some(requested));
    let conn = open(&input).unwrap_or_else(|error| {
        panic!("cache_size requested {requested}, expected resolved {resolved}: {error}")
    });
    assert_eq!(
        state(&conn, &input).cache_size,
        resolved,
        "request {requested}"
    );
}

#[test]
fn r6_f1_cache_size_int32_endpoints_and_small_values_are_exact() {
    for requested in [i64::from(i32::MIN), i64::from(i32::MAX), 0, -321, 2000] {
        assert_cache_size_resolution(&requested.to_string(), requested, requested);
    }
}

macro_rules! cache_size_overflow_case {
    ($name:ident, $requested:expr) => {
        #[test]
        fn $name() {
            assert_cache_size_resolution(stringify!($name), $requested, 0);
        }
    };
}

cache_size_overflow_case!(r6_f1_cache_size_above_i32, 2147483648);
cache_size_overflow_case!(r6_f1_cache_size_below_i32, -2147483649);
cache_size_overflow_case!(r6_f1_cache_size_not_low_word_truncation, 4294967297);
cache_size_overflow_case!(r6_f1_cache_size_i64_max, i64::MAX);
cache_size_overflow_case!(r6_f1_cache_size_i64_min, i64::MIN);
