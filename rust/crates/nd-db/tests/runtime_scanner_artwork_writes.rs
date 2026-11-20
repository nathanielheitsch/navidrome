//! DB-12 — the EXECUTED scanner artwork invalidation/enqueue subset
//! (`persistence/artwork_repository.go:150-157`,
//! `persistence/artwork_queue_repository.go:47-68`) through the DB-00 typed
//! transaction seam.
//!
//! Go truth under test, SOURCE-VERIFIED at those lines plus the helpers they call:
//! `DeleteForItems` chunks at `artworkBatchSize` 200 (`:151`) and goes through
//! `sqlRepository.delete` (`sql_base_repository.go:644-647`), which discards the
//! affected-row count; `Enqueue`/`EnqueuePreservingBackoff` share the private `enqueue`
//! (`:150-163`: chunk 100, ONE `time.Now()` for the whole call, `(kind, id, image_type,
//! priority, 0, now, now)` per row) and differ ONLY in the conflict suffix;
//! `EnqueueAllMissing` (`:58-68`) resolves the entity table through `artworkOwnerTables`
//! (`artwork_repository.go:83-89`), errors and runs no SQL for an unmapped kind, and
//! returns `executeSQL`'s count under `skipIfQueued` = `DO NOTHING` (`:96`). Kind tokens
//! are `Kind.Prefix()` (`model/artwork_id.go:22-25`), never the kind name.
//!
//! Every behavioral claim below is EXECUTED against a real SQLite DB built from the fresh
//! DDL (`nd_dbschema::schema::apply_fresh_schema`, one isolated temp file per invocation,
//! `_foreign_keys=on` — Go's own default DSN parameter, `consts/consts.go:14`), written
//! through the production write API on the caller's transaction and read back through the
//! production executed [`ArtworkQueueRepo`] reads (`get`, `count`, `count_queued`). No
//! durable Go oracle exists for these methods in this worktree
//! (`persistence/artwork_queue_repository_test.go` is a Ginkgo suite against a Go DB), so
//! expectations are pinned to Go source lines and labeled EXECUTED-RUST. The SQLite
//! semantics the assertions lean on — DO UPDATE vs DO NOTHING, `changes()` for the
//! INSERT…SELECT arm, duplicate PK keys inside one multi-row VALUES — were checked with
//! `sqlite3` over the same DDL before being written here.
//!
//! Row counts are never used as proof of STATEMENT issuance: the chunk split is pinned by
//! the rendered-leg counts asserted beside the rows, and the "empty input issues no SQL"
//! cases run on an UNMIGRATED DB, where any statement at all fails with `no such table`.
//!
//! Both renderer legs this subset executes were REPAIRED here (parent-approved, 2026-11-19):
//! `delete_for_items_sql` rendered `SELECT * FROM item_artwork …` instead of the DELETE
//! `sqlRepository.delete` builds (`sql_base_repository.go:644-647`), and
//! `enqueue_if_missing_sql` omitted the `INSERT INTO artwork_queue (…)` leg
//! `insertIfNotQueued` prepends (`artwork_queue_repository.go:99-102`), which made its
//! statement a SQLite parse error. Both now follow a render verified against pinned
//! `github.com/Masterminds/squirrel@v1.5.4` (`go run` over the same builder Go builds), and
//! `enqueue_if_missing_renderer_executes_the_cte_insert` pins the fixed statement.
//!

use std::convert::Infallible;

use chrono::DateTime;
use chrono::Utc;
use nd_db::artwork_queue_repo::{
    self, enqueue_all_missing_in_tx, enqueue_if_missing_in_tx, enqueue_in_tx, enqueue_sql,
    ArtworkQueueItem, ArtworkQueueRepo, ArtworkQueueStat, EnqueueMode, UnsupportedKind, CHUNK_SIZE,
    CONFLICT_ENQUEUE, CONFLICT_PRESERVE_BACKOFF, IMAGE_TYPE_PRIMARY,
};
use nd_db::artwork_repo::{delete_for_items_in_tx, delete_for_items_sql, owner_table, BATCH_SIZE};
use nd_db::pool::{Pool, PoolConfig};
use nd_db::store::SQLStore;
use nd_db::{time, DbOperationError, DbRuntimeError, TransactionStage};
use nd_dbschema::schema::apply_fresh_schema;
use rusqlite::ffi;
use rusqlite::types::Value as SqlValue;
use rusqlite::Error as SqliteError;

/// The caller's enqueue stamp as stored TEXT (rusqlite's `chrono` spelling,
/// `crate::time`). Go samples `time.Now()` inside the repository
/// (`artwork_queue_repository.go:63,71,151`); this port takes it from the caller, so the
/// tests own it.
const NOW_TEXT: &str = "2023-11-14 22:13:20+00:00";
/// An ALREADY-QUEUED row's `retry_at`/`enqueued_at`: a spent lifecycle, six years earlier.
const SPENT_TEXT: &str = "2017-07-13 00:26:40+00:00";

#[must_use]
fn now() -> DateTime<Utc> {
    time::parse(NOW_TEXT).unwrap_or_else(|| panic!("{NOW_TEXT} parses"))
}
#[must_use]
fn spent() -> DateTime<Utc> {
    time::parse(SPENT_TEXT).unwrap_or_else(|| panic!("{SPENT_TEXT} parses"))
}

fn dsn(path: &std::path::Path) -> String {
    format!("file:{}?mode=rwc&_foreign_keys=on", path.display())
}

/// One isolated on-disk DB per invocation. `schema = false` leaves it UNMIGRATED, which is
/// what makes "this call issued no statement at all" non-vacuous: any SQL would then fail
/// with `no such table`.
fn fresh_store(label: &str, schema: bool) -> (SQLStore, std::path::PathBuf) {
    let path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
        &format!("nd-db-scanner-artwork-{label}-"),
        ".db",
    ));
    if path.exists() {
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("cannot clear {path:?}: {e}"));
    }
    let (pool, first) = Pool::new(&PoolConfig::with_size(dsn(&path), 2)).unwrap();
    if schema {
        apply_fresh_schema(&first).unwrap();
    }
    pool.put_back(first).expect("return configured handle");
    (SQLStore::new(pool), path)
}

/// Run one closure on a leased connection; lease failures panic, the closure's own
/// `Result` comes back untouched for assertion.
fn lease<T>(store: &SQLStore, call: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    store
        .with_connection_sync(|conn| Ok(call(conn)))
        .unwrap_or_else(|e| panic!("lease failed: {e}"))
}

/// A raw fixture statement (seed rows and the test-local trigger DDL). Queue and state
/// rows are seeded ONLY here or through the production write API — never as a side effect
/// of a read.
fn fixture_insert(store: &SQLStore, sql: &str, args: &[SqlValue]) {
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

/// One scalar probe on a FRESH lease (state after a commit or a rollback).
fn scalar<T>(store: &SQLStore, sql: &str) -> T
where
    T: rusqlite::types::FromSql,
{
    lease(store, |conn| {
        conn.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    })
}

/// Library 1, so `EnqueueAllMissing`'s INSERT…SELECT reads a real entity table whose
/// `album`/`artist` rows satisfy the FK.
fn seed_library(store: &SQLStore) {
    fixture_insert(
        store,
        "INSERT INTO library (id, name, path, remote_path) VALUES (?, ?, ?, ?)",
        &[integer(1), text("Main"), text("/music"), text("")],
    );
}

/// `album` rows, ids in insert order.
fn seed_albums(store: &SQLStore, ids: &[&str]) {
    for id in ids {
        fixture_insert(
            store,
            "INSERT INTO album (id, name, library_id) VALUES (?, ?, ?)",
            &[text(id), text(&format!("album-{id}")), integer(1)],
        );
    }
}

/// One `item_artwork` state row. `hash` distinguishes resolved (`<> ''`) from
/// settled-absent (`= ''`); BOTH exclude an entity from `EnqueueAllMissing` (`:66`).
fn seed_item_artwork(store: &SQLStore, kind: &str, id: &str, image_type: &str, hash: &str) {
    fixture_insert(
        store,
        "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source, source_path, \
         ref_mtime, attempted_at, updated_at, trace, last_failure) \
         VALUES (?, ?, ?, ?, ?, '', 0, ?, ?, '[]', '[]')",
        &[
            text(kind),
            text(id),
            text(image_type),
            text(hash),
            text(if hash.is_empty() { "" } else { "cover" }),
            text(SPENT_TEXT),
            text(SPENT_TEXT),
        ],
    );
}

/// An ALREADY-QUEUED row with a spent lifecycle: `attempts` spent, `retry_at` in the past,
/// a non-empty trace, a distinctive priority. Every mode difference below reads those
/// fields through the executed `Get`.
fn seed_queue_row(store: &SQLStore, kind: &str, id: &str, priority: i64, attempts: i64) {
    fixture_insert(
        store,
        "INSERT INTO artwork_queue (item_kind, item_id, image_type, priority, attempts, \
         retry_at, enqueued_at, trace) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            text(kind),
            text(id),
            text(IMAGE_TYPE_PRIMARY),
            integer(priority),
            integer(attempts),
            text(SPENT_TEXT),
            text(SPENT_TEXT),
            text(r#"["prior-failure"]"#),
        ],
    );
}

/// An `ArtworkQueueItem` shaped like Go's scanner's (`scanner/image_changes.go:76-78`,
/// `scanArtworkItem`): kind prefix, entity id, EMPTY image_type (`cmp.Or` → `primary`,
/// `:155`), and a caller-chosen priority.
fn item(kind: &str, id: &str, priority: i64) -> ArtworkQueueItem {
    ArtworkQueueItem {
        item_kind: kind.to_string(),
        item_id: id.to_string(),
        image_type: String::new(),
        priority,
        attempts: 0,
        retry_at: None,
        enqueued_at: None,
        trace: String::new(),
    }
}

fn ids(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}-{i}")).collect()
}

/// Commit one `delete_for_items_in_tx` call through the typed seam as a caller would.
fn delete_committed(store: &SQLStore, kind: &str, ids: &[String]) -> usize {
    store
        .try_with_tx_sync::<usize, Infallible>(|tx| delete_for_items_in_tx(tx, kind, ids))
        .unwrap_or_else(|e| {
            panic!(
                "delete_for_items_in_tx({kind}, {} ids) failed: {e}",
                ids.len()
            )
        })
}

/// Commit one `enqueue_in_tx` call through the typed seam.
fn enqueue_committed(store: &SQLStore, items: &[ArtworkQueueItem], mode: EnqueueMode) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| enqueue_in_tx(tx, items, mode, &now()))
        .unwrap_or_else(|e| panic!("enqueue_in_tx({mode:?}, {} items) failed: {e}", items.len()));
}

/// Commit one `enqueue_all_missing_in_tx` call and report its affected-row count.
fn enqueue_all_missing_committed(store: &SQLStore, kind: &str, priority: i64) -> i64 {
    store
        .try_with_tx_sync::<i64, UnsupportedKind>(|tx| {
            enqueue_all_missing_in_tx(tx, kind, priority, &now())
        })
        .unwrap_or_else(|e| panic!("enqueue_all_missing_in_tx({kind}) failed: {e}"))
}

/// Commit one `enqueue_if_missing_in_tx` call through the typed seam.
fn enqueue_if_missing_committed(store: &SQLStore, items: &[ArtworkQueueItem]) {
    store
        .try_with_tx_sync::<(), Infallible>(|tx| enqueue_if_missing_in_tx(tx, items, &now()))
        .unwrap_or_else(|e| {
            panic!(
                "enqueue_if_missing_in_tx({} items) failed: {e}",
                items.len()
            )
        });
}

/// The executed queue `Get` of one row — the STATE the tests key on, never a count.
fn queue_row(store: &SQLStore, kind: &str, id: &str) -> ArtworkQueueItem {
    lease(store, |conn| {
        ArtworkQueueRepo::new(conn)
            .get(kind, id, IMAGE_TYPE_PRIMARY)
            .unwrap_or_else(|e| panic!("queue get({kind},{id}) failed: {e}"))
    })
}

/// `priority/attempts/retry_at/enqueued_at/trace` of one queue row, through `Get`.
fn row_state(store: &SQLStore, kind: &str, id: &str) -> String {
    let row = queue_row(store, kind, id);
    format!(
        "{}/{}/{}/{}/{}",
        row.priority,
        row.attempts,
        time::to_text(&row.retry_at.expect("NOT NULL column hydrates")),
        time::to_text(&row.enqueued_at.expect("NOT NULL column hydrates")),
        row.trace
    )
}

/// The spent-lifecycle state a fixture seeds, spelled the same way as [`row_state`].
fn spent_state(priority: i64, attempts: i64) -> String {
    format!("{priority}/{attempts}/{SPENT_TEXT}/{SPENT_TEXT}/[\"prior-failure\"]")
}

/// The state a fresh enqueue writes: caller priority, attempts 0, both stamps = now, and
/// the DDL's `trace` default `'[]'`.
fn fresh_state(priority: i64) -> String {
    format!("{priority}/0/{NOW_TEXT}/{NOW_TEXT}/[]")
}

/// `CountQueued` + `Count` on a fresh lease: grouped `(kind, priority, count)` rows in
/// Go's `ORDER BY item_kind, priority desc` order, plus the total.
fn queue_stats(store: &SQLStore) -> (Vec<ArtworkQueueStat>, i64) {
    lease(store, |conn| {
        let repo = ArtworkQueueRepo::new(conn);
        (
            repo.count_queued(&[], &[]).expect("count_queued"),
            repo.count().expect("count"),
        )
    })
}

fn rendered_legs(items: &[ArtworkQueueItem], mode: EnqueueMode) -> usize {
    enqueue_sql(items, mode.conflict(), &now()).len()
}

/// The extended SQLite code — proof the seam kept the concrete cause rather than a
/// stringified wrapper.
fn extended_code(err: &rusqlite::Error) -> Option<i32> {
    match err {
        SqliteError::SqliteFailure(code, _) => Some(code.extended_code),
        _ => None,
    }
}

/// Assert a Body-stage SQLite failure carrying `want`, not busy-classified.
fn assert_body_sqlite<E: std::error::Error + 'static>(err: &DbRuntimeError<E>, want: Option<i32>) {
    match err {
        DbRuntimeError::Sqlite { stage, source } => {
            assert_eq!(
                *stage,
                TransactionStage::Body,
                "the executor owns the stage"
            );
            assert_eq!(extended_code(source), want, "raw SQLite cause: {source}");
            assert!(
                !nd_db::store::is_busy(source),
                "a constraint/parse failure is not a busy failure"
            );
        }
        other => panic!("expected a Body-stage SQLite failure, got {other:?}"),
    }
}

/// DeleteForItems at 0/1/200/201 ids: rendered chunk counts, committed rows, and the
/// affected-row counts (`:152` binds kind + chunk; Go's `delete` DISCARDS the count, so
/// the port's value is advisory observability only).
#[test]
fn delete_for_items_zero_one_boundary_and_boundary_plus_one() {
    let (store, path) = fresh_store("delete-chunks", true);
    // 500 state rows of kind `al` (al-0..al-499) plus an id nobody names, and 201
    // DECOY rows that carry the SAME ids under kind `mf` and must survive.
    for id in ids("al", 500) {
        seed_item_artwork(&store, "al", &id, IMAGE_TYPE_PRIMARY, "h");
    }
    for id in ids("al", 201) {
        seed_item_artwork(&store, "mf", &id, IMAGE_TYPE_PRIMARY, "h");
    }
    seed_item_artwork(&store, "al", "al-untouched", IMAGE_TYPE_PRIMARY, "h");
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        702,
        "500 al + 201 mf decoys + 1 never-named"
    );

    // 0 ids: renders no chunk, runs no statement, changes nothing.
    assert_eq!(delete_committed(&store, "al", &[]), 0);
    assert_eq!(
        delete_for_items_sql("al", &[]).len(),
        0,
        "empty input renders no chunk"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        702
    );

    // 1 id: one chunk, one affected row; the same id under another kind survives.
    let one = vec!["al-0".to_string()];
    assert_eq!(delete_for_items_sql("al", &one).len(), 1);
    assert_eq!(delete_committed(&store, "al", &one), 1);
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'al' AND item_id = 'al-0'"
        ),
        0,
        "the named row is gone"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf' AND item_id = 'al-0'"
        ),
        1,
        "the same id under another kind survives (`Eq{{item_kind, item_id}}`, `:152`)"
    );

    // Boundary: exactly 200 ids = ONE chunk, all removed.
    let two_hundred: Vec<String> = (1..=200).map(|i| format!("al-{i}")).collect();
    assert_eq!(
        delete_for_items_sql("al", &two_hundred).len(),
        1,
        "200 = one chunk at BATCH_SIZE {BATCH_SIZE}"
    );
    assert_eq!(delete_committed(&store, "al", &two_hundred), 200);

    // Boundary + 1: 201 ids = two chunks (200 + 1), and the extra id in chunk 2 lands.
    let two_hundred_and_one: Vec<String> = (201..=401).map(|i| format!("al-{i}")).collect();
    assert_eq!(
        delete_for_items_sql("al", &two_hundred_and_one).len(),
        2,
        "201 ids split 200 + 1"
    );
    assert_eq!(delete_committed(&store, "al", &two_hundred_and_one), 201);
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'al' AND item_id = 'al-401'"
        ),
        0,
        "chunk 2's single extra id was deleted too"
    );

    // What is left of kind `al`: al-402..al-499 (98) plus al-untouched = 99.
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'al'"
        ),
        99,
        "500 - 202 named + al-untouched"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        201,
        "every decoy survives"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_id = 'al-untouched'"
        ),
        1
    );
    std::fs::remove_file(&path).ok();
}

/// DeleteForItems does NOT restrict `image_type` (`Eq{item_kind, item_id}` only, `:152`),
/// so every slot of a named kind+id goes; and a kind NAME binds a token the column never
/// holds — zero rows, no error, which is Go's own behavior for a mis-keyed bind.
#[test]
fn delete_for_items_covers_every_image_type_and_kind_names_match_nothing() {
    let (store, path) = fresh_store("delete-scope", true);
    seed_item_artwork(&store, "al", "a1", IMAGE_TYPE_PRIMARY, "h-primary");
    seed_item_artwork(&store, "al", "a1", "cover", "h-cover");
    seed_item_artwork(&store, "al", "a1", "background", "h-bg");
    seed_item_artwork(&store, "mf", "a1", IMAGE_TYPE_PRIMARY, "h-mf");

    let a1 = vec!["a1".to_string()];
    assert!(
        !delete_for_items_sql("al", &a1)[0].0.contains("image_type"),
        "the rendered leg carries only item_kind and item_id"
    );
    assert_eq!(
        delete_committed(&store, "album", &a1),
        0,
        "a kind NAME matches nothing while matching prefix rows still exist"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'al' AND item_id = 'a1'"
        ),
        3,
        "the negative kind-name check did not delete the target rows"
    );
    assert_eq!(
        delete_committed(&store, "al", &a1),
        3,
        "all three slots of al/a1"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        1,
        "the other kind's row of the same id survives"
    );
    assert_eq!(
        delete_committed(&store, "al", &a1),
        0,
        "already deleted: a zero-row chunk is a successful NO-OP whose loop continues"
    );
    assert_eq!(
        scalar::<i64>(
            &store,
            "SELECT count(*) FROM item_artwork WHERE item_kind = 'mf'"
        ),
        1
    );
    std::fs::remove_file(&path).ok();
}

/// The repaired renderer contract, pinned in text: the verb is the DELETE that
/// `sqlRepository.delete` builds (`sql_base_repository.go:644-647`) — not the `SELECT *`
/// this leg used to render, which executed as a query and deleted nothing — and the bind
/// order is the ids BEFORE the kind, exactly as pinned squirrel v1.5.4 renders
/// `Eq{item_kind, item_id}` (`Eq.ToSql` sorts keys, `expr.go:145-160`):
/// ```text
/// DELETE FROM item_artwork WHERE item_id IN (?,?) AND item_kind = ?   ARGS: [a b al]
/// ```
#[test]
fn delete_for_items_renders_the_delete_verb_with_ids_bound_before_kind() {
    let statements = delete_for_items_sql("al", &["a".to_string(), "b".to_string()]);
    assert_eq!(statements.len(), 1);
    let (sql, args) = &statements[0];
    assert_eq!(
        sql,
        "DELETE FROM item_artwork WHERE item_id IN (?, ?) AND item_kind = ?"
    );
    assert_eq!(
        args,
        &[
            nd_db::sqlrepo::Value::Text("a".to_string()),
            nd_db::sqlrepo::Value::Text("b".to_string()),
            nd_db::sqlrepo::Value::Text("al".to_string())
        ],
        "the IN list binds before the kind"
    );

    // Executed on the real schema, so the verb and the bind order are proven against the
    // table rather than only against the rendered text.
    let (store, path) = fresh_store("delete-verb", true);
    seed_item_artwork(&store, "al", "a", IMAGE_TYPE_PRIMARY, "h-a");
    seed_item_artwork(&store, "al", "b", IMAGE_TYPE_PRIMARY, "h-b");
    seed_item_artwork(&store, "mf", "a", IMAGE_TYPE_PRIMARY, "h-mf-a");
    assert_eq!(
        delete_committed(&store, "al", &["a".to_string(), "b".to_string()]),
        2
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        1
    );
    assert_eq!(
        scalar::<String>(&store, "SELECT item_kind FROM item_artwork"),
        "mf",
        "only the other kind's row survives"
    );
    std::fs::remove_file(&path).ok();
}

/// `Enqueue` (Reset) over a SPENT row: attempts→0, retry_at/enqueued_at→the caller's now,
/// trace→`'[]'`, priority = MAX(existing, incoming) — exact row state read through the
/// executed `Get`, plus `CountQueued`'s grouping.
#[test]
fn enqueue_reset_overwrites_attempts_backoff_and_trace() {
    let (store, path) = fresh_store("enqueue-reset", true);
    seed_queue_row(&store, "al", "a1", 5, 4);
    assert_eq!(
        row_state(&store, "al", "a1"),
        spent_state(5, 4),
        "the fixture really carries a spent lifecycle"
    );

    // LOWER incoming priority: MAX keeps 5, but the lifecycle still resets.
    enqueue_committed(&store, &[item("al", "a1", 2)], EnqueueMode::Reset);
    assert_eq!(
        row_state(&store, "al", "a1"),
        fresh_state(5),
        "priority = MAX(priority, excluded.priority) with attempts = 0, fresh stamps, trace '[]'"
    );

    // HIGHER incoming priority: the row takes it.
    enqueue_committed(&store, &[item("al", "a1", 9)], EnqueueMode::Reset);
    let row = queue_row(&store, "al", "a1");
    assert_eq!(row.priority, 9);
    assert_eq!(
        row.image_type, IMAGE_TYPE_PRIMARY,
        "empty image_type defaulted (`cmp.Or`, `:155`)"
    );
    assert_eq!(
        row.item_kind, "al",
        "the kind prefix passes through as the renderer types it"
    );

    let (stats, total) = queue_stats(&store);
    assert_eq!(total, 1);
    assert_eq!(
        stats,
        vec![ArtworkQueueStat {
            item_kind: "al".to_string(),
            priority: 9,
            count: 1,
        }]
    );
    std::fs::remove_file(&path).ok();
}

/// `EnqueuePreservingBackoff` over the SAME spent fixture: only `priority` moves.
/// `retry_at`, `attempts`, `enqueued_at` and `trace` all survive — the whole point of the
/// mode (`:52-53`).
#[test]
fn enqueue_preserving_backoff_keeps_attempts_retry_trace_and_enqueued_at() {
    let (store, path) = fresh_store("enqueue-preserve", true);
    seed_queue_row(&store, "al", "a1", 5, 4);
    assert_eq!(row_state(&store, "al", "a1"), spent_state(5, 4));

    enqueue_committed(&store, &[item("al", "a1", 9)], EnqueueMode::PreserveBackoff);
    let after = queue_row(&store, "al", "a1");
    assert_eq!(after.priority, 9, "the ONLY column the preserve arm sets");
    assert_eq!(after.attempts, 4, "the spent attempt counter survives");
    assert_eq!(
        after.retry_at,
        Some(spent()),
        "the existing backoff is not reset, so a read-through cannot clear it"
    );
    assert_eq!(
        after.enqueued_at,
        Some(spent()),
        "the lifecycle start survives"
    );
    assert_eq!(
        after.trace, r#"["prior-failure"]"#,
        "the prior walk survives"
    );

    // A LOWER priority cannot demote the row either.
    enqueue_committed(&store, &[item("al", "a1", 1)], EnqueueMode::PreserveBackoff);
    assert_eq!(queue_row(&store, "al", "a1").priority, 9);
    std::fs::remove_file(&path).ok();
}

/// The two modes over one identical fixture: two rows seeded with the SAME spent
/// lifecycle, each enqueued with the same priority and the same `now`, and the only
/// difference is the conflict suffix (`artwork_queue_repository.go:47-57` both funnel into
/// the private `enqueue`, `:150-163`). Reset clears the budget; Preserve touches priority
/// alone.
#[test]
fn reset_and_preserve_differ_on_the_same_spent_rows() {
    let (store, path) = fresh_store("enqueue-modes", true);
    seed_queue_row(&store, "al", "reset", 5, 4);
    seed_queue_row(&store, "al", "keep", 5, 4);
    let items = &[item("al", "reset", 7)];
    assert_eq!(rendered_legs(items, EnqueueMode::Reset), 1);
    assert_eq!(rendered_legs(items, EnqueueMode::PreserveBackoff), 1);

    enqueue_committed(&store, items, EnqueueMode::Reset);
    enqueue_committed(
        &store,
        &[item("al", "keep", 7)],
        EnqueueMode::PreserveBackoff,
    );
    let reset = queue_row(&store, "al", "reset");
    let keep = queue_row(&store, "al", "keep");
    assert_eq!(
        (reset.priority, reset.attempts),
        (7, 0),
        "Reset zeroes the budget"
    );
    assert_eq!(reset.retry_at, Some(now()));
    assert_eq!(reset.trace, "[]");
    assert_eq!(
        (keep.priority, keep.attempts),
        (7, 4),
        "Preserve raises priority only"
    );
    assert_eq!(
        keep.retry_at,
        Some(spent()),
        "a spent backoff survives (`:54-56`)"
    );
    assert_eq!(keep.trace, r#"["prior-failure"]"#);

    // The arms the seam ran are the two existing rendered suffixes.
    assert!(CONFLICT_ENQUEUE.contains("attempts = 0"));
    assert!(CONFLICT_ENQUEUE.contains("trace = '[]'"));
    assert!(!CONFLICT_PRESERVE_BACKOFF.contains("attempts"));
    assert!(!CONFLICT_PRESERVE_BACKOFF.contains("retry_at"));
    std::fs::remove_file(&path).ok();
}

/// Duplicate queue keys inside ONE chunk: the PRIMARY KEY is
/// `(item_kind, item_id, image_type)` (DDL line 50), so the second occurrence resolves to
/// the row the first inserted. Reset collapses them to ONE row with MAX priority; the
/// DO NOTHING arm (all-missing) keeps the existing row's values untouched. The reported
/// counts are `sqlite3_changes()`, which counts BOTH VALUES rows here (verified with
/// `sqlite3` over this DDL: 2), so they are never read as insert counts.
#[test]
fn duplicate_queue_keys_collapse_to_one_row_per_mode() {
    let (store, path) = fresh_store("enqueue-duplicates", true);
    seed_library(&store);
    seed_albums(&store, &["a1"]);

    enqueue_committed(
        &store,
        &[item("al", "a1", 2), item("al", "a1", 8)],
        EnqueueMode::Reset,
    );
    let (stats, total) = queue_stats(&store);
    assert_eq!(total, 1, "one row, not two");
    assert_eq!(stats[0].count, 1);
    let row = queue_row(&store, "al", "a1");
    assert_eq!(
        row.priority, 8,
        "MAX(priority, excluded.priority) over the duplicate"
    );
    assert_eq!(row.attempts, 0);
    assert_eq!(row.enqueued_at, Some(now()));

    // EnqueueAllMissing now SELECTs a1 (no item_artwork row) and hits DO NOTHING: the
    // existing row keeps the values Reset wrote. sqlite3_changes reports 0 for a candidate
    // whose only outcome is a skipped conflict (verified with sqlite3 over this DDL), so
    // the count is 0 while the state assertion below is the real evidence.
    assert_eq!(enqueue_all_missing_committed(&store, "al", 3), 0);
    let after = queue_row(&store, "al", "a1");
    assert_eq!(
        after.priority, 8,
        "DO NOTHING does not bump priority (`:95-96`)"
    );
    assert_eq!(
        after.enqueued_at,
        Some(now()),
        "DO NOTHING does not reset stamps"
    );
    std::fs::remove_file(&path).ok();
}

/// Enqueue at the 100-row chunk cap: 0/1/100/101 items render 0/1/1/2 legs, every row
/// lands, and the 101st lives in chunk 2.
#[test]
fn enqueue_chunk_boundary_100_and_101() {
    let (store, path) = fresh_store("enqueue-chunks", true);
    assert_eq!(rendered_legs(&[], EnqueueMode::Reset), 0);
    assert_eq!(rendered_legs(&[item("al", "a1", 1)], EnqueueMode::Reset), 1);
    assert_eq!(
        rendered_legs(&[item("al", "a1", 1)], EnqueueMode::PreserveBackoff),
        1
    );

    let hundred: Vec<ArtworkQueueItem> = ids("mf", CHUNK_SIZE)
        .iter()
        .map(|id| item("mf", id, 1))
        .collect();
    assert_eq!(
        rendered_legs(&hundred, EnqueueMode::Reset),
        1,
        "100 = one chunk"
    );
    enqueue_committed(&store, &hundred, EnqueueMode::Reset);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM artwork_queue"),
        100
    );

    let one_more: Vec<ArtworkQueueItem> = ids("mf", CHUNK_SIZE + 1)
        .iter()
        .map(|id| item("mf", id, 2))
        .collect();
    assert_eq!(
        rendered_legs(&one_more, EnqueueMode::PreserveBackoff),
        2,
        "101 items split at CHUNK_SIZE {CHUNK_SIZE}"
    );
    enqueue_committed(&store, &one_more, EnqueueMode::PreserveBackoff);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM artwork_queue"),
        101,
        "chunk 2's extra row landed"
    );
    // Preserve raised every row's priority to 2 without re-stamping the lifecycle.
    let (stats, total) = queue_stats(&store);
    assert_eq!(total, 101);
    assert_eq!(
        stats,
        vec![ArtworkQueueStat {
            item_kind: "mf".to_string(),
            priority: 2,
            count: 101,
        }],
        "one group, in CountQueued's item_kind/priority-desc order"
    );
    std::fs::remove_file(&path).ok();
}

/// Empty input issues NO SQL: on an UNMIGRATED DB the empty calls commit `Ok` while the
/// one-item calls fail with `no such table`, so the empty case provably never reached
/// SQLite. The unsupported kind is rejected BEFORE the statement, which the failure of
/// the supported kind makes non-vacuous.
#[test]
fn empty_input_issues_no_statement_on_an_unmigrated_database() {
    let (store, path) = fresh_store("empty-no-sql", false);

    let empty =
        store.try_with_tx_sync::<usize, Infallible>(|tx| delete_for_items_in_tx(tx, "al", &[]));
    assert_eq!(empty.expect("empty delete is Ok"), 0);

    let empty_enqueue = store.try_with_tx_sync::<(), Infallible>(|tx| {
        enqueue_in_tx(tx, &[], EnqueueMode::Reset, &now())
    });
    assert!(empty_enqueue.is_ok(), "empty enqueue issues no statement");

    // The same seam with ONE item does reach SQLite and fails for lack of schema.
    let one_enqueue = store.try_with_tx_sync::<(), Infallible>(|tx| {
        enqueue_in_tx(tx, &[item("al", "a1", 1)], EnqueueMode::Reset, &now())
    });
    assert_body_sqlite(&one_enqueue.unwrap_err(), Some(ffi::SQLITE_ERROR));

    let one_delete = store.try_with_tx_sync::<usize, Infallible>(|tx| {
        delete_for_items_in_tx(tx, "al", &["a1".to_string()])
    });
    assert_body_sqlite(&one_delete.unwrap_err(), Some(ffi::SQLITE_ERROR));

    let supported = store.try_with_tx_sync::<i64, UnsupportedKind>(|tx| {
        enqueue_all_missing_in_tx(tx, "al", 1, &now())
    });
    assert_body_sqlite(&supported.unwrap_err(), Some(ffi::SQLITE_ERROR));

    let unsupported = store.try_with_tx_sync::<i64, UnsupportedKind>(|tx| {
        enqueue_all_missing_in_tx(tx, "dc", 1, &now())
    });
    match unsupported {
        Err(DbRuntimeError::Domain(UnsupportedKind { kind })) => assert_eq!(kind, "dc"),
        other => panic!("expected the Domain arm before any SQL, got {other:?}"),
    }
    std::fs::remove_file(&path).ok();
}

/// `EnqueueAllMissing` excludes entities that already have an `item_artwork` row —
/// RESOLVED (`hash <> ''`) and SETTLED-ABSENT (`hash = ''`) alike, because Go's anti-join
/// (`:66`) tests only for the row's existence — and leaves an already-QUEUED entity's row
/// untouched via `DO NOTHING`. `item_artwork` is never written.
#[test]
fn enqueue_all_missing_excludes_resolved_and_skips_already_queued() {
    let (store, path) = fresh_store("all-missing", true);
    seed_library(&store);
    seed_albums(&store, &["a1", "a2", "a3", "a4"]);
    seed_item_artwork(&store, "al", "a1", IMAGE_TYPE_PRIMARY, "h-resolved");
    seed_item_artwork(&store, "al", "a2", IMAGE_TYPE_PRIMARY, "");
    seed_queue_row(&store, "al", "a3", 7, 4);
    // a4: no state row, no queue row — the only entity this call can insert.

    let n = enqueue_all_missing_committed(&store, "al", 3);
    assert_eq!(
        n, 1,
        "executeSQL's RowsAffected (Go's return value, `:64-67`) is `sqlite3_changes()` for
        this INSERT…SELECT: the a4 row the statement inserted, and NOT the a3 row the
        anti-join filtered out of the SELECT nor the DO NOTHING conflict it would have hit.
        Verified with sqlite3 over this DDL: one filtered-out candidate whose only outcome
        is a skipped conflict reports 0, one insert reports 1"
    );

    let (stats, total) = queue_stats(&store);
    assert_eq!(total, 2, "the pre-queued row plus the new one");
    assert_eq!(
        stats,
        vec![
            ArtworkQueueStat {
                item_kind: "al".to_string(),
                priority: 7,
                count: 1
            },
            ArtworkQueueStat {
                item_kind: "al".to_string(),
                priority: 3,
                count: 1
            },
        ],
        "CountQueued order is item_kind, priority desc"
    );
    assert_eq!(
        row_state(&store, "al", "a3"),
        spent_state(7, 4),
        "DO NOTHING left the already-queued row completely untouched"
    );
    assert_eq!(
        row_state(&store, "al", "a4"),
        fresh_state(3),
        "the new row carries the caller's priority, attempts 0, both stamps = now, trace '[]'"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        2,
        "EnqueueAllMissing never writes item_artwork"
    );

    // A repeated scan pass is STABLE in state: a4 is still unresolved so it is selected
    // again, but DO NOTHING leaves the row untouched and `sqlite3_changes()` does not count
    // a skipped conflict (verified with sqlite3 over this DDL), so the count drops to 0
    // while the state assertions below carry the proof.
    assert_eq!(
        enqueue_all_missing_committed(&store, "al", 3),
        0,
        "both un-resolved entities are selected again and both resolve to a skipped conflict"
    );
    assert_eq!(
        row_state(&store, "al", "a4"),
        fresh_state(3),
        "repeated passes do not re-stamp or bump priority"
    );
    assert_eq!(row_state(&store, "al", "a3"), spent_state(7, 4));
    assert_eq!(queue_stats(&store).1, 2);

    // The `ar` leg resolves to the `artist` table, so the owner-table swap is exercised on
    // a second entity table too.
    fixture_insert(
        &store,
        "INSERT INTO artist (id, name, order_artist_name) VALUES (?, ?, ?)",
        &[text("ar-1"), text("Artist"), text("Artist")],
    );
    assert_eq!(enqueue_all_missing_committed(&store, "ar", 4), 1);
    assert_eq!(row_state(&store, "ar", "ar-1"), fresh_state(4));
    std::fs::remove_file(&path).ok();
}

/// Go rejects a kind with no `artworkOwnerTables` entry (`:59-62`) with an error and NO
/// SQL, rather than running a statement that silently matches nothing; the port keeps that
/// as the typed `Domain` arm. The whitelist is also the only source of the interpolated
/// table name, so no caller text can become an identifier.
#[test]
fn unsupported_owner_kind_is_an_error_and_kind_names_are_rejected() {
    let (store, path) = fresh_store("kind-domain", true);
    seed_library(&store);
    seed_albums(&store, &["a1"]);
    assert_eq!(owner_table("al"), Some("album"));
    assert_eq!(owner_table("mf"), Some("media_file"));

    for kind in ["dc", "album", "media_file", "xx", ""] {
        let outcome = store.try_with_tx_sync::<i64, UnsupportedKind>(|tx| {
            enqueue_all_missing_in_tx(tx, kind, 3, &now())
        });
        match outcome {
            Err(DbRuntimeError::Domain(UnsupportedKind { kind: reported })) => {
                assert_eq!(reported, kind, "the rejected token is reported back")
            }
            other => panic!("expected the Domain arm for {kind:?}, got {other:?}"),
        }
    }
    assert_eq!(
        queue_stats(&store).1,
        0,
        "no queue row was inserted by any rejected kind"
    );
    // The supported kind does reach the entity table — the non-vacuity control.
    assert_eq!(enqueue_all_missing_committed(&store, "al", 3), 1);
    std::fs::remove_file(&path).ok();
}

/// One 101-item call produces two internal chunks: the poison trigger on item 101 verifies
/// the first 100 rows already exist, then aborts chunk 2 with `SQLITE_CONSTRAINT_TRIGGER`.
/// The caller's transaction rolls EVERY leg back: the delete, chunk 1's queue rows, and the
/// executed `Get` of one of them on a fresh lease.
#[test]
fn second_chunk_abort_rolls_back_the_delete_and_the_first_chunk() {
    let (store, path) = fresh_store("chunk-rollback", true);
    let doomed = ids("al", 3);
    for id in &doomed {
        seed_item_artwork(&store, "al", id, IMAGE_TYPE_PRIMARY, "h");
    }

    // The poison trigger distinguishes a proper second-chunk abort from an implementation
    // that reaches the poison row before the first 100-item chunk has executed.
    fixture_insert(
        &store,
        concat!(
            "CREATE TRIGGER fixture_queue_chunk_fails BEFORE INSERT ON artwork_queue ",
            "FOR EACH ROW WHEN NEW.item_id = 'mf-poison' BEGIN ",
            "SELECT RAISE(ABORT, 'fixture: first chunk missing') ",
            "WHERE (SELECT count(*) FROM artwork_queue WHERE item_kind = 'mf') <> 100; ",
            "SELECT RAISE(ABORT, 'fixture: poisoned queue id'); END"
        ),
        &[],
    );

    let mut items: Vec<ArtworkQueueItem> = ids("mf", CHUNK_SIZE)
        .iter()
        .map(|id| item("mf", id, 1))
        .collect();
    items.push(item("mf", "mf-poison", 1));
    assert_eq!(rendered_legs(&items, EnqueueMode::Reset), 2);
    assert_eq!(delete_for_items_sql("al", &doomed).len(), 1);

    let failed = store.try_with_tx_sync::<usize, Infallible>(|tx| {
        let deleted = delete_for_items_in_tx(tx, "al", &doomed)?;
        assert_eq!(deleted, 3, "the delete leg ran inside the transaction");
        enqueue_in_tx(tx, &items, EnqueueMode::Reset, &now())?;
        Ok(deleted)
    });
    let err = failed.expect_err("the trigger aborts the second internal chunk");
    assert_body_sqlite(&err, Some(ffi::SQLITE_CONSTRAINT_TRIGGER));
    match &err {
        DbRuntimeError::Sqlite { source, .. } => assert!(
            source.to_string().contains("fixture: poisoned queue id"),
            "the trigger observed the first chunk before rejecting item 101: {source}"
        ),
        other => panic!("expected a SQLite trigger error, got {other:?}"),
    }

    // Fresh lease: nothing was published.
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        3,
        "the delete rolled back: all three state rows are back"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM artwork_queue"),
        0,
        "chunk 1's 100 rows rolled back with chunk 2's abort"
    );
    let (stats, total) = queue_stats(&store);
    assert_eq!(total, 0);
    assert!(stats.is_empty(), "CountQueued sees no groups either");
    assert!(
        matches!(
            lease(&store, |conn| ArtworkQueueRepo::new(conn).get(
                "mf",
                "mf-0",
                IMAGE_TYPE_PRIMARY
            )),
            Err(artwork_queue_repo::ArtworkQueueError::NotFound)
        ),
        "the executed Get reports NotFound, so chunk 1's row really is absent"
    );

    assert_eq!(store.idle_count().expect("pool state"), 2);
    lease(&store, |conn| {
        assert!(conn.is_autocommit(), "the lease came back reusable")
    });
    std::fs::remove_file(&path).ok();
}
/// The repo never begins or commits, so a DOMAIN failure from the caller AFTER both legs
/// succeeded rolls the whole bundle back: neither the delete nor the reset publishes.
#[test]
fn a_caller_domain_failure_rolls_back_both_legs() {
    #[derive(Debug, thiserror::Error)]
    #[error("fixture: caller aborted after the legs ran")]
    struct CallerAborted;

    let (store, path) = fresh_store("caller-rollback", true);
    seed_item_artwork(&store, "al", "a1", IMAGE_TYPE_PRIMARY, "h");
    seed_queue_row(&store, "al", "a2", 5, 4);

    let failed = store.try_with_tx_sync::<usize, CallerAborted>(|tx| {
        let deleted = delete_for_items_in_tx(tx, "al", &["a1".to_string()]).map_err(|e| {
            DbOperationError::<CallerAborted>::Sqlite(match e {
                DbOperationError::Sqlite(source) => source,
                DbOperationError::Domain(infallible) => match infallible {},
            })
        })?;
        enqueue_in_tx(tx, &[item("al", "a2", 9)], EnqueueMode::Reset, &now()).map_err(|e| {
            DbOperationError::<CallerAborted>::Sqlite(match e {
                DbOperationError::Sqlite(source) => source,
                DbOperationError::Domain(infallible) => match infallible {},
            })
        })?;
        // Returning `Ok(deleted)` would commit both legs; aborting proves the repo never
        // begins or commits on its own.
        let _ = deleted;
        Err(DbOperationError::Domain(CallerAborted))
    });
    match failed {
        Err(DbRuntimeError::Domain(CallerAborted)) => {}
        other => panic!("expected the caller's Domain failure, got {other:?}"),
    }
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        1,
        "the delete rolled back"
    );
    assert_eq!(
        row_state(&store, "al", "a2"),
        spent_state(5, 4),
        "the reset never published: attempts and both stamps are still the fixture's"
    );
    std::fs::remove_file(&path).ok();
}

/// The REPAIRED `EnqueueIfMissing` leg, executed. The renderer now emits the composite
/// `insertIfNotQueued` composes (`artwork_queue_repository.go:99-102`): CTE, then
/// `INSERT INTO artwork_queue (<enqueueColumns>)`, then the anti-join SELECT, then
/// `skipIfQueued`. Two filters are pinned here, both from Go's statement (`:85-86`, `:96`):
/// an entity with ANY `item_artwork` row of this kind + image_type is never queued —
/// resolved or settled-absent alike — and an entity ALREADY queued keeps its row
/// completely untouched, so a recheck cannot bump priority or retry_at.
#[test]
fn enqueue_if_missing_queues_only_unresolved_entities_and_never_touches_a_queued_row() {
    let (store, path) = fresh_store("if-missing", true);
    seed_library(&store);
    seed_albums(&store, &["a1", "a2", "a3", "a4"]);
    seed_item_artwork(&store, "al", "a1", IMAGE_TYPE_PRIMARY, "h-resolved");
    seed_item_artwork(&store, "al", "a2", IMAGE_TYPE_PRIMARY, "");
    seed_queue_row(&store, "al", "a3", 7, 4);
    // a4: no state row and no queue row — the only item this call can insert.

    let items: Vec<ArtworkQueueItem> = ["a1", "a2", "a3", "a4"]
        .iter()
        .map(|id| item("al", id, 3))
        .collect();
    let statements = artwork_queue_repo::enqueue_if_missing_sql(&items, &now());
    assert_eq!(statements.len(), 1, "4 items = one chunk");
    assert_eq!(
        statements[0].1.len(),
        4 * 4 + 2,
        "4 per row + now + now (`:74-79`)"
    );
    assert!(
        statements[0].0.contains("INSERT INTO artwork_queue (")
            && statements[0].0.contains("WITH new_items(")
            && statements[0]
                .0
                .contains("ON CONFLICT (item_kind, item_id, image_type) DO NOTHING"),
        "{}",
        statements[0].0
    );

    enqueue_if_missing_committed(&store, &items);
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM artwork_queue"),
        2,
        "the pre-queued row plus a4"
    );
    assert_eq!(
        row_state(&store, "al", "a4"),
        fresh_state(3),
        "a4 got the caller's priority with attempts 0 and both stamps = now"
    );
    assert_eq!(
        row_state(&store, "al", "a3"),
        spent_state(7, 4),
        "DO NOTHING left the already-queued row untouched (`:95-96`)"
    );
    assert!(
        matches!(
            lease(&store, |conn| ArtworkQueueRepo::new(conn).get(
                "al",
                "a1",
                IMAGE_TYPE_PRIMARY
            )),
            Err(artwork_queue_repo::ArtworkQueueError::NotFound)
        ),
        "the resolved entity was excluded by the anti-join"
    );
    assert!(
        matches!(
            lease(&store, |conn| ArtworkQueueRepo::new(conn).get(
                "al",
                "a2",
                IMAGE_TYPE_PRIMARY
            )),
            Err(artwork_queue_repo::ArtworkQueueError::NotFound)
        ),
        "the settled-absent entity is excluded too: Go tests only for the row's existence"
    );
    assert_eq!(
        scalar::<i64>(&store, "SELECT count(*) FROM item_artwork"),
        2,
        "EnqueueIfMissing never writes item_artwork"
    );

    // Recheck with a HIGHER priority: still nothing moves, which is the point of DO NOTHING.
    enqueue_if_missing_committed(&store, &[item("al", "a3", 9), item("al", "a4", 9)]);
    assert_eq!(row_state(&store, "al", "a3"), spent_state(7, 4));
    assert_eq!(
        row_state(&store, "al", "a4"),
        fresh_state(3),
        "priority not bumped"
    );
    // image_type is part of the anti-join (`:86`), not defaulted away: an item naming a slot
    // that HAS a state row is excluded, while the same id under `primary` (empty -> `cmp.Or`,
    // `:77`) is a different key and is queued.
    seed_item_artwork(&store, "al", "a5", "cover", "h-cover");
    let mut cover = item("al", "a5", 5);
    cover.image_type = "cover".to_string();
    enqueue_if_missing_committed(&store, &[cover, item("al", "a5", 5)]);
    assert!(
        matches!(
            lease(&store, |conn| ArtworkQueueRepo::new(conn)
                .get("al", "a5", "cover")),
            Err(artwork_queue_repo::ArtworkQueueError::NotFound)
        ),
        "the named slot has a state row, so the CTE's NOT EXISTS excludes it"
    );
    assert_eq!(
        row_state(&store, "al", "a5"),
        fresh_state(5),
        "the same id under primary is a different key and is queued"
    );
    assert_eq!(store.idle_count().expect("pool state"), 2);
    std::fs::remove_file(&path).ok();
}
