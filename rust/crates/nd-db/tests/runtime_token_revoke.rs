//! DB-13 — the epoch-only token revocation primitive (`user_repo::revoke_tokens_in_tx`)
//! EXECUTED against a real SQLite DB built from the fresh DDL.
//!
//! Go truth under test: `user_repository.go:129-144` bumps `token_epoch` inside the
//! SAME statement as the password UPDATE, because as two statements a concurrent
//! change can interleave and leave a session valid that the other change revoked
//! (`:123-125`). Go has **no epoch-only operation**, so DB-13 is an approved Rust
//! API addition and NOT claimed Go endpoint parity
//! (`docs/rust-port/subplans/db-runtime.md` DB-13 point 8); this file asserts the
//! Rust primitive's own contract rather than a Go golden.
//!
//! Every behavioral claim is EXECUTED through the production API —
//! `revoke_tokens_in_tx` on a caller-owned transaction driven by the DB-00 typed
//! seam (`SQLStore::try_with_tx_sync` → `store::try_with_tx`, and
//! `store::try_with_tx_retry`) — and read back through the production `UserRepo` or
//! a full-column snapshot, never through a hand-built epoch UPDATE. Raw INSERTs are
//! used only for states `UserRepo::put` cannot produce (an epoch at `i64::MAX`, a
//! damaged REAL cell); the mutation under test always runs through the primitive.
//!
//! The guards are asserted as load-bearing rather than decorative, by executing the
//! statements they replace against the real schema: an unguarded increment corrupts
//! a `i64::MAX` cell to REAL, and a MAX-only increment (no `typeof` predicate)
//! coerces a malformed REAL into a VALID INTEGER epoch and publishes it. That is
//! precisely what `increment_token_epoch_sql` prevents, before any mutation.
//!
//! Concurrency is asserted with barriers and a bounded channel handshake — no sleeps
//! anywhere, no wall-clock race — plus one deterministic held-lock case with a zero
//! busy timeout and a zero injected retry delay.
//!
//! Out of scope here by design (BOOT/API-03 owns them, `subplans/README.md:66-67`):
//! authorization, HTTP status, token minting, holder refresh, and rejecting an old
//! token after a revoke. No `nd-auth` dependency is used.

use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::fs;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use nd_db::pool::{Pool, PoolConfig};
use nd_db::runtime_error::{DbOperationError, DbRuntimeError, TransactionStage};
use nd_db::store::{is_busy, try_with_tx_retry, SQLStore, StoreError, TX_MAX_RETRIES};
use nd_db::user_repo::{
    increment_token_epoch_sql, put_password_update_sql, revoke_tokens_in_tx,
    select_token_epoch_sql, NewTokenEpoch, RevokeError, UserPutValues, UserRepo, USER_TABLE,
};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::types::Type as SqliteType;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// Exact `i64` ceiling, spelled as a literal so the assertions state the guard's
/// bound instead of re-deriving it from the renderer.
const MAX_EPOCH: i64 = i64::MAX;

const T0: &str = "2024-01-01 00:00:00+00:00";
const T1: &str = "2024-02-02 00:00:00+00:00";
const T2: &str = "2024-03-03 00:00:00+00:00";

/// Bounded handshake timeout for the two-connection test. It only bounds the wait;
/// every assertion below holds whichever connection wins, and a dropped sender wakes
/// the waiting thread instead of blocking it forever.
const LOCK_HANDSHAKE: Duration = Duration::from_secs(10);

/// Zero-delay injected backoff for the DB-00 retry seam (`store.rs:223`). Contention
/// is proved by the seam's own `attempts` count, not by a counter here, so a retry
/// that does happen costs nothing instead of waiting out the 5s/10s/15s production
/// delays.
fn zero_delay(attempt: u32) -> Duration {
    let _ = attempt;
    Duration::ZERO
}

/// Full snapshot of one `user` row: every declared column in DDL order
/// (`nd-dbschema/src/schema_ddl.rs:57`), so a comparison proves WHICH cells a
/// mutation touched — an epoch-only read cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserSnapshot {
    id: String,
    user_name: String,
    name: String,
    email: String,
    password: String,
    is_admin: bool,
    last_login_at: Option<String>,
    last_access_at: Option<String>,
    created_at: String,
    updated_at: String,
    scrobble_filter: String,
    token_epoch: i64,
}

/// A caller's own failure shapes, used to prove a successful revoke publishes
/// nothing when the CALLER fails after it.
#[derive(Debug, thiserror::Error)]
enum CallerError {
    /// The primitive itself failed; wrapped rather than swallowed.
    #[error("revoke failed: {source}")]
    Revoke {
        /// The operation error the primitive reported.
        #[source]
        source: DbOperationError<RevokeError>,
    },

    /// A failure the caller raises AFTER a successful revoke.
    #[error("caller failure after the revoke")]
    Late,
}

/// Lift a primitive failure into the caller's own domain type, keeping the
/// operation error intact as the source.
fn revoke_failed(source: DbOperationError<RevokeError>) -> DbOperationError<CallerError> {
    DbOperationError::Domain(CallerError::Revoke { source })
}

/// One isolated on-disk DB per invocation: a unique temp path (never a fixed `/tmp`
/// name two runs could share), freshly migrated, pooled so `SEEDEDRAND`/`NATSORT`
/// are registered per connection (`schema-contract.md` §4). `_foreign_keys=on` is
/// Go's `db/db.go:55` DSN parameter, so `user_library`'s FK really is enforced.
fn fresh_store(label: &str, size: usize) -> (SQLStore, std::path::PathBuf) {
    let path = temp_db_path(label);
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, size)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Unique temp DB path with any leftover file cleared, so a stale file can never
/// make a fresh-schema assumption false. A missing file is the expected first-run
/// state, so existence is probed rather than an error code matched.
fn temp_db_path(label: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-token-revoke-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    path
}

/// Run one closure on a leased connection; pool/store failures panic, the
/// closure's own value comes back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// An independent raw handle on the same file. rusqlite transactions need
/// `&mut Connection`, so a caller transaction that is opened and dropped — rather
/// than driven by the store seam — is exercised through its own handle, exactly as
/// an outside caller would do.
fn raw_conn(path: &std::path::Path) -> rusqlite::Connection {
    nd_dbschema::schema::open(&format!(
        "file:{}?mode=rwc&_foreign_keys=on",
        path.display()
    ))
    .unwrap_or_else(|e| panic!("raw handle on {:?}: {e}", path))
}

/// Seed a `user` row through a raw handle (the two-connection tests build their DB
/// outside the pool, so the pooled fixture helper cannot reach it). Same columns as
/// [`seed_user`], epoch from the column DEFAULT.
fn seed_user_raw(conn: &rusqlite::Connection, id: &str) {
    conn.execute(
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, \
         created_at, updated_at, scrobble_filter) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            id,
            id,
            "John Doe",
            "jdoe@example.com",
            "CIPHER",
            0,
            T0,
            T1,
            ""
        ],
    )
    .unwrap_or_else(|e| panic!("raw user seed: {e}"));
}

/// One scalar probe (`count(*)`, `typeof`, a stored value) on a pooled lease.
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// A raw fixture statement. Used only for states `UserRepo::put` cannot produce;
/// it is never the mutation under test.
fn fixture_exec(store: &SQLStore, sql: &str, args: &[SqlValue]) {
    lease(store, |conn| {
        conn.execute(sql, rusqlite::params_from_iter(args.iter()))
            .unwrap_or_else(|e| panic!("{sql} --{args:?}--: {e}"));
    });
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

/// Epoch probe for one id, read as `i64`, so a cell SQLite promoted to REAL fails
/// the read instead of being silently coerced.
fn stored_epoch(store: &SQLStore, id: &str) -> i64 {
    scalar(
        store,
        &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = '{id}'"),
    )
}

/// Storage class SQLite reports for the epoch cell.
fn epoch_storage(store: &SQLStore, id: &str) -> String {
    scalar(
        store,
        &format!("SELECT typeof(token_epoch) FROM {USER_TABLE} WHERE id = '{id}'"),
    )
}

/// Assert the epoch cell is an intact INTEGER holding `want`.
fn assert_epoch_integer(store: &SQLStore, id: &str, want: i64) {
    assert_eq!(stored_epoch(store, id), want, "epoch cell changed for {id}");
    assert_eq!(
        epoch_storage(store, id),
        "integer",
        "token_epoch must stay an INTEGER (schema_ddl.rs:57) for {id}"
    );
}

/// Read epoch + storage class from a raw read-only handle on a file path, used
/// after writer threads have returned their connections.
fn epoch_from_file(db_path: &str, id: &str) -> (i64, String) {
    let conn = nd_dbschema::schema::open(&format!("file:{db_path}?mode=ro")).unwrap();
    conn.query_row(
        &format!("SELECT token_epoch, typeof(token_epoch) FROM {USER_TABLE} WHERE id = '{id}'",),
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|e| panic!("epoch read on {db_path}: {e}"))
}

/// Assert the committed epoch on a file is the INTEGER `want`.
fn assert_epoch_integer_from_file(db_path: &str, id: &str, want: i64) {
    let (epoch, storage) = epoch_from_file(db_path, id);
    assert_eq!(epoch, want, "committed epoch for {id}");
    assert_eq!(
        storage, "integer",
        "committed cell for {id} must be INTEGER",
    );
}

/// `user` row (`schema_ddl.rs:57`): `password`/`created_at`/`updated_at` are
/// `not null`, so all are seeded explicitly. `token_epoch` takes the column DEFAULT
/// (0) unless `epoch` is `Some` — the only way a fixture reaches `i64::MAX`, since
/// `UserRepo::put` cannot produce that value.
fn seed_user(store: &SQLStore, id: &str, password: &str, epoch: Option<i64>) {
    let sql = match epoch {
        Some(_) => {
            "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, \
                    created_at, updated_at, scrobble_filter, token_epoch) \
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
        None => {
            "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, \
                created_at, updated_at, scrobble_filter) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
    };
    let mut args = vec![
        text(id),
        text(id),
        text("John Doe"),
        text("jdoe@example.com"),
        text(password),
        integer(0),
        text(T0),
        text(T1),
        text(""),
    ];
    if let Some(epoch) = epoch {
        args.push(integer(epoch));
    }
    fixture_exec(store, sql, &args);
}

/// Snapshot of every `user` column, so a comparison pins what a mutation touched.
fn snapshot(store: &SQLStore, id: &str) -> UserSnapshot {
    lease(store, |conn| {
        conn.query_row(
            "SELECT id, user_name, name, email, password, is_admin, last_login_at, \
             last_access_at, created_at, updated_at, scrobble_filter, token_epoch \
             FROM \"user\" WHERE id = ?",
            [id],
            |row| {
                Ok(UserSnapshot {
                    id: row.get(0)?,
                    user_name: row.get(1)?,
                    name: row.get(2)?,
                    email: row.get(3)?,
                    password: row.get(4)?,
                    is_admin: row.get::<_, bool>(5)?,
                    last_login_at: row.get(6)?,
                    last_access_at: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    scrobble_filter: row.get(10)?,
                    token_epoch: row.get(11)?,
                })
            },
        )
        .unwrap_or_else(|e| panic!("snapshot of {id}: {e}"))
    })
}

/// The `user_library` links of one user as a set (order is not part of the
/// contract; membership and count are).
fn library_links(store: &SQLStore, id: &str) -> BTreeSet<i64> {
    lease(store, |conn| {
        let mut statement = conn
            .prepare("SELECT library_id FROM user_library WHERE user_id = ?")
            .unwrap_or_else(|e| panic!("link query: {e}"));
        statement
            .query_map([id], |row| row.get::<_, i64>(0))
            .unwrap_or_else(|e| panic!("link query: {e}"))
            .collect::<rusqlite::Result<BTreeSet<i64>>>()
            .unwrap_or_else(|e| panic!("link query: {e}"))
    })
}

/// Assert no `user_library` row exists anywhere (a revoke must never assign one).
fn assert_no_library_rows(store: &SQLStore) {
    assert_eq!(
        scalar::<i64>(store, "SELECT count(*) FROM user_library"),
        0,
        "a revoke must not create user_library rows"
    );
}

/// Assert the `user` table holds exactly `want` rows (a revoke must never insert).
fn assert_user_count(store: &SQLStore, want: i64) {
    assert_eq!(
        scalar::<i64>(store, &format!("SELECT count(*) FROM {USER_TABLE}")),
        want,
        "a revoke must not insert users"
    );
}

/// `library` row (`schema_ddl.rs:52`): `name`/`path` are `not null unique`.
fn seed_library(store: &SQLStore, id: i64, name: &str, default_new_users: bool) {
    fixture_exec(
        store,
        "INSERT INTO library (id, name, path, remote_path, default_new_users) \
         VALUES (?, ?, ?, ?, ?)",
        &[
            integer(id),
            text(name),
            text(&format!("/{name}")),
            text(""),
            integer(i64::from(default_new_users)),
        ],
    );
}

/// `UserPutValues` for the production `put` calls (the non-vacuity controls).
/// `updated_at` is a parameter so a `Put` stamp is distinguishable from a revoke
/// that must leave it alone.
fn put_values<'a>(
    id: &'a str,
    password: Option<&'a str>,
    updated_at: &'a str,
) -> UserPutValues<'a> {
    UserPutValues {
        id,
        user_name: id,
        name: "John Doe",
        email: "jdoe@example.com",
        password,
        is_admin: false,
        last_login_at: None,
        last_access_at: None,
        created_at: T0,
        updated_at,
        scrobble_filter: "",
    }
}

/// Drive `revoke_tokens_in_tx` through the DB-00 typed seam on the store: one
/// checkout, one transaction, commit on `Ok`, rollback on any error. This is the
/// only way the primitive is reached from a store here, which is what makes the
/// transaction-only claim observable.
fn revoke_via_store(
    store: &SQLStore,
    user_id: &str,
) -> Result<NewTokenEpoch, DbRuntimeError<RevokeError>> {
    store.try_with_tx_sync(|tx| revoke_tokens_in_tx(tx, user_id))
}

/// The same primitive through the retrying seam with an injected zero delay.
fn revoke_via_raw_conn(
    conn: &mut rusqlite::Connection,
    user_id: &str,
) -> Result<NewTokenEpoch, DbRuntimeError<RevokeError>> {
    try_with_tx_retry(conn, &zero_delay, |tx| revoke_tokens_in_tx(tx, user_id))
}

/// Assert the failure is the typed domain arm carrying exactly this variant.
fn assert_domain<T: std::fmt::Debug>(
    result: Result<T, DbOperationError<RevokeError>>,
    expected: impl FnOnce(&RevokeError) -> bool,
    label: &str,
) {
    match result {
        Err(DbOperationError::Domain(error)) => {
            assert!(expected(&error), "{label}: got {error:?}");
        }
        Err(DbOperationError::Sqlite(source)) => {
            panic!("{label}: expected Domain, got Sqlite({source})");
        }
        Ok(value) => panic!("{label}: expected a Domain failure, got Ok({value:?})"),
    }
}

/// Walk the `source` chain for a concrete `rusqlite::Error`, so the SQLite arm is
/// checked by type rather than by message text.
fn source_chain_has_rusqlite(error: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(node) = current {
        if node.downcast_ref::<SqliteError>().is_some() {
            return true;
        }
        current = node.source();
    }
    false
}

/// The statements the guards exist to replace, executed ONLY as hazard probes:
/// `SET token_epoch = token_epoch + 1` with neither predicate (`"max"`) or with
/// the upper bound only (`"type"`). `RETURNING` binds aliases so `typeof` stays
/// readable once the cell has been coerced or promoted.
fn unguarded_probe(conn: &rusqlite::Connection, id: &str, variant: &str) -> (SqlValue, String) {
    let sql = match variant {
        "max" => {
            "UPDATE \"user\" SET token_epoch = token_epoch + 1 WHERE id = ? \
                RETURNING token_epoch AS epoch, typeof(token_epoch) AS storage"
        }
        "type" => {
            "UPDATE \"user\" SET token_epoch = token_epoch + 1 \
                WHERE id = ? AND token_epoch < 9223372036854775807 \
                RETURNING token_epoch AS epoch, typeof(token_epoch) AS storage"
        }
        other => panic!("unknown hazard probe variant {other}"),
    };
    conn.query_row(sql, [id], |row| {
        Ok((row.get::<_, SqlValue>(0)?, row.get::<_, String>(1)?))
    })
    .unwrap_or_else(|e| panic!("{variant} hazard probe on {id}: {e}"))
}

/// Storage class and raw value of one epoch cell read through a `Value`, so a
/// REAL/TEXT/BLOB cell is reported instead of coerced (`stored_epoch` would fail
/// its `i64` decode on exactly those cells). A read-only handle on the file makes
/// this a statement about COMMITTED state.
fn epoch_state_from_file(db_path: &str, id: &str) -> (SqlValue, String) {
    let conn = nd_dbschema::schema::open(&format!("file:{db_path}?mode=ro")).unwrap();
    conn.query_row(
        &format!("SELECT token_epoch, typeof(token_epoch) FROM {USER_TABLE} WHERE id = ?",),
        [id],
        |row| Ok((row.get::<_, SqlValue>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|e| panic!("epoch state read on {db_path}: {e}"))
}

/// The cell the primitive left behind on a file: exact value AND storage class,
/// byte-identical to `want`.
fn assert_epoch_state_from_file(db_path: &str, id: &str, want: &(SqlValue, String)) {
    assert_eq!(
        epoch_state_from_file(db_path, id),
        *want,
        "a refused revoke must leave {id} exactly as found"
    );
}

/// The cell as it stands INSIDE a caller's transaction, read through the same
/// transaction handle that ran the primitive: this is what a caller that ignores
/// the error and commits would publish.
fn epoch_state_in_tx(tx: &rusqlite::Transaction<'_>, id: &str) -> (SqlValue, String) {
    tx.query_row(
        &format!("SELECT token_epoch, typeof(token_epoch) FROM {USER_TABLE} WHERE id = ?",),
        [id],
        |row| Ok((row.get::<_, SqlValue>(0)?, row.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|e| panic!("in-transaction epoch read on {id}: {e}"))
}

/// Read one epoch cell as a strict `i64` inside a caller's transaction, reporting
/// the failure instead of panicking, so the classification read's own decode
/// result can be asserted.
fn epoch_i64_in_tx(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<i64, SqliteError> {
    tx.query_row(
        &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = ?"),
        [id],
        |row| row.get::<_, i64>(0),
    )
}

/// Assert the SQLite arm carries this exact `rusqlite::Error` variant — the
/// decode failures are pinned by type, so no coercion can quietly make a
/// malformed cell decodable again.
fn assert_sqlite_error<T: std::fmt::Debug>(
    result: Result<T, DbOperationError<RevokeError>>,
    expected: impl FnOnce(&SqliteError) -> bool,
    label: &str,
) {
    match result {
        Err(DbOperationError::Sqlite(source)) => {
            assert!(expected(&source), "{label}: got {source:?}");
        }
        Err(DbOperationError::Domain(error)) => {
            panic!("{label}: expected Sqlite, got Domain({error:?})");
        }
        Ok(value) => panic!("{label}: expected a Sqlite failure, got Ok({value:?})"),
    }
}

/// A list rather than a `const`, because `SqlValue::Text`/`Blob` own heap data.
/// Each entry is a malformed epoch cell under test with the exact value and storage
/// class SQLite reports for it. `sql` is a raw INSERT/UPDATE expression, only usable
/// for states `UserRepo::put` cannot produce; the mutation under test always runs
/// through the primitive.
struct DamagedCell {
    id: &'static str,
    sql: &'static str,
    storage: &'static str,
    value: SqlValue,
}

fn damaged_cells() -> Vec<DamagedCell> {
    vec![
        DamagedCell {
            id: "half",
            sql: "1.5",
            storage: "real",
            value: SqlValue::Real(1.5),
        },
        DamagedCell {
            id: "rounding",
            sql: "4503599627370495.5",
            storage: "real",
            value: SqlValue::Real(4_503_599_627_370_495.5),
        },
        DamagedCell {
            id: "real_max",
            sql: "9.223372036854776e+18",
            storage: "real",
            value: SqlValue::Real(9.223_372_036_854_776e+18),
        },
        DamagedCell {
            id: "text",
            sql: "'bad'",
            storage: "text",
            value: SqlValue::Text("bad".to_string()),
        },
        DamagedCell {
            id: "blob",
            sql: "X'1234'",
            storage: "blob",
            value: SqlValue::Blob(vec![0x12, 0x34]),
        },
    ]
}

/// The `rusqlite::Error` each malformed cell must produce: the strict `i64`
/// decode of the classification read, pinned by variant, column index, column
/// name, and the reported storage type. `Type::Real`/`Text`/`Blob` here is the
/// decode refusing, never a coercion.
fn damaged_type(storage: &str) -> SqliteType {
    match storage {
        "real" => SqliteType::Real,
        "text" => SqliteType::Text,
        "blob" => SqliteType::Blob,
        other => panic!("no malformed-cell assertion for storage class {other}"),
    }
}

#[test]
fn guarded_increment_is_one_statement_with_both_predicates() {
    // Act: the renderer.
    let (sql, args) = increment_token_epoch_sql("u1");

    // Assert: only `token_epoch` is assigned, BOTH guards are present, and the new
    // value is RETURNed by the SAME statement (Go's single-statement rationale,
    // `user_repository.go:123-125`).
    assert_eq!(
        sql,
        format!(
            "UPDATE user SET token_epoch = token_epoch + 1 \
             WHERE id = ? AND typeof(token_epoch) = 'integer' \
             AND token_epoch < {MAX_EPOCH} RETURNING token_epoch"
        )
    );
    // Assert: the id is the only bound value, so no other column can be written.
    assert_eq!(args, vec![nd_db::sqlrepo::Value::from("u1")]);
    // Assert: the guards are predicates in the WHERE clause, never a CAST or a
    // rewrite of the assigned expression, so malformed data is rejected, not repaired.
    assert!(!sql.contains("CAST"), "the guard must not coerce: {sql}");
    assert_eq!(
        sql.split_once(" SET ")
            .expect("one SET clause")
            .1
            .split_once(" WHERE ")
            .expect("guards are in WHERE")
            .0,
        "token_epoch = token_epoch + 1",
        "the assignment is the bare guarded increment"
    );
    // Assert: no other user column and no `user_library` leg appears at all.
    for forbidden in [
        "password",
        "updated_at",
        "last_access_at",
        "last_login_at",
        "is_admin",
        "user_library",
        "INSERT",
    ] {
        assert!(
            !sql.contains(forbidden),
            "the epoch-only statement must not mention {forbidden}: {sql}"
        );
    }

    // Assert: the classification read is a bare primary-key select, so it cannot
    // multiply rows, does not pay for the libraries hydration, and carries NEITHER
    // guard nor CAST: it reports what the row holds, which is what keeps a malformed
    // cell on the SQLite arm instead of Exhausted.
    let (select_sql, select_args) = select_token_epoch_sql("u1");
    assert_eq!(select_sql, "SELECT token_epoch FROM user WHERE id = ?");
    assert_eq!(select_args, vec![nd_db::sqlrepo::Value::from("u1")]);
}

#[test]
fn revoke_increments_from_zero_repeatedly_and_returns_each_new_epoch() {
    let (store, path) = fresh_store("increment", 2);
    seed_user(&store, "u1", "CIPHER", None);
    assert_epoch_integer(&store, "u1", 0);

    // Act: five revokes, each through the production typed seam.
    let epochs = (0..5)
        .map(|_| {
            revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke must succeed: {e}"))
        })
        .map(NewTokenEpoch::value)
        .collect::<Vec<_>>();

    // Assert: a typed new epoch per commit, strictly increasing by one.
    assert_eq!(epochs, vec![1, 2, 3, 4, 5]);
    // Assert: the committed row agrees with the last returned value and stayed an
    // INTEGER throughout.
    assert_epoch_integer(&store, "u1", 5);
    assert_user_count(&store, 1);
    assert_no_library_rows(&store);

    fs::remove_file(path).ok();
}

#[test]
fn revoke_changes_only_epoch_and_returns_committed_value() {
    let (store, path) = fresh_store("columns", 2);
    seed_user(&store, "u1", "argon2$2a$ciphertext-with-bytes", None);
    seed_user(&store, "u2", "OTHER-CIPHER", None);
    seed_library(&store, 1, "Rock", true);
    seed_library(&store, 2, "Jazz", false);
    fixture_exec(
        &store,
        "INSERT INTO user_library (user_id, library_id) VALUES (?, ?)",
        &[text("u1"), integer(2)],
    );

    // Arrange: full snapshots of BOTH users, the password bytes, and the links.
    let before_u1 = snapshot(&store, "u1");
    let before_u2 = snapshot(&store, "u2");
    let links_before = library_links(&store, "u1");
    assert_eq!(links_before, BTreeSet::from([2]));

    // Act:
    let epoch = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));

    // Assert: the returned epoch is the committed value.
    assert_eq!(epoch.value(), 1);
    assert_eq!(stored_epoch(&store, "u1"), epoch.value());

    // Assert: every other column of the revoked row is byte-identical.
    let after_u1 = snapshot(&store, "u1");
    let mut expected_u1 = before_u1.clone();
    expected_u1.token_epoch = 1;
    assert_eq!(
        after_u1, expected_u1,
        "revoke must change token_epoch and nothing else"
    );
    // Assert each field explicitly too, so struct equality cannot hide a coerced
    // column behind an identical rendering.
    assert_eq!(after_u1.password, before_u1.password);
    assert_eq!(after_u1.updated_at, T1);
    assert_eq!(after_u1.created_at, T0);
    assert_eq!(after_u1.last_login_at, None);
    assert_eq!(after_u1.last_access_at, None);
    assert!(!after_u1.is_admin);
    assert_eq!(after_u1.scrobble_filter, "");
    assert_eq!(after_u1.user_name, before_u1.user_name);
    assert_eq!(after_u1.email, before_u1.email);

    // Assert: the untouched user is byte-identical, epoch included.
    assert_eq!(snapshot(&store, "u2"), before_u2);
    // Assert: no library was granted or revoked, and no new rows appeared.
    assert_eq!(library_links(&store, "u1"), links_before);
    assert_eq!(library_links(&store, "u2"), BTreeSet::new());
    assert_user_count(&store, 2);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM user_library"),
        1,
        "revoke must not add user_library rows"
    );

    fs::remove_file(path).ok();
}

#[test]
fn revoke_missing_user_is_typed_notfound_and_creates_nothing() {
    let (store, path) = fresh_store("missing", 2);
    seed_user(&store, "u1", "CIPHER", None);
    let before = snapshot(&store, "u1");

    // Act: an id with no row, through the store seam.
    let result = revoke_via_store(&store, "ghost");

    // Assert: typed NotFound carrying the id, never a SQLite arm.
    match result {
        Err(DbRuntimeError::Domain(RevokeError::NotFound { id })) => assert_eq!(id, "ghost"),
        other => panic!("expected Domain(NotFound), got {other:?}"),
    }
    // Assert: nothing was inserted anywhere.
    assert_user_count(&store, 1);
    assert_no_library_rows(&store);
    assert_eq!(
        scalar::<i64>(
            &store,
            &format!("SELECT count(*) FROM {USER_TABLE} WHERE id = 'ghost'")
        ),
        0,
        "a missing-user revoke must never insert"
    );
    // Assert: the existing user is untouched, epoch included.
    assert_eq!(snapshot(&store, "u1"), before);

    // Assert the same classification inside a raw caller transaction on its own
    // handle, so the result does not depend on the store facade.
    {
        let mut conn = raw_conn(&path);
        let tx = conn.transaction().unwrap();
        let raw = revoke_tokens_in_tx(&tx, "ghost");
        assert_domain(
            raw,
            |error| matches!(error, RevokeError::NotFound { id } if id == "ghost"),
            "raw tx missing user",
        );
        drop(tx);
    }
    assert_user_count(&store, 1);

    fs::remove_file(path).ok();
}

#[test]
fn revoke_at_i64_max_is_typed_exhausted_and_leaves_the_cell_integer() {
    let (store, path) = fresh_store("exhausted", 2);
    seed_user(&store, "u1", "CIPHER", Some(MAX_EPOCH - 1));
    seed_user(&store, "u2", "CIPHER", Some(MAX_EPOCH));

    // Act: MAX-1 still revokes, landing exactly on the ceiling.
    let epoch = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
    assert_eq!(epoch.value(), MAX_EPOCH);
    assert_epoch_integer(&store, "u1", MAX_EPOCH);

    // Act: at the ceiling the guard refuses the increment.
    let result = revoke_via_store(&store, "u2");
    match result {
        Err(DbRuntimeError::Domain(RevokeError::Exhausted { id })) => assert_eq!(id, "u2"),
        other => panic!("expected Domain(Exhausted), got {other:?}"),
    }

    // Assert: the cell is the exact INTEGER MAX — not saturated, wrapped, reset,
    // or promoted to REAL, and no float round-trip happened.
    assert_epoch_integer(&store, "u2", MAX_EPOCH);
    let at_max = snapshot(&store, "u2");
    assert_eq!(at_max.token_epoch, MAX_EPOCH);
    assert_eq!(at_max.password, "CIPHER");
    assert_eq!(at_max.updated_at, T1);

    fs::remove_file(path).ok();
}

#[test]
fn missing_and_exhausted_are_distinguishable_in_the_same_transaction() {
    let (store, path) = fresh_store("classification", 2);
    seed_user(&store, "u1", "CIPHER", Some(MAX_EPOCH));

    // Arrange: one transaction on its own handle holding BOTH no-row outcomes, so
    // the comparison is made against identical statement state rather than across
    // commits.
    {
        let mut conn = raw_conn(&path);
        let tx = conn.transaction().unwrap();
        let missing = revoke_tokens_in_tx(&tx, "ghost");
        let exhausted = revoke_tokens_in_tx(&tx, "u1");

        // Assert: distinct variants — an exhausted row is never reported as missing.
        assert_domain(
            missing,
            |error| matches!(error, RevokeError::NotFound { .. }),
            "missing",
        );
        assert_domain(
            exhausted,
            |error| matches!(error, RevokeError::Exhausted { id } if id == "u1"),
            "exhausted",
        );
        drop(tx);
    }

    // Assert: the rolled-back classification transaction published nothing.
    assert_epoch_integer(&store, "u1", MAX_EPOCH);
    assert_user_count(&store, 1);

    fs::remove_file(path).ok();
}

#[test]
fn guard_is_load_bearing_an_unguarded_increment_corrupts_the_cell() {
    let (store, path) = fresh_store("guard-hazard", 3);
    seed_user(&store, "u1", "CIPHER", Some(MAX_EPOCH));
    fixture_exec(
        &store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, \
         created_at, updated_at, scrobble_filter, token_epoch) \
         VALUES ('u4', 'u4', 'n', 'e', 'CIPHER', 0, ?, ?, '', 4503599627370495.5)",
        &[text(T0), text(T1)],
    );

    // Act: the fully unguarded statement the guards replace, on the real fresh schema.
    let (value, storage) = lease(&store, |conn| unguarded_probe(conn, "u1", "max"));

    // Assert: SQLite promotes the overflow to REAL, so the column declared
    // `token_epoch INTEGER` is corrupted and the value is not an i64. This is the
    // hazard the upper-bound guard removes BEFORE mutation.
    assert_eq!(storage, "real");
    assert!(
        matches!(value, SqlValue::Real(_)),
        "unguarded overflow must not stay an INTEGER, got {value:?}"
    );
    // Assert: the corrupted cell cannot be read back as i64, which is why the
    // primitive must refuse rather than saturate and repair afterwards.
    lease(&store, |conn| {
        let decoded = conn.query_row(
            &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = 'u1'"),
            [],
            |row| row.get::<_, i64>(0),
        );
        assert!(decoded.is_err(), "a REAL cell must not decode as i64");
    });

    // Act: the hazard the `typeof` guard removes — the MAX-only predicate (no type
    // check) run on a malformed REAL below the ceiling. This is the P2 vector: SQLite
    // does the arithmetic and INTEGER-affinity conversion first, so the statement
    // SUCCEEDS with a valid-looking INTEGER.
    let (coerced, coerced_storage) = lease(&store, |conn| unguarded_probe(conn, "u4", "type"));
    // Assert: `4503599627370495.5` is rounded across the f64 precision boundary and
    // becomes an INTEGER cell — a successful revocation of a row that never held an
    // integer epoch, which a committing caller would publish.
    assert_eq!(
        coerced,
        SqlValue::Integer(4_503_599_627_370_496),
        "the MAX-only predicate silently coerces the malformed REAL"
    );
    assert_eq!(coerced_storage, "integer");

    // Act/Assert: the production primitive on a row at MAX leaves it intact.
    seed_user(&store, "u2", "CIPHER", Some(MAX_EPOCH));
    let result = revoke_via_store(&store, "u2");
    match result {
        Err(DbRuntimeError::Domain(RevokeError::Exhausted { id })) => assert_eq!(id, "u2"),
        other => panic!("expected Domain(Exhausted), got {other:?}"),
    }
    assert_epoch_integer(&store, "u2", MAX_EPOCH);

    // Assert: the probe DID mutate the cell — which is exactly what the production
    // primitive must not do. `damaged_cells_are_rejected_before_any_mutation` runs
    // the primitive on the same value and shows the cell survives untouched.
    assert_eq!(stored_epoch(&store, "u4"), 4_503_599_627_370_496);

    fs::remove_file(path).ok();
}

#[test]
fn damaged_cells_are_rejected_before_any_mutation() {
    for cell in damaged_cells() {
        let (store, path) = fresh_store(&format!("damaged-{}", cell.id), 2);
        // Arrange: seed with the column DEFAULT, then damage the cell to the exact
        // malformed state. These are states `UserRepo::put` cannot produce; the
        // mutation under test is always the primitive.
        seed_user(&store, "u1", "CIPHER", None);
        fixture_exec(
            &store,
            &format!(
                "UPDATE \"user\" SET token_epoch = {} WHERE id = ?",
                cell.sql
            ),
            &[text("u1")],
        );
        assert_eq!(epoch_storage(&store, "u1"), cell.storage);
        assert_eq!(
            scalar::<SqlValue>(
                &store,
                &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = 'u1'")
            ),
            cell.value,
            "the fixture must hold the exact malformed value"
        );

        // Act: through the DB-00 typed seam.
        let result = revoke_via_store(&store, "u1");

        // Assert: the SQLite arm, staged `Body`, carrying the strict-decode failure
        // for THIS storage class — never `NotFound`, never `Exhausted`, never `Ok`.
        match result {
            Err(DbRuntimeError::Sqlite { stage, source }) => {
                assert_eq!(stage, TransactionStage::Body);
                assert!(
                    source_chain_has_rusqlite(&source),
                    "the SQLite arm must retain the concrete rusqlite error"
                );
                assert!(
                    matches!(
                        &source,
                        SqliteError::InvalidColumnType(0, name, reported)
                            if name == "token_epoch" && *reported == damaged_type(cell.storage)
                    ),
                    "{}: expected the strict i64 decode refusal, got {source:?}",
                    cell.id
                );
            }
            Err(DbRuntimeError::Domain(error)) => {
                panic!("{}: a damaged cell must never be {error:?}", cell.id);
            }
            Ok(epoch) => panic!(
                "{}: a damaged cell must never report a revocation, got {epoch}",
                cell.id
            ),
            other => panic!("{}: expected Sqlite, got {other:?}", cell.id),
        }

        // Assert: the cell is byte-identical (value AND storage class), so the
        // type guard refused BEFORE the arithmetic: nothing was coerced, promoted,
        // rounded, or incremented. The password is read as text rather than through
        // `snapshot`, whose i64 decode legitimately fails on a non-INTEGER cell.
        assert_eq!(epoch_storage(&store, "u1"), cell.storage);
        assert_eq!(
            scalar::<SqlValue>(
                &store,
                &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = 'u1'")
            ),
            cell.value,
            "{}: a refused revoke must not mutate the cell",
            cell.id
        );
        assert_eq!(
            scalar::<String>(
                &store,
                &format!("SELECT password FROM {USER_TABLE} WHERE id = 'u1'")
            ),
            "CIPHER",
            "a failed revoke must not touch other columns"
        );
        assert_eq!(
            scalar::<String>(
                &store,
                &format!("SELECT updated_at FROM {USER_TABLE} WHERE id = 'u1'")
            ),
            T1,
            "a failed revoke must not stamp updated_at"
        );
        assert_user_count(&store, 1);
        assert_no_library_rows(&store);

        // Assert: a RAW caller transaction that ignores the error and COMMITS still
        // publishes nothing, because the type guard mutated nothing before refusing.
        {
            let mut conn = raw_conn(&path);
            let tx = conn.transaction().unwrap();
            let raw = revoke_tokens_in_tx(&tx, "u1");
            assert_sqlite_error(
                raw,
                |source| {
                    matches!(
                        source,
                        SqliteError::InvalidColumnType(0, name, reported)
                            if name == "token_epoch" && *reported == damaged_type(cell.storage)
                    )
                },
                &format!("{} raw tx", cell.id),
            );
            // Assert BEFORE any commit decision: the in-transaction cell is unchanged,
            // and the classification read's own strict decode fails the same way, so
            // malformed data is never inferred to be an exhausted counter.
            assert_eq!(
                epoch_state_in_tx(&tx, "u1"),
                (cell.value.clone(), cell.storage.to_string()),
                "{}: nothing was mutated inside the caller's tx",
                cell.id
            );
            assert!(
                epoch_i64_in_tx(&tx, "u1").is_err(),
                "{}: the malformed cell must not decode as i64",
                cell.id
            );
            tx.commit()
                .unwrap_or_else(|e| panic!("{} raw commit: {e}", cell.id));
        }
        // Assert: the caller's own commit published exactly the original cell.
        assert_epoch_state_from_file(
            &path.display().to_string(),
            "u1",
            &(cell.value.clone(), cell.storage.to_string()),
        );
        assert_eq!(
            scalar::<String>(
                &store,
                &format!("SELECT password FROM {USER_TABLE} WHERE id = 'u1'")
            ),
            "CIPHER"
        );

        fs::remove_file(path).ok();
    }
}

/// An INTEGRAL REAL must also be refused, even when its numeric value is exactly
/// representable and in range. On the fresh DDL such a value cannot be stored —
/// INTEGER affinity converts `3.0` into an INTEGER cell (asserted at the end of this
/// test) — so this uses a deliberately corrupted schema that declares `token_epoch
/// REAL`, the state a damaged or hand-edited DB can reach. Executed on the same
/// bundled SQLite, through the production primitive.
#[test]
fn an_integral_real_cell_is_refused_and_never_bumped() {
    for value in ["3.0", "4503599627370496.0", "9223372036854775808.0"] {
        let exact = value
            .parse::<f64>()
            .unwrap_or_else(|e| panic!("fixture literal {value} is not an f64: {e}"));
        let path = temp_db_path(&format!("real-column-{}", value.replace('.', "_")));
        let dsn = format!("file:{}?mode=rwc", path.display());
        let mut conn = nd_dbschema::schema::open(&dsn).unwrap();
        // Arrange: the same column list as `schema_ddl.rs:57`, but `token_epoch`
        // declared REAL so INTEGER affinity does not coerce the stored value away.
        conn.execute(
            "CREATE TABLE \"user\" (id TEXT PRIMARY KEY, user_name TEXT, name TEXT, \
             email TEXT, password TEXT, is_admin INTEGER, last_login_at TEXT, \
             last_access_at TEXT, created_at TEXT, updated_at TEXT, scrobble_filter TEXT, \
             token_epoch REAL DEFAULT 0)",
            [],
        )
        .unwrap_or_else(|e| panic!("corrupted-schema create: {e}"));
        conn.execute(
            &format!(
                "INSERT INTO \"user\" (id, user_name, password, created_at, updated_at, \
                 token_epoch) VALUES ('u1', 'u1', 'CIPHER', ?, ?, {value})"
            ),
            [T0, T1],
        )
        .unwrap_or_else(|e| panic!("corrupted-schema seed: {e}"));
        assert_eq!(
            conn.query_row(
                "SELECT typeof(token_epoch) FROM \"user\" WHERE id = 'u1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_else(|e| panic!("corrupted-schema typeof: {e}")),
            "real",
            "{value} must really be stored as a REAL cell"
        );

        // Act: on a caller transaction on that handle.
        let tx = conn.transaction().unwrap();
        let raw = revoke_tokens_in_tx(&tx, "u1");
        // Assert: a concrete decode failure — not `Exhausted` (it is not an INTEGER
        // at MAX), not `NotFound`, not `Ok`.
        assert_sqlite_error(
            raw,
            |source| {
                matches!(
                    source,
                    SqliteError::InvalidColumnType(0, name, SqliteType::Real)
                        if name == "token_epoch"
                )
            },
            &format!("integral REAL {value}"),
        );
        // Assert BEFORE the commit decision: unchanged inside the transaction.
        assert_eq!(
            epoch_state_in_tx(&tx, "u1"),
            (SqlValue::Real(exact), "real".to_string()),
            "{value}: nothing was mutated"
        );
        tx.commit()
            .unwrap_or_else(|e| panic!("integral REAL commit: {e}"));
        // Assert: the caller's own commit published no bump and no coercion.
        assert_eq!(
            epoch_state_from_file(&path.display().to_string(), "u1"),
            (SqlValue::Real(exact), "real".to_string()),
            "{value}: the commit published no bump and no coercion"
        );

        fs::remove_file(path).ok();
    }

    // Assert: on the FRESH DDL an integral REAL cannot survive — INTEGER affinity
    // stores it as an INTEGER, which the primitive then increments normally. The
    // refusal above is therefore about the STORAGE CLASS, not about a numeric range.
    let (store, path) = fresh_store("integral-real-affinity", 2);
    seed_user(&store, "u1", "CIPHER", None);
    fixture_exec(
        &store,
        "UPDATE \"user\" SET token_epoch = 3.0 WHERE id = ?",
        &[text("u1")],
    );
    assert_epoch_integer(&store, "u1", 3);
    let epoch = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
    assert_eq!(epoch.value(), 4);
    assert_epoch_integer(&store, "u1", 4);

    fs::remove_file(path).ok();
}

#[test]
fn missing_or_malformed_table_is_a_sqlite_error_never_notfound() {
    // Arrange: a DB with NO `user` table at all (no fresh schema applied), so the
    // increment fails to prepare rather than matching zero rows.
    let path = temp_db_path("no-table");
    let dsn = format!("file:{}?mode=rwc", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 1)).unwrap();
    pool.put_back(first).expect("return configured handle");
    let store = SQLStore::new(pool);

    // Act:
    let result = revoke_via_store(&store, "u1");

    // Assert: a SQL failure, not a domain miss, and no epoch published.
    match result {
        Err(DbRuntimeError::Sqlite { stage, source }) => {
            assert_eq!(stage, TransactionStage::Body);
            assert!(
                source_chain_has_rusqlite(&source),
                "the concrete SQL error must be retained"
            );
        }
        Err(DbRuntimeError::Domain(error)) => panic!("a missing table must never be {error:?}"),
        Ok(epoch) => panic!("a missing table must never report an epoch: {epoch}"),
        other => panic!("expected Sqlite, got {other:?}"),
    }
    // Assert the raw-transaction path agrees (no store facade involved).
    {
        let mut conn = raw_conn(&path);
        let tx = conn.transaction().unwrap();
        let raw = revoke_tokens_in_tx(&tx, "u1");
        match raw {
            Err(DbOperationError::Sqlite(source)) => {
                assert!(
                    source_chain_has_rusqlite(&source),
                    "a raw tx must surface the SQL error"
                );
            }
            other => panic!("expected Sqlite in a raw tx, got {other:?}"),
        }
        drop(tx);
    }

    fs::remove_file(path).ok();
}

#[test]
fn caller_failure_after_the_revoke_rolls_the_bump_back() {
    let (store, path) = fresh_store("rollback", 2);
    seed_user(&store, "u1", "CIPHER", None);
    seed_user(&store, "u2", "CIPHER", None);
    let before_u1 = snapshot(&store, "u1");

    // Act: the revoke succeeds inside the caller's transaction, then the caller
    // fails after it. The primitive's own errors are wrapped, never swallowed.
    let outcome: Result<NewTokenEpoch, DbRuntimeError<CallerError>> =
        store.try_with_tx_sync(|tx| {
            let epoch = revoke_tokens_in_tx(tx, "u1").map_err(revoke_failed)?;
            assert_eq!(epoch.value(), 1, "the bump is visible inside its own tx");
            Err(DbOperationError::Domain(CallerError::Late))
        });

    // Assert: the caller's failure surfaces, and no epoch was published.
    assert!(matches!(
        outcome,
        Err(DbRuntimeError::Domain(CallerError::Late))
    ));
    // Assert: on a fresh lease the ORIGINAL epoch is visible — the bump rolled back.
    assert_eq!(snapshot(&store, "u1"), before_u1);
    assert_epoch_integer(&store, "u1", 0);
    assert_user_count(&store, 2);
    assert_no_library_rows(&store);

    // Act: a domain failure from the primitive itself (missing user) rolls back
    // the successful revoke in the same transaction.
    let missing: Result<NewTokenEpoch, DbRuntimeError<CallerError>> =
        store.try_with_tx_sync(|tx| {
            let epoch = revoke_tokens_in_tx(tx, "u1").map_err(revoke_failed)?;
            assert_eq!(epoch.value(), 1, "the bump ran before the failing revoke");
            // The missing-user revoke fails the transaction the successful bump ran in.
            revoke_tokens_in_tx(tx, "ghost")
                .map_err(revoke_failed)
                .inspect(|published| panic!("a missing user must never publish epoch {published}"))
        });
    match missing {
        Err(DbRuntimeError::Domain(CallerError::Revoke {
            source: DbOperationError::Domain(RevokeError::NotFound { id }),
        })) => assert_eq!(id, "ghost"),
        other => panic!("expected the wrapped NotFound, got {other:?}"),
    }
    assert_epoch_integer(&store, "u1", 0);
    assert_user_count(&store, 2);

    // Act: a SQLite-arm failure later in the same transaction behaves the same way.
    let sqlite_failure: Result<NewTokenEpoch, DbRuntimeError<CallerError>> = store
        .try_with_tx_sync(|tx| {
            let epoch = revoke_tokens_in_tx(tx, "u1").map_err(revoke_failed)?;
            tx.execute("INSERT INTO nonexistent_table(value) VALUES('x')", [])
                .map_err(DbOperationError::sqlite)?;
            Ok(epoch)
        });
    assert!(matches!(
        sqlite_failure,
        Err(DbRuntimeError::Sqlite {
            stage: TransactionStage::Body,
            ..
        })
    ));
    assert_eq!(snapshot(&store, "u1"), before_u1);

    // Act: a successful bump followed by a malformed-cell revoke (SQLite-arm failure
    // from the primitive itself) rolls the bump back, and the malformed cell is
    // untouched — the caller does not have to abort for the guard to hold, but an
    // aborting caller publishes neither the bump nor any coercion.
    fixture_exec(
        &store,
        "INSERT INTO \"user\" (id, user_name, name, email, password, is_admin, \
         created_at, updated_at, scrobble_filter, token_epoch) \
         VALUES ('u3', 'u3', 'n', 'e', 'CIPHER', 0, ?, ?, '', 4503599627370495.5)",
        &[text(T0), text(T1)],
    );
    let damaged_failure: Result<NewTokenEpoch, DbRuntimeError<CallerError>> = store
        .try_with_tx_sync(|tx| {
            let epoch = revoke_tokens_in_tx(tx, "u1").map_err(revoke_failed)?;
            assert_eq!(epoch.value(), 1, "the bump ran before the failing revoke");
            revoke_tokens_in_tx(tx, "u3")
                .map_err(|source| DbOperationError::Domain(CallerError::Revoke { source }))
                .inspect(|published| {
                    panic!("a malformed cell must never publish epoch {published}")
                })
        });
    match damaged_failure {
        Err(DbRuntimeError::Domain(CallerError::Revoke {
            source: DbOperationError::Sqlite(_),
        })) => {}
        Err(error) => panic!("expected a wrapped SQLite failure, got {error:?}"),
        Ok(epoch) => panic!("a malformed cell must never publish epoch {epoch}"),
    }
    // Assert: the bump rolled back and the malformed cell survived the rollback.
    assert_eq!(snapshot(&store, "u1"), before_u1);
    assert_epoch_integer(&store, "u1", 0);
    assert_eq!(epoch_storage(&store, "u3"), "real");
    assert_eq!(
        scalar::<SqlValue>(
            &store,
            &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = 'u3'")
        ),
        SqlValue::Real(4_503_599_627_370_495.5),
        "the failing revoke must not coerce the malformed cell"
    );
    assert_user_count(&store, 3);

    fs::remove_file(path).ok();
}

#[test]
fn epoch_is_advisory_until_the_caller_commits() {
    let (store, path) = fresh_store("advisory", 2);
    seed_user(&store, "u1", "CIPHER", None);

    // Arrange: an independent read-only handle on the same file, so "not yet
    // observable" is measured from OUTSIDE the revoking transaction.
    let observer = nd_dbschema::schema::open(&format!("file:{}?mode=ro", path.display())).unwrap();
    let observed = || {
        observer
            .query_row(
                &format!("SELECT token_epoch FROM {USER_TABLE} WHERE id = 'u1'"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_else(|e| panic!("observer read: {e}"))
    };
    assert_eq!(observed(), 0);

    // Act: revoke inside a transaction on its own handle that is NOT committed.
    {
        let mut conn = raw_conn(&path);
        let tx = conn.transaction().unwrap();
        let epoch = revoke_tokens_in_tx(&tx, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
        assert_eq!(epoch.value(), 1, "the primitive reports the new epoch");
        // Assert: uncommitted, so an outside reader still sees the old epoch.
        assert_eq!(observed(), 0, "no epoch may be observable before commit");
        drop(tx); // rusqlite's default DropBehavior rolls the transaction back.
    }
    assert_epoch_integer(&store, "u1", 0);

    // Act: commit for real through the typed seam.
    let committed = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
    assert_eq!(committed.value(), 1);
    // Assert: the committed epoch is observable on a fresh lease.
    assert_eq!(observed(), 1);
    assert_epoch_integer(&store, "u1", 1);

    fs::remove_file(path).ok();
}

#[test]
fn concurrent_revokes_over_the_pool_commit_one_bump_each() {
    let (store, path) = fresh_store("concurrent-pool", 4);
    seed_user(&store, "u1", "CIPHER", None);

    // Arrange: four threads released from one barrier so they overlap. The pool
    // serializes the writers and the retry seam absorbs any BUSY refusal with its
    // PRODUCTION backoff (`store.rs:305`, 5s/10s/15s); no assertion depends on that
    // timing, because a successful commit never waits.
    let start = Arc::new(Barrier::new(4));
    let handles = (0..4)
        .map(|index| {
            let store = store.clone();
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                store
                    .try_with_tx_retry_sync(|tx| revoke_tokens_in_tx(tx, "u1"))
                    .unwrap_or_else(|e| panic!("thread {index} revoke must commit: {e}"))
                    .value()
            })
        })
        .collect::<Vec<_>>();

    // Act: join and collect each committed epoch.
    let mut epochs = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| panic!("revoke thread panicked"))
        })
        .collect::<Vec<_>>();
    epochs.sort_unstable();

    // Assert: base + 4 with the four DISTINCT epochs 1..=4 — no lost update and no
    // double bump.
    assert_eq!(
        epochs,
        vec![1, 2, 3, 4],
        "each revoke commits exactly one bump"
    );
    assert_epoch_integer(&store, "u1", 4);
    assert_user_count(&store, 1);
    assert_no_library_rows(&store);

    fs::remove_file(path).ok();
}

/// Two independent connections on ONE file — the shape the pool lends
/// (`pool.rs:123`, opened `SQLITE_OPEN_NO_MUTEX`). Writer A begins and revokes, then
/// HANDSHAKES on a bounded channel that its write state is held; writer B does not
/// start its attempt until that message arrives, so the attempt definitely lands
/// inside A's transaction and SQLite must serialize them. A then waits for B's
/// release message and commits. No sleep is involved: B waits through SQLite's own
/// bounded busy handler, and its retry seam runs with an injected zero delay.
#[test]
fn revokes_from_two_connections_are_serialized_with_no_lost_updates() {
    let path = temp_db_path("concurrent-raw");
    let db_path = path.display().to_string();
    let dsn = format!("file:{db_path}?mode=rwc&_foreign_keys=on");

    let writer_a = nd_dbschema::schema::open(&dsn).unwrap();
    apply_fresh_schema(&writer_a).unwrap();
    // Arrange: the row whose epoch is revoked, seeded through the raw handle.
    seed_user_raw(&writer_a, "u1");
    let writer_b = nd_dbschema::schema::open(&dsn).unwrap();
    // B lets SQLite's own busy handler wait for the lock (a bounded internal wait,
    // not a test sleep), so contention is absorbed rather than flaked away; the
    // retry seam below still runs with an injected ZERO delay, so a retry that does
    // happen costs nothing instead of waiting out the 5s driver default.
    writer_b.busy_timeout(Duration::from_millis(2_000)).unwrap();

    let start = Arc::new(Barrier::new(2));
    // A → B: "the write lock is held". B → A: "attempt started, release now".
    // Bounded receives: a failure before the send wakes the other thread with a
    // timeout error instead of blocking it forever.
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

    // A: begin, revoke, announce the held lock, wait for B's release, commit.
    let mut conn_a = writer_a;
    let thread_a = {
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            start.wait();
            let mut tx = conn_a.transaction().unwrap();
            let epoch = revoke_tokens_in_tx(&tx, "u1").unwrap_or_else(|e| panic!("A revoke: {e}"));
            held_tx
                .send(())
                .unwrap_or_else(|e| panic!("A could not announce the held lock: {e}"));
            release_rx.recv_timeout(LOCK_HANDSHAKE).unwrap_or_else(|e| {
                panic!("A never got B's release signal (B never reached the attempt): {e}")
            });
            tx.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            match tx.execute_batch("COMMIT") {
                Ok(()) => epoch.value(),
                Err(cause) => panic!("A commit must succeed: {cause}"),
            }
        })
    };

    // B: only after the announcement, attempt the same revocation while A holds it.
    let mut conn_b = writer_b;
    let thread_b = {
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            start.wait();
            held_rx.recv_timeout(LOCK_HANDSHAKE).unwrap_or_else(|e| {
                panic!("B never saw a held lock (A failed before announcing it): {e}")
            });
            // The attempt is issued while A's transaction is still open: A is still
            // waiting on `release_rx` at this point.
            release_tx
                .send(())
                .unwrap_or_else(|e| panic!("B could not signal its attempt: {e}"));
            revoke_via_raw_conn(&mut conn_b, "u1")
                .unwrap_or_else(|e| panic!("B revoke must commit after retries: {e}"))
                .value()
        })
    };

    let epoch_a = thread_a
        .join()
        .unwrap_or_else(|_| panic!("writer A panicked"));
    let epoch_b = thread_b
        .join()
        .unwrap_or_else(|_| panic!("writer B panicked"));

    // Assert: the two bumps are serialized in commit order — A's committed first at
    // epoch 1, B's after the lock was released at epoch 2. Distinct values with no
    // gap are the proof of no lost update and no double bump.
    assert_eq!(
        epoch_a, 1,
        "the first committed revoke must publish epoch 1; observed {epoch_a}, B saw {epoch_b}"
    );
    assert_eq!(
        epoch_b, 2,
        "the serialized revoke must publish epoch 2, not repeat 1"
    );
    assert_epoch_integer_from_file(&db_path, "u1", 2);

    fs::remove_file(path).ok();
}

/// A held write lock refuses the revocation with BUSY, which the DB-00 retry seam
/// classifies as retryable and surfaces as `BusyExhausted` — never as a domain
/// miss. Deterministic and same-thread: zero busy timeout on both handles plus a
/// zero injected retry delay, so nothing waits on wall-clock timing and the
/// `attempts` count proves every refused attempt really hit the held lock.
#[test]
fn a_held_write_lock_refuses_with_busy_and_is_never_mistaken_for_notfound() {
    let path = temp_db_path("busy");
    let db_path = path.display().to_string();
    let dsn = format!("file:{db_path}?mode=rwc&_foreign_keys=on");

    let holder = nd_dbschema::schema::open(&dsn).unwrap();
    apply_fresh_schema(&holder).unwrap();
    // Arrange: the row both connections revoke, seeded through the raw handle.
    seed_user_raw(&holder, "u1");
    let contender = nd_dbschema::schema::open(&dsn).unwrap();
    holder.busy_timeout(Duration::ZERO).unwrap();
    let mut conn_holder = holder;
    let mut conn_contender = contender;
    conn_contender.busy_timeout(Duration::ZERO).unwrap();

    // Arrange: A holds an uncommitted write transaction.
    let mut tx_a = conn_holder.transaction().unwrap();
    let epoch = revoke_tokens_in_tx(&tx_a, "u1").unwrap_or_else(|e| panic!("holder revoke: {e}"));
    assert_eq!(epoch.value(), 1);

    // Act: B attempts the same revocation while A's lock is held.
    let outcome = revoke_via_raw_conn(&mut conn_contender, "u1");

    // Assert: exhausted BUSY with a BUSY-classified source, and never a domain
    // result — the retry seam reports the stage of the final busy failure.
    match &outcome {
        Err(DbRuntimeError::BusyExhausted {
            attempts,
            stage,
            source,
        }) => {
            assert_eq!(*attempts, TX_MAX_RETRIES + 1);
            assert!(
                matches!(stage, TransactionStage::Begin | TransactionStage::Body),
                "the refusal stage is {stage}"
            );
            assert!(is_busy(source), "the refusal must be BUSY-class: {source}");
        }
        Err(DbRuntimeError::Domain(error)) => {
            panic!("a lock refusal must never be reported as {error:?}");
        }
        Ok(value) => panic!("B must be refused while A holds the lock, got {value}"),
        other => panic!("expected BusyExhausted, got {other:?}"),
    }

    // Assert: B's refused attempts published nothing (each attempt rolled back).
    assert_eq!(
        epoch_from_file(&db_path, "u1").0,
        0,
        "nothing committed yet",
    );

    // Act: A commits, then B retries with the lock released.
    tx_a.set_drop_behavior(rusqlite::DropBehavior::Rollback);
    tx_a.execute_batch("COMMIT").unwrap();
    assert_epoch_integer_from_file(&db_path, "u1", 1);
    let released = revoke_via_raw_conn(&mut conn_contender, "u1")
        .unwrap_or_else(|e| panic!("post-release revoke must commit: {e}"));
    assert_eq!(released.value(), 2);
    assert_epoch_integer_from_file(&db_path, "u1", 2);

    fs::remove_file(path).ok();
}

#[test]
fn revoke_is_not_a_password_put_and_password_put_behavior_is_unchanged() {
    let (store, path) = fresh_store("put-parity", 2);
    // Arrange: create the user through the PRODUCTION `put`, so the fixture row is
    // a real repository row with a real password.
    let created = lease(&store, |conn| {
        UserRepo::new(conn)
            .put(&put_values("u1", Some("CIPHER-A"), T1), T2, false)
            .unwrap_or_else(|e| panic!("put: {e}"))
    });
    assert!(created.is_new_user);
    // Go's password arm finds no row for a new user, so no epoch is returned
    // (`user_repository.go:135-141`) and the INSERT takes the column DEFAULT 0.
    assert_eq!(created.token_epoch, None);
    assert_epoch_integer(&store, "u1", 0);

    // Non-vacuity control 1: a no-password `put` leaves the epoch untouched, so a
    // "revoke" test that merely reused `put` could not pass.
    let before = snapshot(&store, "u1");
    let no_password = lease(&store, |conn| {
        UserRepo::new(conn)
            .put(&put_values("u1", None, T1), T2, false)
            .unwrap_or_else(|e| panic!("put: {e}"))
    });
    assert!(!no_password.is_new_user);
    assert_eq!(
        no_password.token_epoch, None,
        "a password-less Put returns no epoch (user_repository.go:129-144)"
    );
    assert_eq!(
        snapshot(&store, "u1").token_epoch,
        before.token_epoch,
        "a password-less Put must not bump the epoch"
    );

    // Non-vacuity control 2: a password `put` DOES bump, but it also rewrites the
    // password and stamps `updated_at` — exactly what a revoke must not do.
    let before_password_put = snapshot(&store, "u1");
    let with_password = lease(&store, |conn| {
        UserRepo::new(conn)
            .put(&put_values("u1", Some("CIPHER-B"), T2), T2, true)
            .unwrap_or_else(|e| panic!("put: {e}"))
    });
    assert_eq!(
        with_password.token_epoch,
        Some(before_password_put.token_epoch + 1)
    );
    assert!(with_password.refresh_inflight_token);
    let after_password_put = snapshot(&store, "u1");
    assert_eq!(after_password_put.password, "CIPHER-B");
    assert_eq!(after_password_put.updated_at, T2);

    // Act: the revoke changes ONLY the epoch.
    let epoch = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
    assert_eq!(epoch.value(), after_password_put.token_epoch + 1);
    let mut expected = after_password_put.clone();
    expected.token_epoch = epoch.value();
    assert_eq!(snapshot(&store, "u1"), expected);

    // Assert: the password arm's renderer is unchanged — full row SET, Go's
    // unguarded bump (`user_repository.go:136-137`), and the RETURNING leg.
    let (sql, args) = put_password_update_sql(&put_values("u1", Some("CIPHER-C"), T2));
    assert!(sql.contains("password = ?"));
    assert!(sql.contains("token_epoch = token_epoch + 1"));
    assert!(sql.ends_with("RETURNING token_epoch"));
    assert!(
        !sql.contains("token_epoch <"),
        "the password arm keeps Go's unguarded bump"
    );
    assert_eq!(args.len(), 12, "11 SET columns plus the WHERE id");

    fs::remove_file(path).ok();
}

#[test]
fn the_primitive_is_reachable_only_through_a_transaction() {
    let path = temp_db_path("tx-only");
    let mut conn = nd_dbschema::schema::open(&format!(
        "file:{}?mode=rwc&_foreign_keys=on",
        path.display()
    ))
    .unwrap();
    apply_fresh_schema(&conn).unwrap();

    // Act: the only entry point takes a `rusqlite::Transaction`. `&Connection` does
    // not coerce to it, so an autocommit revoke is structurally impossible (pinned
    // by the `compile_fail` doctest on `revoke_tokens_in_tx`).
    let tx = conn.transaction().unwrap();
    let outcome = revoke_tokens_in_tx(&tx, "u1");
    assert_domain(
        outcome,
        |error| matches!(error, RevokeError::NotFound { id } if id == "u1"),
        "raw tx on an empty table",
    );
    drop(tx);

    // Assert: the rolled-back call published nothing.
    assert_eq!(
        conn.query_row(&format!("SELECT count(*) FROM {USER_TABLE}"), [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );

    fs::remove_file(path).ok();
}

/// Reachability is pinned from the other side too: the untyped store seams
/// (`SQLStore::with_tx_sync`, `with_connection_sync`) hand back a bare
/// `rusqlite::Error`/`StoreError`, so `DbOperationError<RevokeError>` cannot be
/// carried through them at all — the typed seam is the only driver, and the
/// `compile_fail` doctest on `revoke_tokens_in_tx` pins that a `&Connection` is
/// not an accepted argument. Below, the untyped seams drive the same rows and are
/// shown to leave the epoch exactly where the revoke left it.
#[test]
fn the_untyped_store_seam_leaves_the_epoch_untouched() {
    let (store, path) = fresh_store("untyped-seam", 1);
    seed_user(&store, "u1", "CIPHER", None);
    let before = snapshot(&store, "u1");
    let seeded = revoke_via_store(&store, "u1").unwrap_or_else(|e| panic!("revoke: {e}"));
    assert_eq!(seeded.value(), 1);
    let after_revoke = snapshot(&store, "u1");

    // Act: drive the untyped seams over the same rows, never reaching the primitive.
    let untyped: Result<(), StoreError> = store.with_connection_sync(|conn| {
        let (sql, args) = select_token_epoch_sql("u1");
        let epoch = conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
            row.get::<_, i64>(0)
        })?;
        assert_eq!(epoch, 1, "the committed epoch is what the seam reads");
        Ok(())
    });
    assert!(untyped.is_ok());
    let untyped_tx: Result<(), StoreError> = store.with_tx_sync(|tx| {
        tx.execute(
            &format!("UPDATE {USER_TABLE} SET last_login_at = ? WHERE id = ?"),
            [T2, "u1"],
        )?;
        Ok(())
    });
    assert!(untyped_tx.is_ok());

    // Assert: an untyped transaction stamps a timestamp column but leaves the epoch
    // exactly where the revoke left it — no hidden autocommit bump exists.
    assert_epoch_integer(&store, "u1", 1);
    let after_untyped = snapshot(&store, "u1");
    assert_eq!(after_untyped.token_epoch, after_revoke.token_epoch);
    assert_eq!(after_untyped.last_login_at.as_deref(), Some(T2));
    assert_eq!(after_untyped.updated_at, before.updated_at);
    assert_user_count(&store, 1);
    assert_no_library_rows(&store);

    fs::remove_file(path).ok();
}
