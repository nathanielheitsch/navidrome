//! BOOT-01 increment D: synchronous DB-open/migrate assembly acceptance tests.
//!
//! Every case runs against synthetic, pre-created temp directories with an
//! injected startup cwd, so no test touches a production data directory, the real
//! process cwd, or the real environment. No test calls `chdir`, and every DB name
//! carries a unique id, so the suite stays parallel-safe; `tempfile::TempDir`
//! removes each tree on drop.
//!
//! Coverage against `boot-api.md:194` (the DB half of step 3) and this lane's
//! decision record:
//! * fresh open migrates once; a reopen reports an unchanged migration count and
//!   runs no DDL statement — proved non-vacuously by pre-seeding a marker-only DB,
//!   so "count stayed 1" cannot be satisfied by an accidental re-migration;
//! * **every** pool handle, held simultaneously on separate threads, carries the
//!   migrated schema, the per-connection extensions, and identical effective
//!   FK/busy/synchronous/journal state — including the handle that ran the
//!   migration, whose `PRAGMA foreign_keys` the DDL toggled;
//! * pool size 1 is usable after admission;
//! * bare `:memory:` survives the drop of the migration anchor; a named
//!   `mode=memory&cache=shared` URI is shared across handles; every private-memory
//!   shape is refused **before** opening;
//! * a relative plain path is created under the injected cwd, not the process cwd,
//!   and never under a filename carrying the query;
//! * an escaped absolute `file:` URI is preserved (SQLite decodes `%2F`);
//! * a malformed DSN and a real migration failure each yield a safe error and no
//!   store: static stage/class, numeric SQLite codes, no path, query or source;
//! * a missing parent directory or a read-only parent is reported, never created;
//! * Unicode, symlinked and relative-with-query paths anchor correctly.
//!
//! Permission limitation, stated rather than hidden: the unreadable-directory
//! assertion below runs as an unprivileged user, so the mode bits genuinely deny
//! access. Under root the mode bits are bypassed and the case would open
//! successfully; the assertions that hold either way (no file created, no directory
//! invented) are the ones that pin behavior.

use std::error::Error as StdError;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use nd_cli::database::{
    open_database, resolve_input, DatabaseError, DatabaseOptions, DbFailureClass, DbStage,
    OpenedDatabase,
};
use nd_db::pool::pool_size;
use nd_db::store::{SQLStore, StoreError};
use nd_dbschema::schema::{effective_state, open, EffectiveState};

/// Absolute path text inside a directory.
fn in_dir(dir: &Path, name: &str) -> String {
    dir.join(name)
        .to_str()
        .expect("synthetic path is UTF-8")
        .to_string()
}

/// Unique DB name, so two tests in the same process never share a file.
fn name(tag: &str) -> String {
    format!("{tag}-{}.db", nd_id::new_random())
}

/// Basename of a DB path.
fn basename(db_path: &str) -> String {
    db_path
        .rsplit('/')
        .next()
        .expect("absolute path has a basename")
        .to_string()
}

/// A pre-created temp parent directory. Nothing here creates a directory a
/// database would need.
fn temp_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("nd-cli-database-{tag}-"))
        .tempdir()
        .expect("create isolated temp parent")
}

/// Canonicalized directory, for comparing against SQLite's own canonicalization.
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).expect("canonicalize temp parent")
}

/// `Config::db_path`'s default shape (`nd-conf/src/consts.rs:16`), spelled with a
/// caller-chosen filename so it can be anchored to an injected cwd.
fn default_shape(file: &str) -> String {
    let query = nd_conf::consts::DEFAULT_DB_PATH
        .split('?')
        .nth(1)
        .expect("the default DSN carries its parameter string");
    format!("{file}?{query}")
}

/// Open a store with the given pool size.
fn open_store(db_path: &str, startup_cwd: &Path, size: usize) -> OpenedDatabase {
    open_database(DatabaseOptions {
        db_path,
        startup_cwd,
        pool_size: size,
    })
    .unwrap_or_else(|e| panic!("open_database({db_path}) failed: {e}"))
}

/// Migration ids plus the post-migration marker row, read through the store.
fn migration_state(store: &SQLStore) -> (usize, Option<String>) {
    let ids = store
        .with_connection_sync(|conn| {
            let mut stmt = conn.prepare("SELECT version_id FROM goose_db_version")?;
            let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<i64>, rusqlite::Error>>()
        })
        .expect("read migration ids");
    let flag = store
        .with_connection_sync(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM property WHERE id = 'FullScanAfterMigration'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok())
        })
        .expect("read marker row");
    (ids.len(), flag)
}

/// Effective state plus migrated-schema and extension evidence from one handle.
struct HandleState {
    effective: EffectiveState,
    logical_tables: usize,
    /// Whether SEEDEDRAND resolved at all (it is an i64, deliberately different per
    /// handle, so only presence is comparable across handles).
    seedrand_present: bool,
    /// SEEDEDRAND called twice on the same handle: identical, because the hasher is
    /// connection-scoped and caches its seed.
    seedrand_stable: bool,
    natsort: bool,
}

/// Logical table count: the fresh-schema golden count (`nd-dbschema`'s
/// `fresh_schema.rs` pins 34 = 31 tables + 3 FTS5 virtual tables) plus the two
/// runtime artifacts the migration adds — `goose_db_version` and SQLite's own
/// `sqlite_sequence` — measured here as 36 with the FTS shadow tables filtered.
fn logical_tables(conn: &rusqlite::Connection) -> usize {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("list tables");
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("read table names");
    names
        .map(|n| n.expect("table name"))
        .filter(|n| {
            !n.ends_with("_config")
                && !n.ends_with("_data")
                && !n.ends_with("_docsize")
                && !n.ends_with("_idx")
        })
        .count()
}

fn handle_state(conn: &rusqlite::Connection) -> HandleState {
    HandleState {
        effective: effective_state(conn).expect("effective state"),
        logical_tables: logical_tables(conn),
        seedrand_present: conn
            .query_row("SELECT SEEDEDRAND('album-1', 'song-1')", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_ok(),
        seedrand_stable: {
            let first = conn.query_row(nd_dbschema::schema::SEEDEDRAND_SQL, [], |r| {
                r.get::<_, i64>(0)
            });
            let second = conn.query_row(nd_dbschema::schema::SEEDEDRAND_SQL, [], |r| {
                r.get::<_, i64>(0)
            });
            first.is_ok() && first == second
        },
        natsort: conn
            .query_row("SELECT 'track2' COLLATE NATSORT < 'track10'", [], |row| {
                row.get(0)
            })
            .expect("NATSORT"),
    }
}

fn state_of(store: &SQLStore) -> HandleState {
    store
        .with_connection_sync(|conn| Ok(handle_state(conn)))
        .expect("read handle state")
}

/// Extended SQLite code of a rusqlite failure, 0 when it has none.
fn extended_code(err: &rusqlite::Error) -> i32 {
    match err {
        rusqlite::Error::SqliteFailure(code, _) => code.extended_code,
        _ => 0,
    }
}

/// Full error text including every source, used to prove nothing leaks.
fn full_text(err: &DatabaseError) -> String {
    let mut text = format!("{err} | debug {err:?}");
    let mut source = StdError::source(err);
    while let Some(s) = source {
        text.push_str(&format!(" | source {s} | source-debug {s:?}"));
        source = s.source();
    }
    text
}

/// Assert the error text carries no trace of the input, its parent directory, or
/// any database filename.
fn assert_no_leak(err: &DatabaseError, db_path: &str, parent: &Path) {
    let text = full_text(err);
    assert!(
        !text.contains(db_path),
        "error must not echo the input {db_path:?}: {text}"
    );
    let parent_text = parent.to_str().expect("synthetic parent is UTF-8");
    assert!(
        !text.contains(parent_text),
        "error must not echo the parent directory {parent_text:?}: {text}"
    );
    assert!(
        !text.contains(".db"),
        "error must not name a database file: {text}"
    );
    assert!(
        !text.contains("unable to open"),
        "error must not forward SQLite's own message: {text}"
    );
}

#[test]
fn fresh_open_migrates_once_and_reopen_reports_the_same_count() {
    // Arrange: an empty, existing parent directory.
    let dir = temp_dir("fresh");
    let db = in_dir(dir.path(), &name("fresh"));

    // Act: first open on a brand-new database.
    let first = open_store(db.as_str(), dir.path(), 2);
    assert!(
        first.target.migrated_on_open,
        "the first open must report that it applied the fresh migration"
    );
    assert_eq!(
        first.target.migration_count, 1,
        "exactly one fresh migration"
    );
    assert_eq!(first.target.pool_size, 2, "two handles owned");
    let (ids, flag) = migration_state(&first.store);
    assert_eq!(ids, 1, "one goose_db_version row after a fresh open");
    assert_eq!(
        flag.as_deref(),
        Some("1"),
        "the post-migration full-scan flag is set by the migration (Go db/db.go:91-95)"
    );
    let first_file = first
        .store
        .with_connection_sync(|conn| effective_state(conn).map(|s| s.db_file))
        .expect("read db file")
        .expect("a file DB reports its canonical file");
    assert_eq!(
        first_file,
        canonical(dir.path())
            .join(basename(db.as_str()))
            .to_str()
            .unwrap(),
        "SQLite canonicalized the supplied absolute path"
    );

    // Act again on the same file.
    let second = open_store(db.as_str(), dir.path(), 2);
    assert!(
        !second.target.migrated_on_open,
        "a reopen must report that it did NOT apply the migration"
    );
    assert_eq!(
        second.target.migration_count, first.target.migration_count,
        "reopen migration count must be unchanged"
    );
    let (ids2, flag2) = migration_state(&second.store);
    assert_eq!(ids2, 1, "reopen must not add a migration row");
    assert_eq!(
        flag2.as_deref(),
        Some("1"),
        "marker row survives the reopen"
    );
    let second_file = second
        .store
        .with_connection_sync(|conn| effective_state(conn).map(|s| s.db_file))
        .expect("read db file")
        .expect("a file DB reports its canonical file");
    assert_eq!(
        first_file, second_file,
        "both opens address the same canonical database file"
    );
}

#[test]
fn reopen_of_a_marker_only_db_runs_no_ddl_statement() {
    // Arrange: a database whose marker says "already migrated" although no DDL
    // ever ran. This makes the reopen assertion non-vacuous: if the migration were
    // re-applied, the fresh tables would appear and a second goose row could.
    let dir = temp_dir("marker-only");
    let db = in_dir(dir.path(), &name("marker"));
    let raw = open(db.as_str()).expect("open raw marker db");
    raw.execute(
        "CREATE TABLE goose_db_version (version_id integer NOT NULL, applied_at TEXT)",
        [],
    )
    .expect("create marker table");
    raw.execute("INSERT INTO goose_db_version (version_id) VALUES (1)", [])
        .expect("record marker row");
    drop(raw);

    // Act.
    let opened = open_store(db.as_str(), dir.path(), 1);

    // Assert: no DDL ran, so the fresh schema is absent and the count is still 1.
    assert!(
        !opened.target.migrated_on_open,
        "marker present => no migration is reported as run"
    );
    assert_eq!(opened.target.migration_count, 1, "no second migration row");
    let (ids, flag) = migration_state(&opened.store);
    assert_eq!(ids, 1, "marker untouched");
    assert_eq!(
        flag, None,
        "no property row was written, so no DDL ran on the reopen"
    );
    let tables = opened
        .store
        .with_connection_sync(|conn| Ok(logical_tables(conn)))
        .expect("count tables");
    assert_eq!(
        tables, 1,
        "only goose_db_version exists: a reopen must not create the fresh schema"
    );
}

#[test]
fn every_pool_handle_holds_the_migrated_schema_and_effective_settings() {
    // Arrange: production-shaped pool size on Navidrome's default DSN shape
    // (WAL / 15000 / NORMAL / FK-on), anchored to an injected cwd.
    let dir = temp_dir("all-handles");
    let db = default_shape(name("handles").as_str());
    let opened = open_store(db.as_str(), dir.path(), pool_size());
    let size = opened.target.pool_size;
    assert!(
        size >= 4,
        "pool_size floor is 4 (conf/configuration.go:1217-1218)"
    );
    assert!(
        opened.target.anchored_to_startup_cwd,
        "relative input anchored"
    );

    // Act: hold every handle at the same time, one per thread, released only after
    // a barrier has seen all of them. A pool that had lost the migration handle
    // would block here instead of completing.
    let barrier = std::sync::Barrier::new(size);
    std::thread::scope(|scope| {
        let joins: Vec<_> = (0..size)
            .map(|_| {
                scope.spawn(|| {
                    opened
                        .store
                        .with_connection_sync(|conn| {
                            barrier.wait();
                            Ok(handle_state(conn))
                        })
                        .expect("hold a handle past the barrier")
                })
            })
            .collect();
        let states: Vec<HandleState> = joins
            .into_iter()
            .map(|j| j.join().expect("thread"))
            .collect();

        // Assert: all `size` handles were live simultaneously, with one identical
        // effective state — including the handle that ran the migration, whose
        // foreign_keys the DDL toggled and must have restored.
        assert_eq!(states.len(), size, "every handle held simultaneously");
        let want = &states[0].effective;
        assert_eq!(
            want.journal_mode, "wal",
            "file DB on the default DSN is WAL"
        );
        assert_eq!(want.busy_timeout, 15000, "Navidrome busy timeout 15000 ms");
        assert_eq!(want.synchronous, 1, "synchronous = NORMAL (1)");
        assert!(
            want.foreign_keys,
            "foreign_keys on: restored after the DDL toggle"
        );
        for state in &states {
            assert_eq!(
                &state.effective, want,
                "every handle holds identical settings"
            );
            assert_eq!(
                state.logical_tables, 36,
                "migrated schema visible from every handle"
            );
            assert!(state.natsort, "NATSORT collation registered per handle");
            // SEEDEDRAND is registered per handle but deliberately NOT equal across
            // handles: it keeps a connection-scoped hasher, matching Go's
            // `RegisterFunc(..., false)` (schema.rs:1739-1744, db/db.go:46). The
            // assertion is that the function exists on every handle and that it is
            // stable within one handle.
            assert!(
                state.seedrand_present,
                "SEEDEDRAND resolves on every handle"
            );
            assert!(
                state.seedrand_stable,
                "SEEDEDRAND is stable within one handle"
            );
        }
    });

    // Assert FK enforcement is live on a pooled handle (media_file.library_id
    // references library, which exists but holds no row 9).
    let fk = opened.store.with_connection_sync(|conn| {
        conn.execute(
            "INSERT INTO media_file (id, path, library_id) VALUES('fk-probe', 'x.mp3', 9)",
            [],
        )
    });
    let fk_code = fk
        .err()
        .map(|e| match e {
            StoreError::Other(inner) => extended_code(&inner),
            other => panic!("unexpected FK failure: {other:?}"),
        })
        .expect("FK violation expected");
    assert_eq!(
        fk_code,
        rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
        "foreign keys are enforced on a pooled handle"
    );
}

#[test]
fn pool_size_one_store_is_usable_after_admission() {
    // Arrange: the BOOT-01 caller-transaction shape — exactly one handle.
    let dir = temp_dir("size-one");
    let opened = open_store(in_dir(dir.path(), &name("one")).as_str(), dir.path(), 1);

    // Assert: the single handle is idle after admission, and a transaction on it
    // commits and reads back.
    assert_eq!(opened.target.pool_size, 1, "pool size 1 is honoured");
    assert_eq!(
        opened.store.idle_count().expect("idle count"),
        1,
        "the admitted handle is idle, not held out"
    );
    opened
        .store
        .with_tx_sync(|tx| tx.execute("INSERT INTO property (id, value) VALUES('k', 'v')", []))
        .expect("transaction on the size-1 store");
    let value = opened
        .store
        .with_connection_sync(|conn| {
            conn.query_row("SELECT value FROM property WHERE id = 'k'", [], |row| {
                row.get::<_, String>(0)
            })
        })
        .expect("read back");
    assert_eq!(value, "v", "size-1 store commits and reads back");
    // Depletion (a second checkout while the only handle is leased) is pinned by
    // nd-db's own pool suite — `SQLStore` exposes no synchronous checkout, so a
    // blocked-checkout assertion here would have to block forever.
}

#[test]
fn bare_memory_survives_the_drop_of_the_migration_anchor() {
    // Act: bare `:memory:`. `open_database` returns after dropping its anchor, so
    // reaching the assertions at all proves the pool alone keeps the DB alive.
    let opened = open_store(":memory:", Path::new("/"), 2);
    assert!(opened.target.is_memory, "bare :memory: is a memory target");
    assert!(
        opened.target.migrated_on_open,
        "fresh memory DB is migrated"
    );
    assert!(
        !opened.target.anchored_to_startup_cwd,
        ":memory: is SQLite's special name and is never anchored to the cwd"
    );

    // Assert: the migrated schema is still there, from two more handles.
    let first = state_of(&opened.store);
    assert_eq!(
        first.logical_tables, 36,
        "fresh schema survives the anchor drop"
    );
    assert_eq!(first.effective.journal_mode, "memory", "memory journal");
    assert!(
        first.effective.db_file.is_none(),
        "a memory DB reports no file"
    );
    let second = state_of(&opened.store);
    assert_eq!(
        second.effective, first.effective,
        "memory DB shared across handles"
    );
}

#[test]
fn named_shared_memory_uri_is_shared_across_handles() {
    let dir = temp_dir("named-shared");
    // A named shared-cache memory database: SQLite keeps one named memory DB
    // visible to every handle asking for the same name with cache=shared. Its
    // path component is relative, but a memory URI has no filesystem location to
    // anchor, so it is accepted as written.
    let db = format!("file:{}?mode=memory&cache=shared", name("shared"));
    let opened = open_store(db.as_str(), dir.path(), 2);
    assert!(opened.target.is_memory, "a memory URI is a memory target");
    assert!(
        !opened.target.anchored_to_startup_cwd,
        "a memory URI is passed through, not anchored"
    );

    opened
        .store
        .with_tx_sync(|tx| tx.execute("CREATE TABLE shared_probe (k TEXT PRIMARY KEY)", []))
        .expect("create on handle A");
    let found = opened
        .store
        .with_connection_sync(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'shared_probe')",
                [],
                |row| row.get::<_, bool>(0),
            )
        })
        .expect("probe on handle B");
    assert!(found, "named shared-cache memory is visible across handles");

    // Both handles live at once, and both report the same (absent) file.
    let a = state_of(&opened.store);
    let b = state_of(&opened.store);
    assert_eq!(
        a.effective, b.effective,
        "both handles address one memory DB"
    );
    assert!(a.effective.db_file.is_none(), "memory DB has no file");
}

#[test]
fn private_memory_shapes_are_refused_before_opening() {
    // Each input is memory-shaped but would give the pool independent databases.
    let dir = temp_dir("private-memory");
    let cases: Vec<(&str, &str)> = vec![
        (
            ":memory:?_fk=off",
            "a non-URI query is stripped; SQLite receives plain private :memory:",
        ),
        (
            ":memory:?cache=shared",
            "stripped the same way, so still private (db/db.go:53-56 is exact equality)",
        ),
        (
            "file::memory:",
            "no cache=shared: private unless its own URI asks for it",
        ),
        (
            Box::leak(format!("file:{}?mode=memory", name("priv")).into_boxed_str()),
            "mode=memory without cache=shared is private",
        ),
        (
            Box::leak(
                format!(
                    "file:{}?cache=shared&mode=memory&cache=private",
                    name("priv")
                )
                .into_boxed_str(),
            ),
            "the last cache= occurrence wins in SQLite's ordered option loop",
        ),
    ];
    for (db, why) in cases {
        let err = open_database(DatabaseOptions {
            db_path: db,
            startup_cwd: dir.path(),
            pool_size: 2,
        })
        .expect_err(&format!("{db} must be refused: {why}"));
        assert_eq!(
            err.stage,
            DbStage::Input,
            "{db}: refused before opening: {err}"
        );
        assert_eq!(
            err.class,
            DbFailureClass::PrivateMemory,
            "{db}: {why}: {err}"
        );
        assert!(
            err.sqlite.is_none(),
            "{db}: a refusal has no SQLite codes: {err:?}"
        );
        assert_no_leak(&err, db, dir.path());
    }
    // resolve_input agrees without opening anything.
    let refused = resolve_input(":memory:?cache=shared", dir.path());
    assert_eq!(
        refused.err().map(|e| (e.stage, e.class)),
        Some((DbStage::Input, DbFailureClass::PrivateMemory)),
        "resolve_input is the same classification open_database uses"
    );
}

#[test]
fn relative_plain_path_is_created_under_the_injected_cwd_not_the_process_cwd() {
    // Arrange: an injected cwd with an existing subdirectory. The process cwd is
    // never consulted.
    let dir = temp_dir("relative");
    let nested = dir.path().join("data");
    std::fs::create_dir_all(&nested).expect("create injected cwd");
    let process_cwd = std::env::current_dir().expect("process cwd");
    let db_name = name("relative");
    let db = default_shape(db_name.as_str());

    // Act.
    let opened = open_store(db.as_str(), nested.as_path(), 2);
    assert!(
        opened.target.anchored_to_startup_cwd,
        "a relative input is anchored to the supplied cwd"
    );
    assert!(
        opened.target.migrated_on_open,
        "fresh relative DB is migrated"
    );

    // Assert: the file exists under the injected cwd with no query in its name,
    // and nothing was created under the process cwd or the temp root.
    assert!(
        nested.join(&db_name).is_file(),
        "database created under the injected cwd: {nested:?}"
    );
    let state = state_of(&opened.store);
    assert_eq!(
        state.effective.db_file.as_deref(),
        canonical(&nested).join(&db_name).to_str(),
        "SQLite canonicalized the anchored path and stripped the query"
    );
    let file = state.effective.db_file.expect("file DB reports a file");
    assert!(
        !file.contains('?'),
        "the URI query is never filename characters: {file}"
    );
    assert!(
        !process_cwd.join(&db_name).exists(),
        "nothing is created under the process cwd"
    );
    assert!(
        !dir.path().join(&db_name).exists(),
        "the anchor is the injected cwd, not its parent"
    );
    // The retained query actually configured the handle.
    assert_eq!(
        state.effective.journal_mode, "wal",
        "retained query applied"
    );
    assert_eq!(
        state.effective.busy_timeout, 15000,
        "busy timeout from the retained query"
    );
    assert!(state.effective.foreign_keys, "FK from the retained query");
}

#[test]
fn forwarded_shared_cache_reaches_sqlite_only_for_the_exact_default_dsn() {
    // `cache=shared` is forwarded for **Navidrome's default DSN only**
    // (`nd-conf/src/consts.rs:16` + `nd-conf/src/config.rs:999-1011` build exactly
    // `<DataFolder>/navidrome.db?<default query>`), and it is a deliberate D-OPEN
    // divergence (schema.rs:269-287). Measured on the anchored path:
    //   * exact default shape  -> SQLite receives `file:<cwd>/navidrome.db?cache=shared`,
    //     and a second handle writing a table a transaction holds fails immediately
    //     with SQLITE_LOCKED_SHAREDCACHE (262);
    //   * the same query on any other filename -> a plain path with no forwarding, so
    //     the conflict is the ordinary SQLITE_BUSY (5) once the (here 1 ms) busy
    //     timeout expires.
    // Pinning both proves anchoring kept the URI form for the default DSN and did not
    // smuggle `cache=shared` into an unrelated input.
    let dir = temp_dir("shared-cache");
    let default_db = default_shape("navidrome.db");
    let opened = open_store(default_db.as_str(), dir.path(), 2);
    assert!(
        opened.target.anchored_to_startup_cwd,
        "default shape anchored"
    );
    opened
        .store
        .with_connection_sync(|conn| {
            conn.execute("CREATE TABLE cache_probe (k TEXT PRIMARY KEY)", [])
        })
        .expect("create table committed");

    let shared_code = conflict_code(&opened.store);
    assert_eq!(
        shared_code,
        rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE,
        "the default DSN's forwarded cache=shared reached SQLite through the anchor"
    );

    // Same parameters, different basename: no forwarding, so ordinary busy.
    let custom =
        default_shape(name("nocache").as_str()).replace("_busy_timeout=15000", "_busy_timeout=1");
    let other = open_store(custom.as_str(), dir.path(), 2);
    other
        .store
        .with_connection_sync(|conn| {
            conn.execute("CREATE TABLE cache_probe (k TEXT PRIMARY KEY)", [])
        })
        .expect("create table committed");
    let busy_code = conflict_code(&other.store);
    assert_eq!(
        busy_code,
        rusqlite::ffi::SQLITE_BUSY,
        "a non-default input keeps the ordinary busy conflict (no cache=shared injected)"
    );
}

/// Extended code from a second handle writing a table a live transaction holds.
fn conflict_code(store: &SQLStore) -> i32 {
    store
        .with_tx_sync(|tx| {
            tx.execute("INSERT INTO cache_probe (k) VALUES('a')", [])?;
            let blocked = store.with_connection_sync(|other| {
                other.execute("INSERT INTO cache_probe (k) VALUES('b')", [])
            });
            let err = blocked
                .expect_err("a second handle is refused while the transaction holds the table");
            match err {
                StoreError::Other(inner) => Ok(extended_code(&inner)),
                other => Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ffi::ErrorCode::Unknown,
                        extended_code: 0,
                    },
                    Some(format!("{other:?}")),
                )),
            }
        })
        .expect("owning transaction completes")
}

#[test]
fn escaped_absolute_file_uri_is_preserved_byte_for_byte() {
    // Arrange: an absolute path whose directory separator is percent-encoded, so
    // SQLite must decode it and open `<parent>/wei/rd/<name>`. Nothing here
    // percent-encodes or rewrites the URI.
    let dir = temp_dir("escaped-uri");
    let inner = dir.path().join("wei/rd");
    std::fs::create_dir_all(&inner).expect("create directory containing a separator");
    let db_name = name("escaped");
    let parent = dir.path().to_str().expect("UTF-8 parent").to_string();
    let uri = format!("file:{parent}/wei%2Frd/{db_name}?_journal_mode=WAL");

    // Act.
    let opened = open_store(uri.as_str(), dir.path(), 1);
    assert!(
        !opened.target.anchored_to_startup_cwd,
        "an absolute file: URI passes through unchanged"
    );
    assert!(
        inner.join(&db_name).is_file(),
        "SQLite decoded %2F and created the file: {inner:?}"
    );
    let state = state_of(&opened.store);
    assert_eq!(
        state.effective.db_file.as_deref(),
        canonical(&inner).join(&db_name).to_str(),
        "effective file is the decoded absolute path"
    );
    assert_eq!(
        state.effective.journal_mode, "wal",
        "URI query preserved verbatim"
    );
}

#[test]
fn relative_file_uri_and_bad_authority_are_refused_without_rewriting() {
    let dir = temp_dir("relative-uri");
    let cases = [
        (
            "file:data/missing.db",
            DbFailureClass::RelativeUri,
            "a relative file: URI naming a file cannot be anchored without rewriting it",
        ),
        (
            "file://tmp/navidrome.db",
            DbFailureClass::InvalidAuthority,
            "SQLite rejects an authority-bearing URI",
        ),
    ];
    for (db, class, why) in cases {
        let err = open_database(DatabaseOptions {
            db_path: db,
            startup_cwd: dir.path(),
            pool_size: 1,
        })
        .expect_err(why);
        assert_eq!(err.stage, DbStage::Input, "{db}: {why}: {err}");
        assert_eq!(err.class, class, "{db}: class: {err}");
        assert_no_leak(&err, db, dir.path());
    }
    assert!(
        !dir.path().join("data").exists(),
        "a refused URI creates no directory"
    );
}

#[test]
fn malformed_dsn_is_a_safe_error_and_no_store_or_file_is_created() {
    let dir = temp_dir("malformed");
    let db = format!("{}?unknown_param=1", in_dir(dir.path(), &name("bad")));
    let err = open_database(DatabaseOptions {
        db_path: db.as_str(),
        startup_cwd: dir.path(),
        pool_size: 2,
    })
    .expect_err("an unknown parameter is refused, not dropped (D-OPEN option a)");

    assert_eq!(err.stage, DbStage::Input, "refused before opening: {err}");
    assert_eq!(err.class, DbFailureClass::UnsupportedInput, "{err}");
    assert!(
        err.sqlite.is_none(),
        "no handle existed, so no SQLite codes"
    );
    assert_no_leak(&err, db.as_str(), dir.path());
    assert!(
        !dir.path().join(basename(db.as_str())).exists(),
        "a refused input creates no file"
    );
    assert_eq!(
        resolve_input(db.as_str(), dir.path())
            .err()
            .map(|e| (e.stage, e.class)),
        Some((DbStage::Input, DbFailureClass::UnsupportedInput)),
        "resolve_input agrees with open_database"
    );
}

#[test]
fn migration_failure_reports_numeric_codes_and_returns_no_store() {
    // Arrange: the database file itself opens fine, but a regular file sits exactly
    // where its rollback journal must be created. SQLite fails inside the fresh
    // DDL, so this exercises the migration stage rather than the open (measured:
    // SQLITE_CANTOPEN 14 from the DDL pass).
    let dir = temp_dir("migration-failure");
    let db_name = name("broken");
    let db = in_dir(dir.path(), &db_name);
    std::fs::create_dir(dir.path().join(format!("{db_name}-journal")))
        .expect("occupy the journal name with a directory");
    let db = format!("{db}?_journal_mode=delete&_foreign_keys=on");

    let err = open_database(DatabaseOptions {
        db_path: db.as_str(),
        startup_cwd: dir.path(),
        pool_size: 2,
    })
    .expect_err("the fresh DDL must fail when its journal cannot be created");

    assert_eq!(
        err.stage,
        DbStage::Migration,
        "failure at the migration stage: {err}"
    );
    assert_eq!(err.class, DbFailureClass::MigrationFailed, "{err}");
    let codes = err.sqlite.expect("a SQLite failure reports numeric codes");
    assert_eq!(
        codes.primary,
        rusqlite::ffi::SQLITE_CANTOPEN,
        "SQLITE_CANTOPEN (14)"
    );
    assert!(err.source().is_none(), "the error carries no source chain");
    assert_no_leak(&err, db.as_str(), dir.path());

    // No store exists on this path, and the failed migration left no marker.
    let raw = open(in_dir(dir.path(), &db_name).as_str()).expect("open the file directly");
    let marker: i64 = raw
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'goose_db_version'",
            [],
            |row| row.get(0),
        )
        .expect("probe marker");
    assert_eq!(marker, 0, "a failed migration leaves no marker row");
    drop(raw);
}

#[test]
fn unopenable_paths_report_the_open_stage_and_create_nothing() {
    // Case 1: an existing parent that cannot be written to.
    let dir = temp_dir("read-only-parent");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("create parent");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))
        .expect("make parent read-only");
    let db = locked.join(name("ro")).to_str().expect("UTF-8").to_string();

    let err = open_database(DatabaseOptions {
        db_path: db.as_str(),
        startup_cwd: dir.path(),
        pool_size: 2,
    })
    .expect_err("cannot create a database in a read-only parent");
    assert_eq!(err.stage, DbStage::MigrationOpen, "open stage: {err}");
    assert_eq!(err.class, DbFailureClass::OpenFailed, "{err}");
    assert_eq!(
        err.sqlite.map(|c| c.primary),
        Some(rusqlite::ffi::SQLITE_CANTOPEN),
        "SQLITE_CANTOPEN (14): {err}"
    );
    assert_no_leak(&err, db.as_str(), dir.path());

    // Case 2: a relative input whose subdirectory does not exist. SQLite refuses;
    // this stage does not create the directory.
    let dir2 = temp_dir("missing-parent");
    let err2 = open_database(DatabaseOptions {
        db_path: "sub/missing.db",
        startup_cwd: dir2.path(),
        pool_size: 1,
    })
    .expect_err("a missing parent is reported, never created");
    assert_eq!(err2.stage, DbStage::MigrationOpen, "open stage: {err2}");
    assert_eq!(
        err2.sqlite.map(|c| c.primary),
        Some(rusqlite::ffi::SQLITE_CANTOPEN),
        "SQLITE_CANTOPEN (14): {err2}"
    );
    assert_no_leak(&err2, "sub/missing.db", dir2.path());
    assert!(
        !dir2.path().join("sub").exists(),
        "the missing directory stays absent"
    );
    assert!(
        !dir2.path().join("missing.db").exists(),
        "no file created at the cwd root either"
    );

    // Permission-test limitation: mode bits only deny access for an unprivileged
    // user, so this assertion is meaningful only when the suite is not running as
    // root. It holds either way, so the case stays valid.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
        .expect("restore permissions so TempDir can clean up");
    let entries: Vec<String> = std::fs::read_dir(&locked)
        .expect("list parent")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.is_empty(),
        "no database created in the parent: {entries:?}"
    );
}

#[test]
fn startup_cwd_must_be_absolute_and_existing_for_a_relative_input() {
    let dir = temp_dir("cwd-guard");
    let missing = dir.path().join("absent");

    let not_absolute = open_database(DatabaseOptions {
        db_path: "navidrome.db",
        startup_cwd: Path::new("relative/cwd"),
        pool_size: 1,
    })
    .expect_err("a relative cwd cannot anchor a relative path");
    assert_eq!(not_absolute.stage, DbStage::MigrationOpen, "{not_absolute}");
    assert_eq!(
        not_absolute.class,
        DbFailureClass::StartupCwdNotAbsolute,
        "{not_absolute}"
    );
    assert!(not_absolute.sqlite.is_none(), "refused before any handle");
    assert!(!Path::new("relative/cwd").exists(), "no cwd was created");

    let absent = open_database(DatabaseOptions {
        db_path: "navidrome.db",
        startup_cwd: missing.as_path(),
        pool_size: 1,
    })
    .expect_err("a missing cwd is not created");
    assert_eq!(absent.class, DbFailureClass::StartupCwdMissing, "{absent}");
    assert!(!missing.exists(), "the missing cwd stays absent");
    assert_no_leak(&absent, "navidrome.db", dir.path());

    // An absolute DB path never consults the cwd at all.
    let opened = open_store(
        in_dir(dir.path(), &name("no-cwd")).as_str(),
        Path::new("/"),
        1,
    );
    assert!(
        !opened.target.anchored_to_startup_cwd,
        "an absolute path is passed through regardless of cwd"
    );
}

#[test]
fn unicode_symlink_and_trailing_slash_paths_are_anchored_and_canonicalized() {
    let dir = temp_dir("unicode-symlink");

    // Unicode directory component.
    let unicode = dir.path().join("音乐库");
    std::fs::create_dir_all(&unicode).expect("create unicode dir");
    let db_name = name("音乐.db");
    let opened = open_store(db_name.as_str(), unicode.as_path(), 1);
    assert!(
        opened.target.anchored_to_startup_cwd,
        "Unicode relative path anchored"
    );
    assert_eq!(
        state_of(&opened.store).effective.db_file.as_deref(),
        canonical(&unicode).join(&db_name).to_str(),
        "Unicode path canonicalized by SQLite and read back identically"
    );

    // Symlinked cwd: the anchor is used verbatim and SQLite canonicalizes through
    // the link, so the database lands at the link's target.
    let target = dir.path().join("target");
    std::fs::create_dir_all(&target).expect("create symlink target");
    let link = dir.path().join("link");
    symlink(&target, &link).expect("symlink the cwd");
    let via_link = name("via-link.db");
    let opened = open_store(via_link.as_str(), link.as_path(), 1);
    assert!(
        opened.target.migrated_on_open,
        "migration ran through the symlinked cwd"
    );
    assert_eq!(
        state_of(&opened.store).effective.db_file.as_deref(),
        canonical(&target).join(&via_link).to_str(),
        "SQLite canonicalized the symlink to its target"
    );
    assert!(
        target.join(&via_link).is_file(),
        "file created at the symlink target"
    );

    // Trailing slash on the cwd: joining still lands inside it.
    let with_slash = canonical(&target)
        .join("")
        .to_str()
        .expect("UTF-8")
        .to_string();
    let slashed = name("trailing.db");
    let opened = open_store(slashed.as_str(), Path::new(with_slash.as_str()), 1);
    assert!(
        opened.target.anchored_to_startup_cwd,
        "a trailing-slash cwd still anchors"
    );
    assert_eq!(
        state_of(&opened.store).effective.db_file.as_deref(),
        canonical(&target).join(&slashed).to_str(),
        "trailing slash does not change the resolved file"
    );
}

#[test]
fn error_and_target_never_print_the_input() {
    let dir = temp_dir("no-print");
    // `_auth` is refused by D-OPEN, so the input reaches classification only.
    let db = format!("{}?_auth=x", in_dir(dir.path(), &name("secret")));
    let err = open_database(DatabaseOptions {
        db_path: db.as_str(),
        startup_cwd: dir.path(),
        pool_size: 1,
    })
    .expect_err("_auth is refused, not silently dropped");
    let text = full_text(&err);
    assert!(!text.contains("_auth"), "no parameter name echoed: {text}");
    assert!(!text.contains("secret"), "no filename echoed: {text}");
    assert_no_leak(&err, db.as_str(), dir.path());

    let opened = open_store(in_dir(dir.path(), &name("print")).as_str(), dir.path(), 1);
    let shown = format!("{opened:?}");
    assert!(
        !shown.contains(".db"),
        "OpenedDatabase Debug prints no filename: {shown}"
    );
    assert!(
        !shown.contains(dir.path().to_str().unwrap()),
        "OpenedDatabase Debug prints no path: {shown}"
    );
    let displayed = opened.target.to_string();
    assert!(
        !displayed.contains(".db"),
        "target Display names no file: {displayed}"
    );
    assert!(
        !displayed.contains('?'),
        "target Display carries no query: {displayed}"
    );
    // A malformed-DSN error's Debug carries only stage/class/codes.
    let debug = format!("{err:?}");
    assert!(
        debug.contains("Input") && debug.contains("UnsupportedInput"),
        "{debug}"
    );
}
