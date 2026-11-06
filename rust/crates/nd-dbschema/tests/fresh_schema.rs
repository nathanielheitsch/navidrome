//! #04 golden: table set matches the fresh contract (schema-contract.md §1).
//!
//! Golden derived offline by replaying the 96 goose migrations + 23 late-window
//! .sql up-sections with sqlite3 3.54, then applying the fresh deltas (contract
//! §1.4/§1.5): genre tables and the `search` table DROPPED, `plugin` DROPPED,
//! `goose_db_version` DROPPED, FTS5 tables ALWAYS created. Fixtures must match
//! that golden — never adjust to what Rust does.

use nd_dbschema::schema::{
    apply_fresh_schema, open, schema_tables, NATSORT_COLLATION, SEEDEDRAND_SQL,
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
