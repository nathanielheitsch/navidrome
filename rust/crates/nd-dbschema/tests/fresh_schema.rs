//! #04 golden: table set matches the fresh contract (schema-contract.md §1).
//!
//! Golden derived offline by replaying the 96 goose migrations + 23 late-window
//! .sql up-sections with sqlite3 3.54, then applying the fresh deltas (contract
//! §1.4/§1.5): genre tables and the `search` table DROPPED, `plugin` DROPPED,
//! `goose_db_version` DROPPED, FTS5 tables ALWAYS created. Fixtures must match
//! that golden — never adjust to what Rust does.

use nd_dbschema::schema::{
    apply_fresh_schema, apply_fresh_schema_with, open, schema_tables, NATSORT_COLLATION,
    SEEDEDRAND_SQL,
};
use nd_dbschema::schema_ddl::TABLE_NAMES;
use rusqlite::Connection;

/// Golden = 31 ordinary tables + 3 FTS5 virtual tables (34 logical).
fn golden_sorted() -> Vec<String> {
    let names: Vec<String> = TABLE_NAMES.iter().map(|name| name.to_string()).collect();
    let mut sorted = names;
    sorted.push("album_fts".to_string());
    sorted.push("artist_fts".to_string());
    sorted.push("media_file_fts".to_string());
    sorted.sort();
    sorted
}

/// sqlite_schema table names minus FTS5 auxiliary tables.
fn logical_tables(conn: &Connection) -> Vec<String> {
    let all = schema_tables(conn).unwrap();
    let kept = all
        .iter()
        .filter(|name| {
            !name.ends_with("_config")
                && !name.ends_with("_data")
                && !name.ends_with("_docsize")
                && !name.ends_with("_idx")
        })
        .map(|name| name.to_string())
        .collect();
    kept
}

#[test]
fn fresh_table_set_matches_golden() {
    let conn = open(":memory:").unwrap();
    apply_fresh_schema(&conn).unwrap();

    let names = logical_tables(&conn);
    let want_sorted = golden_sorted();
    let mut got_sorted = names;
    got_sorted.sort();
    assert_eq!(got_sorted, want_sorted);
}

#[test]
fn goose_and_plugin_tables_are_dropped() {
    let conn = open(":memory:").unwrap();
    apply_fresh_schema(&conn).unwrap();
    let names = logical_tables(&conn);

    for banned in ["goose_db_version", "plugin", "search"].to_vec() {
        if names.iter().find(|name| name == &banned).is_some() {
            panic!("dropped table {banned} present");
        }
    }
}

#[test]
fn fts5_tables_are_created() {
    let conn = open(":memory:").unwrap();
    apply_fresh_schema(&conn).unwrap();
    let names = logical_tables(&conn);

    let hits = names.iter().filter(|name| name.contains("_fts")).count();
    assert_eq!(hits, 3);
}

#[test]
fn fts5_feeder_columns_survive_writes() {
    // Go `20260220173400_add_fts5_search.go:28-46`: media_file and album carry
    // `search_participants`, all three base tables carry `search_normalized` —
    // the FTS triggers bind against them on the first INSERT.
    let conn = open(":memory:").unwrap();
    apply_fresh_schema(&conn).unwrap();
    let inserts = [
        "INSERT INTO library ( id, name, path ) VALUES ( 1, 'l', '/l' )",
        "INSERT INTO media_file ( id, search_participants, search_normalized ) VALUES ( 'x', '', '' )",
        "INSERT INTO album ( id, name, search_participants, search_normalized ) VALUES ( 'a', 'n', '', '' )",
        "INSERT INTO artist ( id, name, search_normalized ) VALUES ( 'r', 'n', '' )",
    ];
    for sql in inserts.iter().copied() {
        if let Err(e) = conn.execute(sql, []) {
            panic!("feeder column write failed: {e}")
        }
    }
}

#[test]
fn schema_is_idempotent() {
    let conn = open(":memory:").unwrap();
    apply_fresh_schema(&conn).unwrap();
    apply_fresh_schema(&conn).unwrap();
    let names = logical_tables(&conn);
    assert_eq!(names.len(), 34);
}

#[test]
fn memory_path_is_rewritten() {
    let conn = open(":memory:");
    assert!(
        conn.is_ok(),
        "db/db.go:54-59: :memory: becomes the memory DSN"
    );
}

#[test]
fn natsort_collation_is_registered() {
    // Private memory DB: shared-cache :memory: serializes against parallel tests.
    let conn = open("file:nd_collation?mode=memory").unwrap();
    apply_fresh_schema(&conn).unwrap();

    conn.execute("CREATE TABLE t (v TEXT)", []).unwrap();
    for v in ["image20", "image2", "image1", "image10", "IMAGE20"].to_vec() {
        let vals_one: Vec<String> = Vec::new();
        let mut one = vals_one;
        one.push(v.to_string());
        conn.execute(
            "INSERT INTO t (v) VALUES (?)",
            rusqlite::params_from_iter(one),
        )
        .unwrap();
    }
    let sql = format!("SELECT v FROM t ORDER BY v COLLATE {NATSORT_COLLATION}");
    let vals = read_col0(&conn, &sql);

    // Go: natural sort, case-insensitive (utils/natural.go CompareFold): the
    // digit run 20 == 20, and IMAGE20/image20 fold-equal (compare_fold == 0),
    // so SQLite's ORDER BY keeps them as a tie pair in the natural order.
    assert_eq!(vals.len(), 5);

    if !(vals[0] == "image1" && vals[1] == "image2" && vals[2] == "image10") {
        panic!("natural order wrong: {vals:?}");
    }
    if vals.iter().find(|v| *v == "IMAGE20").is_none() {
        panic!("missing IMAGE20: {vals:?}");
    }
    if vals.iter().find(|v| *v == "IMAGE20").is_some()
        && vals.iter().find(|v| *v == "image20").is_none()
    {
        panic!("missing image20: {vals:?}");
    }
}

#[test]
fn seedrand_function_is_registered() {
    let conn = open("file:nd_seedrand?mode=memory").unwrap();
    apply_fresh_schema(&conn).unwrap();

    let sql = SEEDEDRAND_SQL;
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let next = rows.next();
    let row = match next {
        Ok(Some(r)) => r,
        Ok(None) => panic!("no row"),
        Err(e) => panic!("{e}"),
    };
    let n: i64 = row.get(0).unwrap();
    let _n = n;
}

/// Helper: SELECT into a column-0 vector (as strings).
fn read_col0(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut vals: Vec<String> = Vec::new();
    loop {
        let next = rows.next();
        let row = match next {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => panic!("{e}"),
        };
        let v: String = row.get(0).unwrap();
        vals.push(v);
    }
    vals
}

// --------------------------------------------------------------------------------
// DB-03 round 3 (G9) — the FK error contract of `apply_fresh_schema`.
//
// rusqlite exposes an authorizer only under its `hooks` feature, which this
// workspace does not enable (nd-dbschema/Cargo.toml has no rusqlite features), so
// the five scenarios a reviewer probed with an authorizer are reproduced here with
// mechanisms SQLite really has, through `apply_fresh_schema_with`'s probe:
//
//   * A `PRAGMA foreign_keys` inside an open transaction is SILENTLY IGNORED — the
//     pragma's mask is cleared when `db->autoCommit == 0` (`sqlite3.c:139857-139861`)
//     — measured: BEGIN, then `PRAGMA foreign_keys=1` still reads 0. A restore that
//     cannot take effect is exactly a denied restore.
//   * `PRAGMA max_page_count=1` makes the next CREATE fail with
//     `database or disk is full` — a denied DDL statement.
// --------------------------------------------------------------------------------

/// FK state off a live handle.
fn fk(conn: &Connection) -> bool {
    conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .map(|value| value != 0)
        .unwrap_or_else(|cause| panic!("PRAGMA foreign_keys: {cause}"))
}

fn probe(begin: bool, exhaust: bool) -> impl FnMut(&Connection) + 'static {
    move |conn: &Connection| {
        // Runs after a DDL statement has executed, on the same handle.
        if begin {
            // Second BEGIN would error inside the transaction; one is enough.
            let _ = conn.execute("BEGIN", []);
        }
        if exhaust {
            let _ = conn.execute("PRAGMA max_page_count=1", []);
        }
    }
}

#[test]
fn a_restore_that_cannot_take_effect_reports_open_configuration_and_names_the_leak() {
    // Scenario "restore denied on the success path". The DDL completes, then the
    // restore is swallowed by the open transaction the probe opened, so the handle is
    // genuinely left at foreign_keys = off. This used to surface as `Failed`; the
    // contract now requires `OpenConfiguration` naming the leaked state.
    let dir = temp_dir("fk-restore-denied");
    let conn = open_in(&dir, "restore.db");
    assert!(fk(&conn), "this build's FK default is on");

    let error = apply_fresh_schema_with(&conn, Some(&mut probe(true, false)))
        .expect_err("a restore SQLite cannot apply must not report Ok");
    match error {
        nd_dbschema::schema::Error::OpenConfiguration { reason, .. } => {
            assert!(
                reason.contains("foreign_keys=false"),
                "the reason must name the state the handle is left at: {reason}"
            );
            assert!(
                reason.contains("could not be restored"),
                "the reason must say what failed: {reason}"
            );
        }
        other => panic!("expected OpenConfiguration, got {other}"),
    }
    assert!(
        !fk(&conn),
        "the leak is real: the handle sits at foreign_keys = off"
    );
}

#[test]
fn a_denied_ddl_statement_still_reports_failed_with_fk_restored() {
    // Scenario "DDL denied": the failure is the DDL, the restore succeeds, so the
    // handle comes back enforced and the error stays `Failed`.
    let dir = temp_dir("fk-ddl-denied");
    let conn = open_in(&dir, "ddl.db");

    let error = apply_fresh_schema_with(&conn, Some(&mut probe(false, true)))
        .expect_err("a DDL statement that cannot run must not report Ok");
    match error {
        nd_dbschema::schema::Error::Failed { cause, .. } => {
            let text = cause.to_string();
            assert!(
                text.contains("disk is full"),
                "the DDL cause must be SQLite's own: {text}"
            );
        }
        other => panic!("expected Failed when the restore succeeded, got {other}"),
    }
    assert!(fk(&conn), "the restore succeeded, so enforcement is back");
}

#[test]
fn a_denied_ddl_and_a_denied_restore_report_open_configuration_with_the_leak() {
    // Scenario "DDL + restore denied": both failures must be reported together, and
    // the message must name the FK state the handle is left at.
    let dir = temp_dir("fk-ddl-and-restore-denied");
    let conn = open_in(&dir, "both.db");

    let error = apply_fresh_schema_with(&conn, Some(&mut probe(true, true)))
        .expect_err("neither the DDL nor the restore can succeed");
    match error {
        nd_dbschema::schema::Error::OpenConfiguration { reason, .. } => {
            assert!(
                reason.contains("DDL statement failed"),
                "the DDL failure must be reported: {reason}"
            );
            assert!(
                reason.contains("foreign_keys=false"),
                "the leaked state must be named: {reason}"
            );
        }
        other => panic!("expected OpenConfiguration, got {other}"),
    }
    assert!(
        !fk(&conn),
        "the handle is left unenforced, as the message says"
    );
}

#[test]
fn a_disable_that_cannot_take_effect_is_reported_without_leaving_fk_off() {
    // Scenario "disable + restore denied": called on a handle already inside a
    // transaction, the DISABLE pragma is swallowed too, so nothing was ever turned
    // off. The failure is `OpenConfiguration` (the read-back mismatch), and FK stays
    // exactly where the caller had it.
    let dir = temp_dir("fk-disable-denied");
    let conn = open_in(&dir, "disable.db");
    conn.execute("BEGIN", [])
        .unwrap_or_else(|cause| panic!("BEGIN: {cause}"));
    assert!(fk(&conn), "FK is on before the migration attempt");

    let error =
        apply_fresh_schema(&conn).expect_err("the disable cannot take effect inside a transaction");
    match error {
        nd_dbschema::schema::Error::OpenConfiguration { path, reason } => {
            assert!(
                path.contains("disable foreign_keys"),
                "the error names the step that failed: {path}"
            );
            assert!(
                reason.contains("requested false but SQLite reports true"),
                "the reason says the disable did not take effect: {reason}"
            );
        }
        other => panic!("expected OpenConfiguration, got {other}"),
    }
    assert!(fk(&conn), "the handle never lost enforcement");
    conn.execute("COMMIT", []).ok();
}

#[test]
fn a_completed_migration_leaves_the_handles_configured_fk_state() {
    // The control: with nothing interfering, the toggle happens mid-pass and the
    // handle returns to the state it was configured with — including a handle that
    // was deliberately configured FK-off, which Go's unconditional
    // `foreign_keys=on` (`db/db.go:85-91`) would have overwritten.
    let dir = temp_dir("fk-control");
    require_wal_support(&dir);

    let conn_on = open_dsn(&format!("{}?_fk=on", in_dir(&dir, "on.db")));
    let mid = std::cell::Cell::new(None);
    apply_fresh_schema_with(&conn_on, Some(&mut mid_fk(&mid)))
        .unwrap_or_else(|cause| panic!("migration on an FK-on handle: {cause}"));
    assert_eq!(mid.get(), Some(false), "the DDL pass runs with FK off");
    assert!(fk(&conn_on), "FK-on is restored");

    let conn_off = open_dsn(&format!("{}?_fk=off", in_dir(&dir, "off.db")));
    assert!(!fk(&conn_off), "the DSN asked for FK off");
    let mid_off = std::cell::Cell::new(None);
    apply_fresh_schema_with(&conn_off, Some(&mut mid_fk(&mid_off)))
        .unwrap_or_else(|cause| panic!("migration on an FK-off handle: {cause}"));
    assert!(
        !fk(&conn_off),
        "an explicitly configured FK-off must not regain enforcement"
    );
}

/// First FK read during a DDL pass; `None` until a statement has run.
fn mid_fk(cell: &std::cell::Cell<Option<bool>>) -> impl FnMut(&Connection) + '_ {
    move |conn: &Connection| {
        if cell.get().is_none() {
            cell.set(Some(fk(conn)));
        }
    }
}

/// Open `dir/<name>` with the default options.
fn open_in(dir: &std::path::Path, name: &str) -> Connection {
    open_dsn(&in_dir(dir, name))
}

/// Open an already-assembled input, failing loudly on any rejection.
fn open_dsn(input: &str) -> Connection {
    match open(input) {
        Ok(conn) => conn,
        Err(cause) => panic!("open {input}: {cause}"),
    }
}

fn in_dir(dir: &std::path::Path, name: &str) -> String {
    let joined = dir.join(name);
    match joined.to_str() {
        Some(text) => text.to_string(),
        None => panic!("path is not valid UTF-8: {}", joined.display()),
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from("/tmp/nd-dbschema-fresh-schema").join(tag);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => (),
        Err(_absent) => (),
    }
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|cause| panic!("create {}: {cause}", dir.display()));
    dir
}

/// Fail loudly — never weaken an assertion — if this filesystem cannot do WAL.
fn require_wal_support(dir: &std::path::Path) {
    match nd_dbschema::schema::open_with_options(
        &in_dir(dir, "wal-probe.db"),
        Some(nd_dbschema::schema::DbOpenOptions {
            journal_mode: Some(nd_dbschema::schema::JournalMode::Wal),
            ..nd_dbschema::schema::DbOpenOptions::driver_defaults()
        }),
    ) {
        Ok(conn) => {
            let journal: String = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap_or_else(|cause| panic!("journal probe: {cause}"));
            drop(conn);
            assert_eq!(
                journal,
                "wal",
                "STOP AND REPORT: WAL is unavailable on {}: journal_mode reads {journal}. \
                 The DB-03 assertions must not be weakened.",
                dir.display()
            );
        }
        Err(cause) => panic!(
            "STOP AND REPORT: WAL pragma failed on {}: {cause}",
            dir.display()
        ),
    }
}
