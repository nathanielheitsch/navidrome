//! #04 — DB open/init plus the single fresh-schema migration.
//!
//! Go side: `db/db.go`. The custom sqlite driver's `ConnectHook` (`db/db.go:44-51`)
//! registers `SEEDEDRAND` (`utils/hasher.HashFunc()`) and the `NATSORT` collation
//! (`utils/natural.CompareFold`) **per connection** — contract §4 of
//! `docs/rust-port/schema-contract.md` requires re-registering both on every Rust
//! connection. `:memory:` is rewritten (`db/db.go:54-59`); the DDL pass runs with
//! foreign keys off then back on (`db/db.go:85-89`). Per contract §0 there is ONE
//! fresh migration ([`crate::schema_ddl`]) — no goose history, no upgrade path.

use crate::schema_ddl::{FRESH_STATEMENTS, TABLE_NAMES};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::{Connection, Error as RsError, OpenFlags};

use std::cmp::Ordering;

use nd_utils::hasher::Hasher;
use nd_utils::natural::compare_fold;

/// Contract §4: collation name Go registers (`db/db.go:24`).
pub const NATSORT_COLLATION: &str = "NATSORT";

/// Go-side DSN rewrite for `:memory:` (`db/db.go:54-59`).
pub const MEMORY_PATH: &str = ":memory:";
/// Shared-cache memory URI Go rewrites `:memory:` to (`db/db.go:54-59`).
pub const MEMORY_DSN: &str = "file::memory:?cache=shared&_foreign_keys=on";

/// SELECT exercising the SEEDEDRAND scalar function (tests only).
pub const SEEDEDRAND_SQL: &str = "SELECT SEEDEDRAND('album-1', 'song-1')";

/// Failures opening or applying the migration.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failed to open or process the DB (path kept for diagnostics).
    #[error("cannot open db '{path}': {cause}")]
    Failed {
        /// Path or DSN of the DB that failed.
        path: String,
        /// Underlying rusqlite error.
        cause: RsError,
    },
}

/// Opens the DB (URI-capable flags so the `:memory:` rewrite parses as a sqlite
/// URI) and registers the per-connection extensions. Connections are single-owner
/// values threaded by the caller (explicit DI per the #S1 freeze, replacing Go's
/// process-global singleton `Db()`).
pub fn open(path: &str) -> Result<Connection, Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    match Connection::open_with_flags(effective_path(path), flags) {
        Ok(conn) => {
            if let Err(e) = register_extensions(&conn) {
                Err(Error::Failed {
                    path: path.to_string(),
                    cause: e,
                })
            } else {
                Ok(conn)
            }
        }
        Err(e) => Err(Error::Failed {
            path: path.to_string(),
            cause: e,
        }),
    }
}

/// `:memory:` → shared-cache memory URI (`db/db.go:54-59`); other paths verbatim
/// (they may already be DSNs like `DefaultDbPath` from `consts/consts.go:14`).
pub fn effective_path(path: &str) -> String {
    if path == MEMORY_PATH {
        MEMORY_DSN.to_string()
    } else {
        path.to_string()
    }
}

/// Go's `ConnectHook` equivalent (contract §4: per connection). SEEDEDRAND keeps
/// a connection-scoped `Hasher` in the closure: like Go's shared `HashFunc()`
/// instance, an unseeded id reseeds once and stays cached (so ORDER BY is
/// permutation-stable within a query). Non-deterministic across connections —
/// Go's `RegisterFunc(..., false)` (db/db.go:46).
pub fn register_extensions(conn: &Connection) -> Result<(), RsError> {
    conn.create_collation(NATSORT_COLLATION, move |a, b| {
        to_ordering(compare_fold(a, b))
    })?;
    conn.create_scalar_function("SEEDEDRAND", 2, FunctionFlags::SQLITE_UTF8, {
        let h = Hasher::new();
        move |ctx: &Context| -> Result<i64, RsError> {
            let id: String = ctx.get(0)?;
            let value: String = ctx.get(1)?;
            let fn_ = h.hash_func();
            Ok(fn_(&id, &value) as i64)
        }
    })
}

/// Go `CompareFold` returns negative/zero/positive; SQLite collations want
/// `std::cmp::Ordering`.
pub fn to_ordering(v: i32) -> Ordering {
    if v < 0 {
        Ordering::Less
    } else if v > 0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Applies the single fresh migration with FK pragma off/on around it
/// (`db/db.go:85-89`). Idempotent: every CREATE carries `IF NOT EXISTS`, and the
/// statements run individually (rusqlite batch splitting would corrupt trigger
/// bodies, which contain `;`).
pub fn apply_fresh_schema(conn: &Connection) -> Result<(), Error> {
    if let Err(e) = conn.execute("PRAGMA foreign_keys=off", []) {
        return Err(Error::Failed {
            path: "fresh_schema".to_string(),
            cause: e,
        });
    }
    for sql in FRESH_STATEMENTS {
        if let Err(e) = conn.execute(sql, []) {
            let _ = conn.execute("PRAGMA foreign_keys=on", []);
            return Err(Error::Failed {
                path: "fresh_schema".to_string(),
                cause: e,
            });
        }
    }
    if let Err(e) = conn.execute("PRAGMA foreign_keys=on", []) {
        return Err(Error::Failed {
            path: "fresh_schema".to_string(),
            cause: e,
        });
    }
    Ok(())
}

/// Table names on a connection (sqlite internals excluded; FTS5 virtual tables
/// included by name) — for tests and #05 sanity checks.
pub fn schema_tables(conn: &Connection) -> Result<Vec<String>, RsError> {
    let sql =
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND substr(name,1,7) <> 'sqlite_'";
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    let mut names: Vec<String> = Vec::new();
    loop {
        let next = rows.next();
        let row = match next {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(e) => return Err(e),
        };
        match row {
            Some(r) => {
                let name: String = r.get(0)?;
                names.push(name);
            }
            None => break,
        }
    }
    Ok(names)
}

/// Sorted fresh table names (the golden from `schema_ddl`).
pub fn golden_sorted_table_names() -> Vec<String> {
    let names: Vec<String> = TABLE_NAMES.iter().map(|name| name.to_string()).collect();
    let mut sorted = names;
    sorted.sort();
    sorted
}
