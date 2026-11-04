//! #05 probe — the FTS5 virtual-table machinery and SQLite's JSON path function
//! both work on a connection borrowed from the pool.
//!
//! The borrowed connection is the port of Go's shared `sql.DB` handle every
//! lazy repository accessor reads through (`persistence.go:17-40` `SQLStore`),
//! bounded by `db.SetMaxOpenConns(conf.MaxOpenConns())`
//! (`conf/configuration.go:1213-1216`). FTS5 tables are always part of the fresh
//! schema (`schema-contract.md` §1), so the virtual-table machinery must be
//! compiled into the bundled sqlite behind a pooled lease, never via the CLI.

use nd_db::pool::{Lease, Pool, PoolConfig};
use rusqlite::Error;

/// Act helper: run one statement with no bindings on a leased connection.
/// Arrange precondition asserted by the call site: the statement is valid SQL for
/// a connection opened through `nd_dbschema::schema::open`.
fn exec(conn: &Lease<'_>, sql: &str) -> Result<(), Error> {
    conn.conn().execute(sql, []).map(|_used| ())
}

/// Act helper: how many rows a single-column SELECT yields.
fn count_rows(conn: &Lease<'_>, sql: &str) -> usize {
    let mut stmt = conn.conn().prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut seen = 0;
    loop {
        let next = rows.next();
        let row = match next {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => panic!("{e}"),
        };
        let _row = row;
        seen += 1;
    }
    seen
}

/// Read column 0 of a SELECT as a string.
fn col0(conn: &Lease<'_>, sql: &str) -> String {
    let mut stmt = conn.conn().prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let next = rows.next();
    let row = match next {
        Ok(Some(r)) => r,
        Ok(None) => panic!("json_type probe returned no row"),
        Err(e) => panic!("{e}"),
    };
    let value: String = row.get(0).unwrap();
    value
}

#[test]
fn fts5_virtual_table_binds_on_a_pooled_connection() {
    // Arrange: private FILE-backed db path, previous run removed first. Never
    // `:memory:`: `nd-dbschema/src/schema.rs:80-88` rewrites it to a shared-cache
    // DSN that aliases instances across concurrently running tests.
    let path = "/tmp/nd-db-fts-json-probe.sqlite";
    let removed = match std::fs::remove_file(path) {
        Ok(_gone) => true,
        Err(_missing) => false,
    };
    let _removed_or_absent = removed;

    // Arrange: pool pinned to two connections so the leased/idle split is exact.
    let config = PoolConfig::with_size(path, 2);
    let pool = Pool::new(&config).unwrap();
    // Assert
    assert_eq!(pool.size(), 2, "the probe pins the pool to two connections");
    assert_eq!(
        pool.idle_count().unwrap(),
        2,
        "a fresh pool starts with size idle connections"
    );

    // Act: one leased pooled connection.
    let lease = pool.checkout().unwrap();

    // Act + Assert: the CREATE VIRTUAL TABLE binds (a bind failure is a rusqlite
    // Error on this connection, not a silent no-op).
    let create_err = match exec(
        &lease,
        "CREATE VIRTUAL TABLE probe_fts USING fts5(probe_text)",
    ) {
        Ok(_bound) => None as Option<Error>,
        Err(e) => Some(e),
    };
    let bound = match create_err {
        None => true,
        Some(_fts_unavailable) => false,
    };
    assert!(bound, "fts5 virtual table binds on a pooled connection");

    // Act: one INSERT, then a MATCH query against the same table.
    let insert_err = match exec(
        &lease,
        "INSERT INTO probe_fts (probe_text) VALUES ('quantum leap transit')",
    ) {
        Ok(_stored) => None as Option<Error>,
        Err(e) => Some(e),
    };
    let inserted = match insert_err {
        None => true,
        Some(_write_err) => false,
    };
    assert!(inserted, "the probe row is stored in the fts5 table");

    let matched = count_rows(
        &lease,
        "SELECT probe_text FROM probe_fts WHERE probe_text MATCH 'quantum'",
    );
    // Assert
    assert_eq!(
        matched, 1,
        "the MATCH query finds the inserted row exactly once"
    );

    // Act: SQLite JSON path function on the same connection.
    let json_sql = "SELECT json_type('{\"a\":1}', '$.a')".to_string();
    let json_kind = col0(&lease, &json_sql);

    // Assert
    assert_eq!(
        json_kind, "integer",
        "json_type reports the integer type of $.a"
    );

    // Assert: the lease is still live; Drop returns the connection to the pool.
    assert_eq!(
        pool.idle_count().unwrap(),
        1,
        "the leased connection is not idle while the lease is live"
    );
}
