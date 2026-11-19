//! DB-01 (docs/rust-port/subplans/db-runtime.md): `mediafile_repo::mark_missing_sql`
//! EXECUTED against the migrated schema, pinning the ARGUMENT ORDER the statement
//! needs — the defect this slice fixes.
//!
//! Go truth under test: `persistence/mediafile_repository.go` `MarkMissing`
//! (`:455-469`: `slice.CollectChunks(ids, 200)` `:457`, `Set("missing", …)` `:459`,
//! `Set("updated_at", time.Now())` `:460`, `Where(Eq{"id": chunk})` `:461`) and
//! `MarkMissingByFolder` (`:472-487`: `slices.Chunk(folderIDs, 200)` `:473`,
//! `Eq{"folder_id": chunk}` `:478`, `Eq{"missing": !missing}` `:479`). squirrel
//! appends args in build order — SET values, then WHERE values — so the bind order
//! is `[missing, updated_at, ids…]`.
//!
//! The bug this file exists for (parent-reproduced against the migrated schema):
//! the old Rust `mark_missing_sql` emitted ids FIRST —
//! `[Text(ids…), Bool(missing), Integer(updated_at)]` — against
//! `UPDATE media_file SET missing = ?, updated_at = ? WHERE id IN (?)`. SQLite then
//! bound a media-file id into `missing`, the flip into `updated_at` and the stamp
//! into the id list. Measured against the migrated schema: with one id the UPDATE
//! matched nothing (**rows affected 0**, every row keeping `missing = 0`); with the
//! mixed-shape set `["mf-target", "5"]` and `updated_at = 5` the stamp lands in
//! `IN (…)` and matches the numeric-looking id `5` (`media_file.id` is `varchar`, and
//! SQLite compares the bound INTEGER 5 against the stored text '5'), so the OLD order
//! touches the WRONG row — one row affected, its `missing` set to the TEXT
//! `'mf-target'` and its `updated_at` to 1 — while the intended target stays
//! untouched. Both failure modes are asserted as an executed control
//! (`pre_fix_ids_first_bind_is_a_no_op_and_hits_the_wrong_row`), and each positive
//! test asserts the row-level state the old order could not produce.
//!
//! The claims this file pins, each executed by calling the Rust renderer and
//! running what it returns (never a hand-written UPDATE under test):
//! (1) one id marks exactly that row and leaves every sibling — including the
//!     numeric decoy `5` and a same-folder sibling — untouched;
//! (2) several ids in one statement mark all of them and nothing else;
//! (3) 201 ids render 2 statements of 200 and 1 placeholders and EXECUTE to 200
//!     and 1 rows affected, so both chunks land;
//! (4) `missing` flips in both directions, and `updated_at` is stamped on the rows
//!     it touches;
//! (5) `mark_missing_by_folder_sql` gates on `missing = !missing`, so already-flagged
//!     rows are not re-marked and its `updated_at` does not move;
//! (6) an empty id list renders no statements and touches nothing;
//! (7) the pre-fix ids-first bind against the SAME rendered SQL is a no-op (0 rows
//!     affected) in the single-id case and touches the wrong row (`5`) in the
//!     mixed-shape case, with corrupted SET values in both.
//!
//! Harness shape copied from `tests/runtime_timestamp_convention.rs:48-117`
//! (`Value` → `rusqlite::types::Value` mapping, temp file, pool, `apply_fresh_schema`).

use nd_db::mediafile_repo::mark_missing_by_folder_sql;
use nd_db::mediafile_repo::mark_missing_sql;
use nd_db::pool::{Pool, PoolConfig};
use nd_db::sqlrepo::Value;
use nd_db::store::SQLStore;
use nd_dbschema::schema::apply_fresh_schema;

/// Deterministic `updated_at` stand-in for Go's `time.Now()` (`:460`, `:476`).
/// Deliberately not 5, except where a test wants the mis-bound stamp to collide with
/// the numeric decoy id (see `pre_fix_ids_first_bind_is_a_no_op_and_hits_the_wrong_row`).
const STAMP: i64 = 1234;

/// The numeric-looking decoy id. `media_file.id` is `varchar` (TEXT affinity), so
/// SQLite compares a bound INTEGER `5` against the text `'5'` and matches it —
/// which is exactly how an ids-first bind lets a timestamp reach `WHERE id IN (…)`
/// and mark a row nobody asked for.
const DECOY: &str = "5";

/// One isolated on-disk DB per invocation: unique temp path, freshly migrated,
/// pooled, `_foreign_keys=on` (Go's `db/db.go:55` DSN parameter) so the
/// `folder`/`library` FKs really are enforced.
fn fresh_store() -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        "nd-db-missing-state-",
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path)
            .unwrap_or_else(|e| panic!("cannot clear stale {:?}: {e}", path));
    }
    let dsn = format!("file:{}?mode=rwc&_foreign_keys=on", path.display());
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn, 2)).unwrap();
    apply_fresh_schema(&first).unwrap();
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Run a raw block on one pooled lease, panicking on lease or statement failure.
fn run<T>(
    store: &SQLStore,
    call: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
) -> T {
    store
        .with_connection_sync(call)
        .unwrap_or_else(|e| panic!("store lease or statement failed: {e}"))
}

/// `rusqlite::types::Value` for a [`Value`] — the same mapping the repositories use
/// internally (`share_repo::bind_values`), so the tests drive the statements the
/// public renderers produce without reaching into private helpers.
fn value_ref(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Text(s) => rusqlite::types::Value::Text(s.clone()),
        Value::Integer(i) => rusqlite::types::Value::Integer(*i),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        Value::Null => rusqlite::types::Value::Null,
    }
}

/// Execute every statement a renderer returned and hand back the per-statement
/// rows-affected counts — the numbers the non-vacuity argument rests on.
fn execute_all(store: &SQLStore, statements: &[(String, Vec<Value>)]) -> Vec<usize> {
    run(store, |conn| {
        let counts = statements
            .iter()
            .map(|(sql, args)| {
                conn.execute(
                    sql.as_str(),
                    rusqlite::params_from_iter(args.iter().map(value_ref)),
                )
            })
            .collect::<Result<Vec<usize>, rusqlite::Error>>()?;
        Ok(counts)
    })
}

/// Seed `library` (`id` INTEGER PK, `name` NOT NULL) → `folder` (varchar id, FK to
/// library) → the named `media_file` rows (varchar id, FK to folder), all with
/// `missing = 0` and `updated_at` NULL.
fn seed(store: &SQLStore, folder_id: &str, media_file_ids: &[&str]) {
    run(store, |conn| {
        conn.execute(
            "INSERT INTO library (id, name, path) VALUES(1, 'Music', '/music')",
            rusqlite::params![],
        )?;
        conn.execute(
            "INSERT INTO folder (id, library_id, path, name) VALUES(?, 1, '/music', 'Music')",
            rusqlite::params![folder_id],
        )?;
        for id in media_file_ids {
            conn.execute(
                "INSERT INTO media_file (id, folder_id, library_id, title, path) \
                 VALUES(?, ?, 1, ?, ?)",
                rusqlite::params![id, folder_id, id, format!("/music/{id}.mp3")],
            )?;
        }
        Ok(())
    })
}

/// `(missing, updated_at)` of one row, as RAW values, because the storage class is
/// itself evidence. `media_file.missing` (`boolean`) and `updated_at` (`datetime`)
/// both resolve to SQLite NUMERIC affinity (neither type name contains INT/REAL/
/// TEXT/BLOB), so a correctly bound flip lands as INTEGER 0/1 and a numeric-looking
/// mis-bound TEXT (`'5'`) is converted back to INTEGER — while a non-numeric mis-bound
/// TEXT (`'mf-target'`) survives as TEXT, and `CAST(missing AS INTEGER)` of it is 0,
/// i.e. the corrupted row is not even flagged.
fn state(store: &SQLStore, id: &str) -> (rusqlite::types::Value, rusqlite::types::Value) {
    run(store, |conn| {
        let mut stmt = conn.prepare("SELECT missing, updated_at FROM media_file WHERE id = ?")?;
        let row = stmt
            .query_map(rusqlite::params![id], |r| {
                Ok((
                    r.get::<_, rusqlite::types::Value>(0)?,
                    r.get::<_, rusqlite::types::Value>(1)?,
                ))
            })?
            .next()
            .unwrap_or_else(|| panic!("no media_file row {id}"))?;
        Ok(row)
    })
}

/// Rows whose `missing` is truthy (1), ordered by id — the aggregate view used to
/// prove no unintended row was touched.
fn missing_ids(store: &SQLStore) -> Vec<String> {
    run(store, |conn| {
        let mut stmt = conn
            .prepare("SELECT id FROM media_file WHERE CAST(missing AS INTEGER) = 1 ORDER BY id")?;
        let ids = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<String>, rusqlite::Error>>()?;
        Ok(ids)
    })
}

/// Rows whose `updated_at` still holds the seed value (NULL) — used to prove the
/// gate left a row completely alone, stamp included.
fn unstamped_ids(store: &SQLStore) -> Vec<String> {
    run(store, |conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM media_file WHERE updated_at IS NULL ORDER BY id")?;
        let ids = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<String>, rusqlite::Error>>()?;
        Ok(ids)
    })
}

fn cleanup(store: SQLStore, path: std::path::PathBuf) {
    drop(store);
    std::fs::remove_file(&path).unwrap();
}

/// Copy of a rendered arg vector with the OLD (defective) ids-first order, so the
/// control executes the same SQL the renderer produced and only the bind order
/// differs. `Vec<Value>` is `Clone` (`sqlrepo.rs:41`).
fn ids_first(args: &[Value]) -> Vec<Value> {
    let (ids, tail) = args.split_at(args.len() - 2);
    tail.iter().chain(ids.iter()).cloned().collect()
}

#[test]
fn single_id_marks_the_target_and_leaves_siblings_untouched() {
    // Arrange: target + numeric decoy + two plain siblings, all missing = 0.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-target", DECOY, "mf-a", "mf-b"]);
    // Act: the real renderer, executed.
    let counts = execute_all(
        &store,
        &mark_missing_sql(true, &["mf-target".to_string()], STAMP),
    );
    // Assert: one statement, one row affected. Pre-fix this was 0 rows affected
    // and nothing below would hold.
    assert_eq!(counts, vec![1], "one statement marks exactly one row");
    // Assert: the target carries the flip and the stamp.
    assert_eq!(
        state(&store, "mf-target"),
        (
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Integer(STAMP),
        )
    );
    // Assert: no sibling moved, and the aggregate missing set is the target alone.
    assert_eq!(missing_ids(&store), vec!["mf-target".to_string()]);
    assert_eq!(unstamped_ids(&store), vec![DECOY, "mf-a", "mf-b"]);
    cleanup(store, path);
}

#[test]
fn multiple_ids_in_one_statement_mark_every_named_row() {
    // Arrange: five rows, three of them named in one chunk.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-1", "mf-2", "mf-3", DECOY, "mf-9"]);
    // Act:
    let statements = mark_missing_sql(
        true,
        &["mf-1".to_string(), "mf-2".to_string(), "mf-3".to_string()],
        STAMP,
    );
    assert_eq!(statements.len(), 1, "three ids stay in one chunk");
    let counts = execute_all(&store, &statements);
    // Assert: 3 rows affected, exactly the named ids missing, the decoy and mf-9
    // untouched (pre-fix: 0 rows affected, no row flipped).
    assert_eq!(counts, vec![3]);
    assert_eq!(
        missing_ids(&store),
        vec!["mf-1".to_string(), "mf-2".to_string(), "mf-3".to_string()]
    );
    assert_eq!(state(&store, DECOY).0, rusqlite::types::Value::Integer(0));
    assert_eq!(state(&store, "mf-9").0, rusqlite::types::Value::Integer(0));
    assert_eq!(unstamped_ids(&store), vec![DECOY, "mf-9"]);
    cleanup(store, path);
}

#[test]
fn chunk_boundary_at_200_executes_two_statements_with_exact_counts() {
    // Arrange: 201 rows (`mf-0` … `mf-200`) plus a decoy that is NOT in the id set.
    let (store, path) = fresh_store();
    let ids: Vec<String> = (0..201).map(|i| format!("mf-{i}")).collect();
    let mut seed_ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    seed_ids.push(DECOY);
    seed(&store, "f-1", &seed_ids);
    // Act:
    let statements = mark_missing_sql(true, &ids, STAMP);
    // Assert: 2 statements, 200 and 1 placeholders (`slice.CollectChunks(ids, 200)`,
    // `:457`), each with the flip + stamp ahead of its ids.
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].0.matches('?').count(), 202);
    assert_eq!(statements[1].0.matches('?').count(), 3);
    assert_eq!(statements[0].1.len(), 202);
    assert_eq!(statements[1].1.len(), 3);
    assert_eq!(statements[0].1[0], Value::Bool(true));
    assert_eq!(statements[0].1[1], Value::Integer(STAMP));
    assert_eq!(statements[0].1[2], Value::Text("mf-0".to_string()));
    assert_eq!(statements[1].1[2], Value::Text("mf-200".to_string()));
    let counts = execute_all(&store, &statements);
    // Assert: 200 + 1 rows affected and all 201 rows flagged — the old order
    // executed both chunks to 0 rows affected, so `missing_ids` stayed empty.
    assert_eq!(counts, vec![200, 1]);
    assert_eq!(missing_ids(&store).len(), 201);
    assert_eq!(
        missing_ids(&store).last().map(String::as_str),
        Some("mf-99")
    );
    assert_eq!(state(&store, DECOY).0, rusqlite::types::Value::Integer(0));
    assert_eq!(unstamped_ids(&store), vec![DECOY]);
    cleanup(store, path);
}

#[test]
fn missing_flips_both_directions_and_stamps_the_rows_it_touches() {
    // Arrange: start the target flagged, with a distinguishable old stamp.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-target", DECOY, "mf-a"]);
    run(&store, |conn| {
        conn.execute(
            "UPDATE media_file SET missing = 1, updated_at = 111 WHERE id = 'mf-target'",
            rusqlite::params![],
        )?;
        Ok(())
    });
    // Act 1: flip 1 → 0.
    let counts = execute_all(
        &store,
        &mark_missing_sql(false, &["mf-target".to_string()], STAMP),
    );
    // Assert: the row is un-flagged and re-stamped (pre-fix: 0 rows affected, so
    // `missing` would have stayed 1 forever and the scanner could never restore a
    // file that reappears).
    assert_eq!(counts, vec![1]);
    assert_eq!(
        state(&store, "mf-target").0,
        rusqlite::types::Value::Integer(0)
    );
    assert_eq!(missing_ids(&store), Vec::<String>::new());
    // Act 2: flip 0 → 1 with a new stamp.
    let counts = execute_all(
        &store,
        &mark_missing_sql(true, &["mf-target".to_string()], 2222),
    );
    // Assert:
    assert_eq!(counts, vec![1]);
    assert_eq!(
        state(&store, "mf-target").0,
        rusqlite::types::Value::Integer(1)
    );
    assert_eq!(missing_ids(&store), vec!["mf-target".to_string()]);
    assert_eq!(
        state(&store, "mf-target").1,
        rusqlite::types::Value::Integer(2222)
    );
    // Assert: the siblings never moved in either direction.
    assert_eq!(unstamped_ids(&store), vec![DECOY, "mf-a"]);
    cleanup(store, path);
}

#[test]
fn mark_by_folder_gates_on_the_current_flag() {
    // Arrange: two folders; f-1's rows are already flagged, f-2's are not, and one
    // row in f-2 is flagged (it must NOT be re-marked by the `missing = !missing`
    // gate, `:479`). f-3 exists but is not named.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-f1-a", "mf-f1-b"]);
    run(&store, |conn| {
        conn.execute(
            "INSERT INTO folder (id, library_id, path, name) VALUES('f-2', 1, '/music/2', 'Two')",
            rusqlite::params![],
        )?;
        conn.execute(
            "INSERT INTO folder (id, library_id, path, name) VALUES('f-3', 1, '/music/3', 'Three')",
            rusqlite::params![],
        )?;
        for id in ["mf-f2-a", "mf-f2-b", "mf-f2-flagged"] {
            conn.execute(
                "INSERT INTO media_file (id, folder_id, library_id, title, path) \
                 VALUES(?, 'f-2', 1, ?, ?)",
                rusqlite::params![id, id, format!("/music/2/{id}.mp3")],
            )?;
        }
        conn.execute(
            "UPDATE media_file SET missing = 1, updated_at = 111 WHERE id = 'mf-f2-flagged'",
            rusqlite::params![],
        )?;
        Ok(())
    });
    // Act: mark-by-folder for f-1 and f-2 with missing = true; the gate binds
    // `missing = !missing` = false, so only un-flagged rows flip.
    let statements =
        mark_missing_by_folder_sql(true, &["f-1".to_string(), "f-2".to_string()], STAMP);
    assert_eq!(statements.len(), 1, "two folder ids stay in one chunk");
    assert_eq!(statements[0].1[0], Value::Bool(true));
    assert_eq!(statements[0].1[1], Value::Integer(STAMP));
    assert_eq!(statements[0].1[2], Value::Text("f-1".to_string()));
    assert_eq!(statements[0].1[3], Value::Text("f-2".to_string()));
    assert_eq!(
        statements[0].1[4],
        Value::Bool(false),
        "the gate is !missing"
    );
    let counts = execute_all(&store, &statements);
    // Assert: 4 rows affected — f-1's 2 plus f-2's 2 un-flagged; the already-flagged
    // row is excluded by the gate, f-3 is excluded by folder_id.
    assert_eq!(counts, vec![4]);
    assert_eq!(
        missing_ids(&store),
        vec![
            "mf-f1-a".to_string(),
            "mf-f1-b".to_string(),
            "mf-f2-a".to_string(),
            "mf-f2-b".to_string(),
            "mf-f2-flagged".to_string(),
        ]
    );
    // Assert: the gated row kept its OLD stamp (111), proving it was not re-marked.
    assert_eq!(
        state(&store, "mf-f2-flagged").1,
        rusqlite::types::Value::Integer(111)
    );
    // Act 2: the reverse flip (missing = false, gate `missing = true`) un-marks only
    // the flagged rows of f-2 and leaves f-1's alone.
    let statements = mark_missing_by_folder_sql(false, &["f-2".to_string()], 2222);
    assert_eq!(statements[0].1[0], Value::Bool(false));
    assert_eq!(statements[0].1[1], Value::Integer(2222));
    assert_eq!(
        statements[0].1[3],
        Value::Bool(true),
        "the gate is !missing"
    );
    let counts = execute_all(&store, &statements);
    // Assert: the 3 currently-flagged f-2 rows un-mark (2 newly + 1 already flagged).
    assert_eq!(counts, vec![3]);
    assert_eq!(
        missing_ids(&store),
        vec!["mf-f1-a".to_string(), "mf-f1-b".to_string()]
    );
    cleanup(store, path);
}

#[test]
fn empty_id_list_renders_no_statements_and_touches_nothing() {
    // Arrange: rows present, `chunks()` of an empty slice yields no chunk.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-a", DECOY]);
    // Act:
    let statements = mark_missing_sql(true, &[], STAMP);
    // Assert: no statement at all (Go's chunker iterates zero times, `:457`), so
    // nothing executes and no row changes. A renderer that emitted `IN ()` would
    // be a SQLite syntax error here, not a silent no-op.
    assert_eq!(statements.len(), 0);
    let counts = execute_all(&store, &statements);
    assert_eq!(counts, Vec::<usize>::new());
    assert_eq!(missing_ids(&store), Vec::<String>::new());
    assert_eq!(unstamped_ids(&store), vec![DECOY, "mf-a"]);
    // Assert: same for the folder variant.
    assert_eq!(mark_missing_by_folder_sql(true, &[], STAMP).len(), 0);
    cleanup(store, path);
}

/// Executed control for the defect: the SAME SQL text the renderer emits, bound in
/// the pre-fix ids-first order. This documents why every positive test above is
/// non-vacuous — under the old order the statements parse and run without error
/// while changing nothing (or the wrong thing).
#[test]
fn pre_fix_ids_first_bind_is_a_no_op_and_hits_the_wrong_row() {
    // Arrange: a mixed-shape set where the stamp (5) collides with the numeric
    // decoy id, plus a plain single-id case.
    let (store, path) = fresh_store();
    seed(&store, "f-1", &["mf-target", DECOY, "mf-a"]);
    // Act 1: ids-first on a single id whose text cannot equal the stamp.
    let statements = mark_missing_sql(true, &["mf-target".to_string()], STAMP);
    let mis_bound = ids_first(&statements[0].1);
    let counts = execute_all(&store, &[(statements[0].0.clone(), mis_bound)]);
    // Assert: it EXECUTES without an error but changes nothing — 0 rows affected,
    // `missing` still 0, `updated_at` still NULL. This is the silent no-op.
    assert_eq!(counts, vec![0]);
    assert_eq!(
        state(&store, "mf-target").0,
        rusqlite::types::Value::Integer(0)
    );
    assert_eq!(state(&store, "mf-target").1, rusqlite::types::Value::Null);
    assert_eq!(missing_ids(&store), Vec::<String>::new());
    // Act 2: ids-first with `updated_at = 5` and the id set `["mf-target", "5"]`,
    // so the stamp lands in `IN (…)` and matches the decoy.
    let statements = mark_missing_sql(true, &["mf-target".to_string(), DECOY.to_string()], 5);
    let mis_bound = ids_first(&statements[0].1);
    let counts = execute_all(&store, &[(statements[0].0.clone(), mis_bound)]);
    // Assert: exactly one row affected, and it is the WRONG one (`5`).
    assert_eq!(
        counts,
        vec![1],
        "the mis-bound stamp matches the numeric id 5"
    );
    // Assert: the flag set is still EMPTY — the mis-bound flip went into
    // `updated_at`, so the wrong row is touched but not even marked missing.
    // `missing_ids` casts, so a corrupted TEXT `missing` reads as 0.
    assert_eq!(missing_ids(&store), Vec::<String>::new());
    assert_eq!(
        state(&store, "mf-target").0,
        rusqlite::types::Value::Integer(0),
        "the intended target was never touched"
    );
    // Assert: the wrong row's SET values are the mis-bound ones — a media-file id
    // in `missing` and the flip in `updated_at` — i.e. corrupted, not merely stale.
    assert_eq!(
        state(&store, DECOY).0,
        rusqlite::types::Value::Text("mf-target".to_string())
    );
    assert_eq!(state(&store, DECOY).1, rusqlite::types::Value::Integer(5));
    // Assert: with two ids the mis-bind shifts everything — `missing` gets the first
    // id as TEXT, `updated_at` gets the SECOND id ('5', stored as INTEGER by the
    // column's NUMERIC affinity), and the flip + stamp land in `IN (1, 5)`. So the
    // wrong row is touched and neither SET value is meaningful.
    // Act 3: the FIXED order on the same mixed set marks both rows correctly.
    let counts = execute_all(
        &store,
        &mark_missing_sql(true, &["mf-target".to_string(), DECOY.to_string()], STAMP),
    );
    // Assert: 2 rows affected, both really flagged with the stamp, and the decoy's
    // corrupted `missing` TEXT replaced by the real flip (so `missing_ids` now
    // reports both).
    assert_eq!(counts, vec![2]);
    assert_eq!(state(&store, DECOY).0, rusqlite::types::Value::Integer(1));
    assert_eq!(
        state(&store, DECOY).1,
        rusqlite::types::Value::Integer(STAMP)
    );
    assert_eq!(
        state(&store, "mf-target").1,
        rusqlite::types::Value::Integer(STAMP)
    );
    assert_eq!(
        missing_ids(&store),
        vec![DECOY.to_string(), "mf-target".to_string()]
    );
    cleanup(store, path);
}
