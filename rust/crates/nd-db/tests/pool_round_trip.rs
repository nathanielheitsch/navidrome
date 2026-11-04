//! #05 golden — pool round trip: every lease is handed out and taken back, and
//! the `SEEDEDRAND` scalar / `NATSORT` collation are live on each pooled connection.
//!
//! Go port target: the single shared `sql.DB` handle every lazy repository
//! accessor reads through (`persistence.go:17-40` `SQLStore`), whose connection
//! bound is `db.SetMaxOpenConns(conf.MaxOpenConns())` with
//! `MaxOpenConns() = max(4, runtime.NumCPU())` (`conf/configuration.go:1213-1216`).
//! `rusqlite::Connection` is `Send` but NOT `Sync`, so the pool hands each
//! [`Lease`] its own connection and this test exercises them on ONE thread.

use std::fs::{exists, remove_file};

use nd_db::pool::{pool_size, Lease, Pool, PoolConfig, MIN_POOL_SIZE};
use nd_dbschema::schema::{NATSORT_COLLATION, SEEDEDRAND_SQL};
use rusqlite::Connection;

/// Arrange helper: delete the previous run's probe db. A missing file is the
/// expected first-run state, so existence is probed instead of matching platform
/// error codes.
fn remove_stale_db(path: &str) {
    let present = match exists(path) {
        Ok(true) => true,
        Ok(false) => false,
        // A stat failure means we cannot prove the file is absent: remove it and
        // let `Pool::new` report the sqlite-level reason.
        Err(_stat_err) => true,
    };
    if present {
        match remove_file(path) {
            Ok(_removed) => (),
            Err(remove_err) => panic!("stale probe db could not be removed: {remove_err}"),
        }
    }
}

/// The literals whose byte (codepoint) order differs from natural order — the
/// same fixture family as `nd-dbschema/tests/fresh_schema.rs`.
fn probe_literals() -> Vec<String> {
    let literals: Vec<&str> = ["image2", "image10", "image1", "IMAGE20", "image20"].to_vec();
    let owned = literals.iter().map(|lit| lit.to_string()).collect();
    owned
}

/// A small UNION of aliased literals ordered by the per-connection collation.
/// UNION keeps case-distinct literals separate (binary equality), so all five
/// rows survive.
fn natsort_sql() -> String {
    let clauses: Vec<String> = probe_literals()
        .iter()
        .map(|lit| format!("SELECT '{lit}' AS v"))
        .collect();
    let joined = clauses.join(" UNION ");
    format!("SELECT v FROM ({joined}) ORDER BY v COLLATE {NATSORT_COLLATION}")
}

/// Act helper: run a 2-arity `SEEDEDRAND` call and read the i64 hash it returns.
/// Arrange precondition asserted by the call site: the function is registered on
/// this connection by `nd_dbschema::schema::open` (`nd-dbschema/src/schema.rs:99`).
fn seedrand_value(conn: &Connection, sql: &str) -> i64 {
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let next = rows.next();
    let row = match next {
        Ok(Some(r)) => r,
        Ok(None) => panic!("SEEDEDRAND call returned no row"),
        Err(e) => panic!("{e}"),
    };
    let value: i64 = row.get(0).unwrap();
    value
}

/// Act helper: run a SELECT and collect column 0 as strings.
/// Arrange precondition asserted by the call site: the SQL is valid on a
/// connection opened through `nd_dbschema::schema::open`.
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

#[test]
fn pool_round_trip_hands_out_and_takes_back_every_lease() {
    // Arrange: temp FILE-backed db path, previous run removed first. Never
    // `:memory:`: `nd-dbschema/src/schema.rs:80-88` rewrites that to a shared-cache
    // DSN which aliases instances across concurrently running tests.
    let path = "/tmp/nd-db-pool-round-trip.sqlite";
    remove_stale_db(path);

    let config = PoolConfig::new(path);
    let want_size = pool_size();
    // Assert
    assert_eq!(
        config.size, want_size,
        "PoolConfig::new uses the MaxOpenConns pool size"
    );
    assert!(
        want_size >= MIN_POOL_SIZE,
        "conf/configuration.go:1217-1218 floors the pool at 4"
    );

    let pool = Pool::new(&config).unwrap();
    // Assert
    assert_eq!(pool.path(), path, "the pool opens the configured file path");
    assert_eq!(
        pool.size(),
        want_size,
        "Pool.size reports the configured pool size"
    );
    assert_eq!(
        pool.idle_count().unwrap(),
        want_size,
        "a fresh pool starts with size idle connections"
    );

    // Act + Assert inside an explicit scope so the Lease vector's Drop runs before
    // the idle-restored assertion below. All leases stay live together on ONE
    // thread: `rusqlite::Connection` is `Send` but not `Sync`, so the pool never
    // shares one connection across the leases it hands out.
    {
        let mut leases: Vec<Lease<'_>> = Vec::new();
        while leases.len() < want_size {
            let before = pool.idle_count().unwrap();
            match pool.checkout() {
                Ok(lease) => leases.push(lease),
                Err(pool_err) => panic!("pool could not serve a lease: {pool_err}"),
            }
            let after = pool.idle_count().unwrap();
            // Assert: one connection left the idle queue per lease taken.
            assert_eq!(
                after,
                before - 1,
                "each checkout takes exactly one connection from the idle queue"
            );
        }
        // Assert
        assert_eq!(
            leases.len(),
            want_size,
            "the pool served as many live leases as it was configured with"
        );
        let drained = pool.idle_count().unwrap();
        assert_eq!(
            drained,
            want_size - leases.len(),
            "every configured connection is now in use, none idle"
        );

        let probe_sql = natsort_sql();
        let want_len = probe_literals().len();
        let seedrand_calls = 2;
        let mut hash_count = 0;
        let mut order_count = 0;
        for lease in leases.iter() {
            // Act: the 2-arity `SEEDEDRAND` scalar and a `COLLATE NATSORT` ORDER BY
            // on this lease's connection (`nd-dbschema/src/schema.rs:95-108`
            // registers both per connection via `schema::open`).
            let conn = lease.conn();
            // The 2-arity positional call. `nd-dbschema/src/schema.rs:99` registers
            // SEEDEDRAND with SQLITE_UTF8 flags, so both arguments bind as text:
            // bare integers are rejected by sqlite (`Invalid function parameter type
            // Integer`), hence the quoted pair.
            let named = seedrand_value(conn, SEEDEDRAND_SQL);
            let positional = seedrand_value(conn, "SELECT SEEDEDRAND('1', '2')");
            let _named = named;
            let _positional = positional;
            hash_count += seedrand_calls;

            let order = read_col0(conn, &probe_sql);
            // Assert: natural order, not codepoint order (codepoint order would
            // lead with `IMAGE20`).
            assert_eq!(
                order.len(),
                want_len,
                "the UNION of distinct literals yields one row each"
            );
            assert_eq!(
                order[0], "image1",
                "natural order starts at the shortest digit run"
            );
            assert_eq!(order[1], "image2", "image2 precedes image10 under NATSORT");
            assert_eq!(
                order[2], "image10",
                "digit runs compare numerically, not as text"
            );
            if order.iter().find(|v| *v == "IMAGE20").is_none() {
                panic!("IMAGE20 missing from NATSORT order: {order:?}");
            }
            if order.iter().find(|v| *v == "image20").is_none() {
                panic!("image20 missing from NATSORT order: {order:?}");
            }
            order_count += 1;
        }

        // Assert
        assert_eq!(
            hash_count,
            want_size * seedrand_calls,
            "every lease answered both SEEDEDRAND calls"
        );
        assert_eq!(
            order_count, want_size,
            "every lease answered the COLLATE NATSORT query"
        );
    }

    // Assert: every lease returned its connection to the idle queue on Drop.
    let idle_after = pool.idle_count().unwrap();
    assert_eq!(
        idle_after, want_size,
        "the pool is back to size idle connections after the leases drop"
    );
    assert_eq!(
        pool.size(),
        idle_after,
        "idle restored equals the configured pool size"
    );
}
