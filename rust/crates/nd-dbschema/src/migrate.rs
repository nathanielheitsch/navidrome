//! #04 B1 — migration runner. Contract §0 (`docs/rust-port/schema-contract.md`)
//! freezes exactly ONE fresh migration: the 138-file Go goose history
//! (`db/migrations/**`) is NEVER replayed and there is no upgrade path, so the
//! "runner" is a marker guard around [`apply_fresh_schema`], not a step engine.
//!
//! Go side: goose runs and then `FullScanAfterMigration` is armed (`db/db.go:91-95`);
//! the marker probe models `db/repair.go:116-138` (`requireFTSMigration`, which probes
//! the marker table separately because querying a missing table aborts at prepare
//! time). The marker table is a RUNTIME artifact only — the fresh DDL never carries
//! it, so the golden table set asserted in `tests/fresh_schema.rs` stays at 34.

use rusqlite::{params_from_iter, Connection, Error as RsError};

use nd_conf::consts::FULL_SCAN_AFTER_MIGRATION_FLAG_KEY;

use crate::schema::{apply_fresh_schema, open, Error};
use crate::schema_ddl::FRESH_STATEMENTS;

/// Pseudo goose version for the single fresh migration. Real goose ids are the 14-digit
/// migration filenames under `db/migrations/**`, so 1 can never collide with one.
pub const FRESH_MIGRATION_VERSION: i64 = 1;

/// Statement count of the fresh DDL (31 tables + 3 FTS5 virtual + 9 triggers +
/// 80 secondary indexes).
pub const FRESH_STATEMENT_COUNT: usize = FRESH_STATEMENTS.len();

/// Opens the DB, applies the FRESH schema only when the marker is absent, and returns the
/// recorded migration ids. Opening the same DB twice applies the DDL once and reports
/// `[1]` on both calls.
pub fn open_db(path: &str) -> Result<(Connection, Vec<i64>), Error> {
    let conn = open(path)?;
    let versions = ensure_fresh_migration(&conn)?;
    Ok((conn, versions))
}

/// Apply-once guard: marker present => no DDL statement re-runs.
pub fn ensure_fresh_migration(conn: &Connection) -> Result<Vec<i64>, Error> {
    if already_migrated(conn) {
        return migration_versions(conn);
    }
    apply_fresh_schema(conn)?;
    record_fresh_migration(conn)?;
    migration_versions(conn)
}

/// Migration ids recorded on this connection (empty => the fresh schema never ran).
pub fn migration_versions(conn: &Connection) -> Result<Vec<i64>, Error> {
    read_i64s(conn, "SELECT version_id FROM goose_db_version")
}

/// Marker probe: a query against a missing table fails at prepare time, which is why
/// `db/repair.go:120-128` probes the table separately.
pub fn already_migrated(conn: &Connection) -> bool {
    let versions = migration_versions(conn);
    match versions {
        Err(_) => false,
        Ok(vs) => vs.contains(&FRESH_MIGRATION_VERSION),
    }
}

/// Marker row plus the post-migration full-scan flag (`db/db.go:91-95`).
pub fn record_fresh_migration(conn: &Connection) -> Result<(), Error> {
    let create = "CREATE TABLE IF NOT EXISTS goose_db_version ( version_id integer NOT NULL, applied_at TEXT )";
    conn.execute(create, []).map_err(migration_error)?;
    if !already_migrated(conn) {
        let insert = "INSERT INTO goose_db_version (version_id) VALUES (?)";
        let ids = [FRESH_MIGRATION_VERSION];
        conn.execute(insert, params_from_iter(ids))
            .map_err(migration_error)?;
    }
    put_property(
        conn,
        FULL_SCAN_AFTER_MIGRATION_FLAG_KEY.to_string(),
        "1".to_string(),
    )
}

/// SELECT column 0 as i64s.
fn read_i64s(conn: &Connection, sql: &str) -> Result<Vec<i64>, Error> {
    let mut stmt = conn.prepare(sql).map_err(migration_error)?;
    let mut rows = stmt.query([]).map_err(migration_error)?;
    let mut vals = Vec::<i64>::new();
    loop {
        let next = rows.next();
        let row = match next {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(cause) => return Err(migration_error(cause)),
        };
        match row {
            Some(r) => {
                let cell = r.get(0);
                match cell {
                    Err(cause) => return Err(migration_error(cause)),
                    Ok(v) => vals.push(v),
                }
            }
            None => return Ok(vals),
        }
    }
}

/// `db/properties.go` / `db/optimize.go:261-265`: replaceable key in the fresh-schema
/// `property` table (Go's `insert ... on conflict(id) do update set value=excluded.value`).
pub fn put_property(conn: &Connection, key: String, value: String) -> Result<(), Error> {
    let sql = "INSERT INTO property(id, value) VALUES(?, ?) ON CONFLICT(id) DO UPDATE SET value=excluded.value";
    let args = [key, value];
    conn.execute(sql, params_from_iter(args))
        .map_err(property_error)?;
    Ok(())
}

/// `getProperty` of `db/optimize.go:267-274` (absent key => `None`).
pub fn get_property(conn: &Connection, key: String) -> Result<Option<String>, Error> {
    let sql = "SELECT value FROM property WHERE id = ?";
    let prepared = conn.prepare(sql);
    let mut stmt = match prepared {
        Err(cause) => return Err(property_error(cause)),
        Ok(stmt) => stmt,
    };
    let args = vec![key];
    let queried = stmt.query(params_from_iter(args));
    let mut rows = match queried {
        Err(cause) => return Err(property_error(cause)),
        Ok(rows) => rows,
    };
    let next = rows.next();
    let row = match next {
        Ok(Some(r)) => Some(r),
        Ok(None) => None,
        Err(cause) => return Err(property_error(cause)),
    };
    match row {
        Some(r) => {
            let cell = r.get(0);
            match cell {
                Err(cause) => Err(property_error(cause)),
                Ok(v) => Ok(Some(v)),
            }
        }
        None => Ok(None),
    }
}

fn migration_error(cause: RsError) -> Error {
    Error::Failed {
        path: "migrations".to_string(),
        cause,
    }
}

fn property_error(cause: RsError) -> Error {
    Error::Failed {
        path: "property".to_string(),
        cause,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::migrate::{
        already_migrated, ensure_fresh_migration, get_property, migration_versions, open_db,
    };
    use nd_conf::consts::FULL_SCAN_AFTER_MIGRATION_FLAG_KEY;

    #[test]
    fn open_db_is_idempotent() {
        // Arrange: a file DB opened twice (shared :memory: cache would alias).
        let path = "/tmp/nd-dbschema-open-db.sqlite";
        let _ = fs::remove_file(path);

        // Act
        let (_conn, first) = open_db(path).unwrap();
        let (second_conn, second) = open_db(path).unwrap();

        // Assert: DDL applied once; both connections report the single fresh
        // migration, marker set, and the full-scan property armed.
        assert_eq!(first, [1_i64]);
        assert_eq!(second, [1_i64]);
        assert!(already_migrated(&second_conn));
        let flag =
            get_property(&second_conn, FULL_SCAN_AFTER_MIGRATION_FLAG_KEY.to_string()).unwrap();
        assert_eq!(flag, Some("1".to_string()));
    }

    #[test]
    fn ensure_on_migrated_connection_is_noop() {
        // Arrange
        let path = "/tmp/nd-dbschema-ensure-noop.sqlite";
        let _ = fs::remove_file(path);
        let (conn, _first) = open_db(path).unwrap();

        // Act: second ensure with the marker already set.
        let again = ensure_fresh_migration(&conn);

        // Assert: still exactly one recorded id, no duplicate rows.
        assert_eq!(again.unwrap(), [1_i64]);
        assert_eq!(migration_versions(&conn).unwrap(), [1_i64]);
    }
}
