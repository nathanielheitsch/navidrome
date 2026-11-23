//! SC-05 fault verticals: the abort-policy arms of `scanner/phase_1_folders.go`, each
//! executed against a real tree and a real pool, with the "nothing was written" half of
//! the contract asserted rather than assumed.
//!
//! Go's two tag-read arms are distinct and are tested separately:
//! * `adapters/gotaglib/gotaglib.go:34-43` — a per-file parse failure `continue`s and
//!   `Parse` returns `nil`, so the file is **omitted** and the folder survives
//!   (`tag_read_failure_policy`).
//! * `core/storage/local/local.go:97-103` — `fs.Stat` failing fails the whole batch,
//!   which `loadTagsFromFiles` returns (`:297-300`) and `processFolder` turns into
//!   `sendWarning` + **`return entry, nil`** (`:274-279`): the folder row is still
//!   persisted, with no tracks, albums, artists, tags, or queue items
//!   (`import_failure_persists_folder_only`).
//!
//! The second arm's trigger — a stat that fails after the listing succeeded, i.e. a file
//! that vanished mid-scan — cannot be produced by any POSIX permission mode: a chmod-`000`
//! file still `lstat`s and `stat`s fine, so the failure lands on OPEN and takes the
//! per-file skip arm instead. The wiring is therefore pinned deterministically by the
//! lib-side unit test `phase1::unit_tests::vanished_entry_produces_the_batch_failure_text`,
//! and the observable *consequence* of the arm (folder persisted, nothing track-shaped) is
//! executed here through a folder whose every audio file fails to parse. No race-based
//! test is used.

mod common;

// Unix-only: `PermissionsExt` and the chmod-based arms below. Everything in this file
// except `tag_read_failure_policy` compiles and runs cross-platform.
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use common::{
    busy_store, children, copy_fixture, count, execute, fixture_source, fresh_store, job,
    job_with_album_pid, later_stamp, laterest_stamp, mapping, pool_healthy, remove_db,
    reopen_store, scalar, scan_ok, seed_library, stamp, strings, Tree, ALL_FORMATS,
    OVERRIDE_ALBUM_PID,
};
use nd_scanner::contract::CancelToken;
use nd_scanner::phase1::{self, Phase1Error, TagFailureKind};

/// Go's default album PID spec (`consts/consts.go:157`), used as the "previous" spec in
/// the transition tests.
const DEFAULT_ALBUM_PID_SPEC: &str = nd_id::DEFAULT_ALBUM_PID;

#[cfg(unix)]
fn mode(value: u32) -> std::fs::Permissions {
    PermissionsExt::from_mode(value)
}

/// `tag_read_failure_policy`: Go's per-file arm (`gotaglib.go:38-40`), executed with two
/// different failure causes, and asserted against the outcome shape.
///
/// `cfg(unix)`: the second cause is a chmod-`000` file, which has no portable equivalent.
/// The first cause (unparsable bytes) and the outcome shape are also covered on every
/// platform by `import_failure_persists_folder_only`.
#[cfg(unix)]
#[test]
fn tag_read_failure_policy() {
    let tree = Tree::new("tag-skip");
    let dir = tree.dir("album/one");
    copy_fixture(&dir, "known.mp3", "known.mp3");
    // Cause 1: bytes that cannot be parsed at all. The name classifies it as audio, so
    // Go's extractor reaches it and `continue`s.
    std::fs::write(dir.join("broken.mp3"), [0u8; 96]).expect("write corrupt file");
    // Cause 2: a file the backend cannot open. `lstat`/`stat` still succeed, so this is
    // an open failure — the same per-file arm, not the batch arm.
    let blocked = dir.join("unreadable.mp3");
    std::fs::write(&blocked, b"readable content").expect("write unreadable file");
    std::fs::set_permissions(&blocked, mode(0o000)).expect("chmod 000");

    let (store, db) = fresh_store("tag-skip", 2);
    seed_library(&store, 1, "tag-skip", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    assert_eq!(outcome.audio_count, 3, "all three files were walked");
    assert_eq!(outcome.tag_failures.len(), 2, "exactly two omissions");
    assert!(
        outcome
            .tag_failures
            .iter()
            .all(|failure| failure.kind == TagFailureKind::PerFileSkipped),
        "both are the extractor's `continue` arm, not the batch arm: {:?}",
        outcome.tag_failures
    );
    let skipped: Vec<&str> = outcome
        .tag_failures
        .iter()
        .map(|failure| failure.path.as_deref().unwrap_or("<none>"))
        .collect();
    assert_eq!(
        skipped,
        vec!["album/one/broken.mp3", "album/one/unreadable.mp3"],
        "each skipped file is named, in walk order"
    );

    // The folder survives: the good file is imported and its fields are real.
    assert_eq!(outcome.new_media_count, 1);
    assert_eq!(children(&store, &outcome.folder_id).len(), 1);
    assert_eq!(
        scalar(&store, "SELECT title FROM media_file"),
        Some("Real café".to_string()),
        "non-vacuity: the untouched fixture still yields its real title"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT num_audio_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        3,
        "the folder row records all three audio files, as Go's entry does"
    );
    assert_eq!(
        outcome.committed.media_ids.len(),
        1,
        "only the parsed file reached the batch"
    );
    assert_eq!(
        outcome.album_ids.len(),
        1,
        "one album draft, from one track"
    );
    assert!(pool_healthy(&store));
    std::fs::set_permissions(&blocked, mode(0o644)).expect("restore permissions");
    remove_db(&db);
}

/// `import_failure_persists_folder_only`: Go's `return entry, nil` arm (`:274-279`) —
/// import is skipped, yet `persistChanges` still runs on the entry, so the folder row
/// is written with its counts and hash and NOTHING track-shaped exists afterwards.
/// Executed with a folder whose every audio file fails to parse, which drives the same
/// empty-import state without depending on a stat race.
#[test]
fn import_failure_persists_folder_only() {
    let tree = Tree::new("tag-abort");
    let dir = tree.dir("album/one");
    std::fs::write(dir.join("broken-a.mp3"), [0u8; 64]).expect("write first corrupt file");
    std::fs::write(dir.join("broken-b.mp3"), [7u8; 64]).expect("write second corrupt file");

    let (store, db) = fresh_store("tag-abort", 2);
    seed_library(&store, 1, "tag-abort", &tree.root);
    let mapping = mapping();

    let before = common::writable_rows(&store);
    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    // Per writable table: the ONLY table allowed to gain a row is `folder`. A summed count
    // would let a stray junction, queue, or FTS row cancel out against something else.
    let after = common::writable_rows(&store);
    for ((table, after_count), (before_table, before_count)) in after.iter().zip(before.iter()) {
        assert_eq!(table, before_table, "snapshot table order drifted");
        if *table == "folder" {
            assert_eq!(
                *after_count,
                *before_count + 1,
                "exactly one folder row is written by the empty-import arm"
            );
        } else {
            assert_eq!(
                *after_count, *before_count,
                "{table}: the empty-import arm wrote nothing track-shaped, per table"
            );
        }
    }

    assert_eq!(outcome.audio_count, 2, "both files were still walked");
    assert_eq!(
        outcome.tag_failures.len(),
        2,
        "both were reported, not swallowed"
    );

    // The folder row is written — Go's stage returns `entry, nil`, so the persist stage
    // still runs on it.
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        1,
        "the folder row is written even though nothing was imported"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT num_audio_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        2,
        "its counts come from the walk, not from the import"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(outcome.hash.clone()),
        "its hash is the walk's fingerprint"
    );

    // Nothing track-shaped, in any leg.
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        0,
        "no tracks"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 0, "no albums");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artist"),
        0,
        "no artists"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM tag"),
        0,
        "no tag dictionary rows"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM library_artist"),
        0,
        "no library links"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        0,
        "no queue items"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file_fts"),
        0,
        "no FTS rows"
    );
    assert_eq!(outcome.album_ids, Vec::<String>::new());
    assert_eq!(outcome.tag_count, 0);
    assert_eq!(outcome.queued_item_count, 0);
    assert_eq!(outcome.new_media_count, 0);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `db_read_fatal_stops_only_this_folder`: a stored child the decoder cannot read makes
/// the children visit fail (`:240-243`). Go's stage error means `persistChanges` never
/// runs for that folder — and `stopWalk` is NOT called, so a sibling folder still scans.
#[test]
fn db_read_fatal_stops_only_this_folder() {
    let tree = Tree::new("db-fatal");
    tree.put("album/bad", "known.mp3");
    tree.put("album/good", "known.flac");
    let (store, db) = fresh_store("db-fatal", 2);
    seed_library(&store, 1, "db-fatal", &tree.root);
    let mapping = mapping();

    // The bad folder's own row plus one child whose `participants` cell is undecodable —
    // the same poison nd-db's runtime cursor tests use (`seed_media_broken`).
    let bad_folder = nd_db::folder_repo::folder_id(1, "", "album/bad");
    execute(
        &store,
        "INSERT INTO folder (id, library_id, path, name, parent_id, missing, hash, num_audio_files, num_playlists, images_updated_at, updated_at, created_at, image_files) \
         VALUES (?1, 1, 'album', 'bad', '', 0, 'pre-existing-hash', 0, 0, ?2, ?3, ?3, '[]')",
        &[
            nd_db::sqlrepo::Value::Text(bad_folder.clone()),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
        ],
    );
    execute(
        &store,
        "INSERT INTO media_file (id, path, library_id, folder_id, title, participants, tags, missing, created_at, updated_at, birth_time) \
         VALUES ('poisoned-child', 'album/bad/known.mp3', 1, ?1, 'Poisoned', '{\"bogus\":[{\"id\":\"a\",\"name\":\"n\"}]}', '{}', 0, ?2, ?2, ?2)",
        &[
            nd_db::sqlrepo::Value::Text(bad_folder.clone()),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
        ],
    );

    let token = CancelToken::new();
    // The writable-table snapshot is taken AFTER the seeding above, so the no-write claim below
    // is about the failing scan only, and it is per table: a helper that wrote a junction row
    // and deleted a folder row would still net to zero in a summed count.
    let before = common::writable_rows(&store);
    assert!(
        before.iter().any(|(_, count)| *count > 0),
        "the seeded folder and child rows are in the snapshot, so the equality below is not \
         comparing an empty database with itself: {before:?}"
    );
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/bad", stamp(), &mapping),
        &token,
    )
    .expect_err("the poisoned child must fail the read");
    match error {
        Phase1Error::ChildrenRead { folder, source } => {
            assert_eq!(folder, "album/bad");
            assert!(
                source.to_string().contains("participants"),
                "the decode failure names the offending column, got: {source}"
            );
        }
        other => panic!("expected ChildrenRead, got {other}"),
    }

    // Nothing was written for that folder, per writable table, so no FTS ghost or junction
    // row and no deleted `item_artwork` row can hide inside a summed count.
    common::assert_no_writes(&store, &before, "children-read abort");
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{bad_folder}'")
        ),
        Some("pre-existing-hash".to_string()),
        "no persist ran for the failed folder"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT updated_at FROM folder WHERE id = '{bad_folder}'")
        ),
        Some(nd_db::time::to_text(&stamp())),
        "unchanged"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album WHERE library_id = 1"),
        0,
        "no albums from the aborted folder"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 0);
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE id = 'poisoned-child'"
        ),
        Some(nd_db::time::to_text(&stamp())),
        "the poisoned child was not flagged missing or touched"
    );

    // The pool is un-poisoned and a sibling folder scans normally.
    assert!(
        pool_healthy(&store),
        "the failed read must not poison the pool"
    );
    let good = scan_ok(&store, &job(1, &tree.root, "album/good", stamp(), &mapping));
    assert_eq!(children(&store, &good.folder_id).len(), 1);
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM media_file WHERE folder_id = '{}'",
                good.folder_id
            )
        ),
        1,
        "the sibling folder's child is its own row"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album"),
        1,
        "the sibling wrote its album"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        3,
        "the sibling's queue leg ran"
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// `persist_fatal_propagates_no_outcome`: a fatal arm inside the folder transaction.
/// With no `library` row the folder Put's FK reference fails, `persist_folder_delta`
/// rolls the whole transaction back, and Go's `stopWalk(err)` arm means the caller gets
/// the error with **no** outcome — so no warning can have been published pre-commit
/// (`D-BATCH`).
#[test]
fn persist_fatal_propagates_no_outcome() {
    let tree = Tree::new("persist-fatal");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("persist-fatal", 2);
    // Deliberately NO library row: every write leg references library(id).
    let mapping = mapping();

    let token = CancelToken::new();
    let before = common::writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/one", stamp(), &mapping),
        &token,
    )
    .expect_err("the FK violation must surface");
    match error {
        Phase1Error::Persist { folder, source } => {
            assert_eq!(folder, "album/one");
            assert!(
                source.to_string().to_lowercase().contains("foreign key"),
                "the driver error is retained, got: {source}"
            );
        }
        other => panic!("expected Persist, got {other}"),
    }

    // Zero committed rows for the folder, in every leg — asserted per writable table rather
    // than one table at a time, so no leg can leak a row outside the listed handful.
    common::assert_no_writes(&store, &before, "persist-fatal rollback");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM folder"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM tag"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 0);
    assert!(
        pool_healthy(&store),
        "a rolled-back transaction must not poison the pool"
    );
    assert_eq!(
        store
            .idle_count()
            .expect("pool still reports its idle count"),
        2,
        "the lease was returned after the rollback"
    );
    remove_db(&db);
}

/// `warn_only_outcome_still_commits`: Go's warn-only arms (`ReassignAnnotation`,
/// `:488-493`) log + `sendWarning` and continue, so the transaction still commits and the
/// outcome carries the warning. Deterministically forced with a `RAISE(ABORT)` trigger on
/// `annotation`: SQLite aborts only that statement (verified against the driver — a later
/// statement in the same transaction still succeeds), which is exactly the arm's shape.
#[test]
fn warn_only_outcome_still_commits() {
    let tree = Tree::new("warn-only");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("warn-only", 2);
    seed_library(&store, 1, "warn-only", &tree.root);
    let mapping = mapping();
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());

    let first = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let album_a = first.album_ids[0].clone();

    // Switch the album PID spec: a new album id, and a transition A->B consumed cleanly.
    let second = scan_ok(
        &store,
        &job_with_album_pid(
            1,
            &tree.root,
            "album/one",
            later_stamp(),
            &mapping,
            OVERRIDE_ALBUM_PID,
            Some(DEFAULT_ALBUM_PID_SPEC),
        ),
    );
    let album_b = second.album_ids[0].clone();
    assert_ne!(album_a, album_b);
    assert_eq!(second.committed.consumed_transitions.len(), 1);
    assert!(second.committed.warnings.is_empty(), "first move is clean");

    let user = "u-warn";
    execute(
        &store,
        "INSERT INTO user (id, user_name, name, email, password, is_admin, created_at, updated_at) \
         VALUES (?1, ?1, 'U', 'u@example.invalid', 'p', 1, '2024-01-01 00:00:00', '2024-01-01 00:00:00')",
        &[text(user)],
    );
    execute(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, play_count, rating) \
         VALUES (?1, ?2, 'album', 3, 4)",
        &[text(user), text(&album_b)],
    );

    // Inject a statement-level failure on the annotation UPDATE only.
    execute(
        &store,
        "CREATE TRIGGER sc05_inject BEFORE UPDATE ON annotation BEGIN \
         SELECT RAISE(ABORT, 'injected annotation failure'); END",
        &[],
    );

    // Switch back to the default spec: derived id is A again, stored child is B, so the
    // transition B->A runs the warn-only reassignment against the injected failure.
    // Third pass: a stamp strictly later than BOTH earlier passes (`stamp` < `later_stamp`
    // < `laterest_stamp`), so "this scan's stamp" below cannot be satisfied by the value
    // the first pass wrote.
    let third = scan_ok(
        &store,
        &job_with_album_pid(
            1,
            &tree.root,
            "album/one",
            laterest_stamp(),
            &mapping,
            DEFAULT_ALBUM_PID_SPEC,
            Some(OVERRIDE_ALBUM_PID),
        ),
    );

    assert_eq!(third.album_ids, vec![album_a.clone()]);
    assert_eq!(
        third.committed.consumed_transitions,
        vec![nd_db::scanner_writes::AlbumTransition {
            new_id: album_a.clone(),
            prev_id: album_b.clone(),
        }],
        "the mapping is consumed even though the annotation move failed (:504)"
    );
    let warnings = &third.committed.warnings;
    assert_eq!(
        warnings.len(),
        1,
        "one warn-only arm degraded, got {:?}",
        warnings.iter().map(|w| w.stage_str()).collect::<Vec<_>>()
    );
    assert_eq!(warnings[0].stage_str(), "reassign_annotation");
    assert_eq!(warnings[0].item_id, album_a);
    assert_eq!(warnings[0].related_id, album_b);
    assert!(
        warnings[0]
            .source
            .to_string()
            .contains("injected annotation failure"),
        "the driver's message is retained, got {}",
        warnings[0].source
    );
    assert_eq!(
        third.folder_id, first.folder_id,
        "an outcome is delivered, not an error"
    );

    // Everything else committed despite the warn-only failure.
    assert_eq!(
        scalar(&store, "SELECT album_id FROM media_file"),
        Some(album_a.clone()),
        "the MediaFile Put ran after the failed arm"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 2);
    assert_eq!(
        scalar(
            &store,
            &format!(
                "SELECT updated_at FROM folder WHERE id = '{}'",
                third.folder_id
            )
        ),
        Some(nd_db::time::to_text(&laterest_stamp())),
        "the folder row carries this scan's stamp, which differs from both earlier passes"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT created_at FROM album WHERE id = '{}'", album_a)
        ),
        scalar(
            &store,
            &format!("SELECT created_at FROM album WHERE id = '{}'", album_b)
        ),
        "the warn-only arm did not stop CopyAttributes from running"
    );
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM annotation WHERE item_id = '{album_b}'")
        ),
        1,
        "the failed move left the annotation on the previous album"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        4,
        "queue leg ran: 2 artists + both albums across the three scans"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album_fts"),
        2,
        "both albums indexed"
    );
    assert!(pool_healthy(&store));
    execute(&store, "DROP TRIGGER sc05_inject", &[]);
    remove_db(&db);
}

/// The other warn-only arm, on the queue leg (`:466-471`: "Never fails the scan"): an
/// injected statement failure on `artwork_queue` degrades only that arm.
#[test]
fn warn_only_enqueue_arm_still_commits() {
    let tree = Tree::new("warn-enqueue");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("warn-enqueue", 2);
    seed_library(&store, 1, "warn-enqueue", &tree.root);
    let mapping = mapping();

    execute(
        &store,
        "CREATE TRIGGER sc05_inject_queue BEFORE INSERT ON artwork_queue BEGIN \
         SELECT RAISE(ABORT, 'injected queue failure'); END",
        &[],
    );

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    let warnings = &outcome.committed.warnings;
    assert_eq!(
        warnings.len(),
        1,
        "only the queue arm degraded, got {:?}",
        warnings.iter().map(|w| w.stage_str()).collect::<Vec<_>>()
    );
    assert_eq!(warnings[0].stage_str(), "enqueue");
    assert_eq!(
        warnings[0].item_id, outcome.folder_id,
        "Go logs this arm folder-wide"
    );
    assert!(warnings[0]
        .source
        .to_string()
        .contains("injected queue failure"));
    assert_eq!(
        outcome.queued_item_count, 3,
        "the items were still handed to the queue"
    );

    // The rest of the vertical committed.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM tag"), 3);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        0,
        "queue empty"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 1);
    assert!(pool_healthy(&store));
    execute(&store, "DROP TRIGGER sc05_inject_queue", &[]);
    remove_db(&db);
}

/// `cancellation_during_walk_stops_cleanly`: cancellation is cooperative and aborts
/// before any write, at each checkpoint the port checks — before the DB read (Go
/// `:226-228`), inside the children visit, and between files — and the worker thread
/// always returns.
#[test]
fn cancellation_during_walk_stops_cleanly() {
    let tree = Tree::new("cancel");
    // Enough files that a concurrent cancel lands between tag reads rather than before
    // the scan starts.
    let dir = tree.dir("album/big");
    for index in 0..600 {
        copy_fixture(&dir, "known.mp3", &format!("track-{index:03}.mp3"));
    }
    let (store, db) = fresh_store("cancel", 2);
    seed_library(&store, 1, "cancel", &tree.root);
    let mapping = mapping();
    let root = tree.root.clone();

    // (a) Cancelled before the DB read: Go's `context.Cause` arm at `:226-228`.
    let token = CancelToken::new();
    token.cancel();
    let before_cancel = common::writable_rows(&store);
    assert!(
        before_cancel.iter().all(|(_, count)| *count == 0),
        "the 600-file folder is not yet in the DB: {before_cancel:?}"
    );
    let error = phase1::scan_folder(
        &store,
        &job(1, &root, "album/big", stamp(), &mapping),
        &token,
    )
    .expect_err("a cancelled scan must not proceed");
    assert!(
        matches!(error, Phase1Error::Cancelled { .. }),
        "expected Cancelled, got {error}"
    );
    common::assert_no_writes(&store, &before_cancel, "cancel before the DB read");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        0,
        "nothing written"
    );
    assert_eq!(
        store.idle_count().expect("pool idle count"),
        2,
        "no lease held"
    );

    // (b) Token tripped while the children visit runs: the DB-11 flag stops the visit and
    // the adapter reports it as a cancellation, not a partial success.
    let big_folder = nd_db::folder_repo::folder_id(1, "", "album/big");
    execute(
        &store,
        "INSERT INTO folder (id, library_id, path, name, parent_id, missing, hash, num_audio_files, num_playlists, images_updated_at, updated_at, created_at, image_files) \
         VALUES (?1, 1, 'album', 'big', '', 0, 'old-hash', 0, 0, ?2, ?3, ?3, '[]')",
        &[
            nd_db::sqlrepo::Value::Text(big_folder.clone()),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
        ],
    );
    execute(
        &store,
        "INSERT INTO media_file (id, path, library_id, folder_id, title, participants, tags, missing, created_at, updated_at, birth_time) \
         VALUES ('existing-child', 'album/big/track-000.mp3', 1, ?1, 'Existing', '{}', '{}', 0, ?2, ?2, ?2)",
        &[
            nd_db::sqlrepo::Value::Text(big_folder.clone()),
            nd_db::sqlrepo::Value::Text(nd_db::time::to_text(&stamp())),
        ],
    );
    let token = CancelToken::new();
    token.cancel();
    let before_visit = common::writable_rows(&store);
    assert!(
        before_visit.iter().any(|(_, count)| *count > 0),
        "the seeded folder + child are in the snapshot, so the equality below is not \
         comparing an empty DB with itself: {before_visit:?}"
    );
    let error = phase1::scan_folder(
        &store,
        &job(1, &root, "album/big", later_stamp(), &mapping),
        &token,
    )
    .expect_err("the pre-tripped token must abort before the read completes");
    assert!(
        matches!(error, Phase1Error::Cancelled { .. }),
        "expected Cancelled, got {error}"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE id = 'existing-child'"
        ),
        Some(nd_db::time::to_text(&stamp())),
        "no missing-marking or Put ran"
    );
    common::assert_no_writes(&store, &before_visit, "cancel during the children visit");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 0);
    assert!(pool_healthy(&store));

    // (c) Cancelled mid-ingest by another thread: a typed cancellation, no writes, and
    // the worker joins (no hung task). 600 real tag reads take far longer than the delay.
    let before_thread = common::writable_rows(&store);
    let worker_store = store.clone();
    let worker_mapping = mapping.clone();
    let handle = std::thread::Builder::new()
        .name("sc05-cancel-probe".to_string())
        .spawn(move || {
            let token = CancelToken::new();
            let late = token.clone();
            let canceller = std::thread::Builder::new()
                .name("sc05-cancel-trigger".to_string())
                .spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    late.cancel();
                })
                .expect("spawn canceller");
            let job = job(1, &root, "album/big", stamp(), &worker_mapping);
            let result = phase1::scan_folder(&worker_store, &job, &token);
            canceller.join().expect("the canceller thread returns");
            result
        })
        .expect("spawn scanner thread");
    let result = handle
        .join()
        .unwrap_or_else(|_| panic!("the scanner thread must return, not hang or panic"));
    match result {
        Err(Phase1Error::Cancelled { folder }) => assert_eq!(folder, "album/big"),
        Err(other) => panic!("expected Cancelled, got {other}"),
        Ok(outcome) => panic!(
            "the cancel landed after the ingest finished (audio_count = {}): the tag loop \
             must outlast the 20ms delay for this checkpoint to be exercised",
            outcome.audio_count
        ),
    }
    common::assert_no_writes(&store, &before_thread, "cancel mid-ingest");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        1,
        "only the pre-seeded child row exists; the ingest wrote nothing"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 0);
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{big_folder}'")
        ),
        Some("old-hash".to_string()),
        "the folder row is untouched"
    );
    assert!(pool_healthy(&store));
    assert_eq!(
        store
            .idle_count()
            .expect("pool still usable after the join"),
        2,
        "the cancelled scan returned its lease"
    );
    remove_db(&db);
}

/// `multi_track_folder_is_sc04`: two tracks that hash to the same album id are a
/// multi-track group. SC-04 owns `MediaFiles.ToAlbum`, so the projection refuses rather
/// than writing a singleton draft with `SongCount = 1` and one track's duration.
#[test]
fn multi_track_folder_is_sc04() {
    let tree = Tree::new("multi-track");
    let dir = tree.dir("album/one");
    // mp3 and ogg share the default-spec album id (neither carries `releasedate`).
    copy_fixture(&dir, "known.mp3", "known.mp3");
    copy_fixture(&dir, "known.ogg", "known.ogg");
    let (store, db) = fresh_store("multi-track", 2);
    seed_library(&store, 1, "multi-track", &tree.root);
    let mapping = mapping();

    let token = CancelToken::new();
    let before = common::writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/one", stamp(), &mapping),
        &token,
    )
    .expect_err("SC-04 aggregation is not implemented here");
    match error {
        Phase1Error::Projection { folder, source } => {
            assert_eq!(folder, "album/one");
            assert!(
                matches!(
                    source,
                    nd_scanner::projection::ProjectionError::MultiTrackGroup { tracks: 2, .. }
                ),
                "expected MultiTrackGroup with 2 tracks, got {source}"
            );
            assert!(
                source.to_string().contains("SC-04"),
                "the message names the owning package, got {source}"
            );
        }
        other => panic!("expected Projection, got {other}"),
    }

    // Refused, not half-written: no folder row, no tracks, no album draft — per writable table,
    // so a projection refusal that wrote some legs and refused later cannot pass.
    common::assert_no_writes(&store, &before, "projection refusal (multi-track)");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM folder"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 0);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// Non-vacuity of the corrupt-file path: a single flipped byte inside the fixture's tag
/// block changes what the vertical stores, so a green "corrupt file is skipped" assertion
/// cannot be an accident of an unreadable path or a stubbed reader.
#[test]
fn corrupted_fixture_byte_changes_the_stored_fields() {
    let clean = Tree::new("corrupt-clean");
    clean.put("album/one", "known.mp3");
    let dirty = Tree::new("corrupt-dirty");
    let dir = dirty.dir("album/one");
    let path = copy_fixture(&dir, "known.mp3", "known.mp3");

    let (store_a, db_a) = fresh_store("corrupt-clean", 2);
    seed_library(&store_a, 1, "corrupt-clean", &clean.root);
    let mapping = mapping();
    let clean_outcome = scan_ok(
        &store_a,
        &job(1, &clean.root, "album/one", stamp(), &mapping),
    );
    let clean_title = scalar(&store_a, "SELECT title FROM media_file");
    assert_eq!(clean_title, Some("Real café".to_string()));
    let clean_album = clean_outcome.album_ids[0].clone();

    // Flip a byte inside the TIT2 *value* (offset 21 is the `R` of "Real café"), so the
    // parsed title changes rather than the frame header being mangled.
    let mut bytes = std::fs::read(&path).expect("read fixture");
    assert!(bytes.len() > 64, "fixture is large enough to edit");
    assert_eq!(
        bytes[21], b'R',
        "the fixture's title frame is where we expect"
    );
    bytes[21] = b'X';
    std::fs::write(&path, bytes).expect("write back the edited fixture");

    let (store_b, db_b) = fresh_store("corrupt-dirty", 2);
    seed_library(&store_b, 1, "corrupt-dirty", &dirty.root);
    let dirty_outcome = scan_ok(
        &store_b,
        &job(1, &dirty.root, "album/one", stamp(), &mapping),
    );

    // Either the parse failed (typed skip) or the stored fields differ. Both prove the
    // tag path really reads these bytes.
    let dirty_title = scalar(&store_b, "SELECT title FROM media_file");
    assert!(
        !dirty_outcome.tag_failures.is_empty() || dirty_title != clean_title,
        "the edited fixture changed nothing: title {dirty_title:?}, failures {:?}",
        dirty_outcome.tag_failures
    );
    if dirty_outcome.tag_failures.is_empty() {
        assert_ne!(
            dirty_title, clean_title,
            "the edited title byte must change the parsed title"
        );
        assert_eq!(
            dirty_outcome.album_ids[0], clean_album,
            "the album spec keys on album/albumartistid/releasedate, so a title change \
             must NOT move the album id"
        );
    } else {
        assert_eq!(count(&store_b, "SELECT COUNT(*) FROM media_file"), 0);
    }
    assert_eq!(clean_outcome.audio_count, 1);
    assert_eq!(dirty_outcome.audio_count, 1);
    assert!(pool_healthy(&store_a));
    assert!(pool_healthy(&store_b));
    remove_db(&db_a);
    remove_db(&db_b);
}

/// The fixture set is the committed one and the scratch lives outside the repo: every
/// file the tests copy exists under the manifest, and both trees and DBs live in `$TMPDIR`.
/// Paths are compared CANONICALIZED — `fixture_source` resolves through `..`, so a raw
/// `starts_with` against the raw manifest text only looks like an identity check.
#[test]
fn fixtures_come_from_the_committed_tree() {
    let fixtures = common::fixture_real_dir();
    for fixture in ALL_FORMATS {
        let source = fixture_source(fixture);
        assert!(
            source.exists(),
            "committed fixture {fixture} missing at {}",
            source.display()
        );
        assert!(
            source.starts_with(&fixtures),
            "fixture must resolve inside the canonicalized committed tree {fixtures:?}, got {}",
            source.display()
        );
        assert!(
            !source.components().any(|c| c.as_os_str() == ".."),
            "a canonical fixture path carries no `..` component: {}",
            source.display()
        );
    }
    let tree = Tree::new("tmp-check");
    // `$TMPDIR` itself can be a symlink (`/var` → `/private/var` on macOS), so the
    // comparison is against the canonicalized temp dir, matching canonicalized Tree::new.
    assert!(tree
        .root
        .starts_with(std::fs::canonicalize(std::env::temp_dir()).expect("temp dir")));
    let (store, db) = fresh_store("tmp-check", 1);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        0,
        "fresh schema"
    );
    assert!(strings(&store, "SELECT id FROM folder WHERE 0").is_empty());
    assert!(db.starts_with(std::env::temp_dir()));
    remove_db(&db);
}

/// Every table the vertical writes, plus the three FTS shadow tables a rolled-back
/// statement must not leave a ghost row in. This is `common::WRITABLE_TABLES` re-exported, not
/// a second copy: the two suites' snapshots cannot drift in membership or order, so a table
/// added on the `common` side (e.g. `annotation`, which the warn-only ReassignAnnotation leg
/// writes) is in the rollback snapshot too. `item_artwork` is included because the artwork legs
/// DELETE rows there (`nd-db`'s committed outcome counts `deleted_artwork`:
/// `scanner_writes.rs:585`, `artwork_queue_repo.rs`), so a rollback that left art deleted —
/// or a no-write path that deleted it — must show up in this snapshot.
const WRITABLE_TABLES: &[&str] = common::WRITABLE_TABLES;

/// `folder_transaction_rolls_back_after_each_write_leg`: one injected statement failure per
/// FATAL leg of Go's persistence order (`phase_1_folders.go:374-460`), each on its own fresh
/// DB, driven through the real `scan_folder` entry. The `insert_legs` matrix below covers only
/// the INSERT legs, in Go's order: Folder Put → Tag Add → Artist Put → Library AddArtist →
/// Album Put → MediaFile Put (its participant/tag DELETE legs are injected on a re-import).
/// MarkMissing (`:443-452`) and Album Touch (`:454-459`) are UPDATE legs that only run when a
/// stored child disappeared from disk, so they are injected on a SEPARATE pass of this test
/// (`update_legs`, below) against a pre-seeded folder — they are not part of this matrix.
///
/// Each leg asserts the same three things: the caller gets `Phase1Error::Persist` with no
/// outcome (Go's `stopWalk(err)`), the state is byte-identical to the pre-scan snapshot — so
/// nothing earlier in the same transaction survived — and the equality still holds after
/// reopening with a FRESH pool, i.e. the rollback is durable rather than lease-local. The
/// `*_fts` tables are in the snapshot, so a shadow-row leak fails, and `annotation` is too, so
/// a warn-only ReassignAnnotation leg that survived the rollback fails. On the `update_legs`
/// pass the earlier legs are proven not to survive by the state columns [`state_snapshot`] pins:
/// `folder.hash` (a committed Folder Put leg would rewrite it), `media_file.missing` (the
/// injected MarkMissing target), and `album.updated_at` (the injected Album Touch target).
/// That pass does not exercise the INSERT legs — no new rows exist to insert on that seed — so
/// insert-leg rollback is covered only by the `insert_legs` matrix above.
#[test]
fn folder_transaction_rolls_back_after_each_write_leg() {
    // `RAISE(ABORT)` fails only the injected statement; the transaction still rolls back
    // because `persist_folder_delta` sees its closure return `Err`.
    let insert_legs: &[(&str, &str)] = &[
        (
            "folder_put",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON folder BEGIN SELECT RAISE(ABORT, 'injected folder_put failure'); END",
        ),
        (
            "tag_add",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON tag BEGIN SELECT RAISE(ABORT, 'injected tag_add failure'); END",
        ),
        (
            "artist_put",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON artist BEGIN SELECT RAISE(ABORT, 'injected artist_put failure'); END",
        ),
        (
            "add_artist",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON library_artist BEGIN SELECT RAISE(ABORT, 'injected add_artist failure'); END",
        ),
        (
            "album_put",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON album BEGIN SELECT RAISE(ABORT, 'injected album_put failure'); END",
        ),
        (
            "media_file_put",
            "CREATE TRIGGER sc05_inject BEFORE INSERT ON media_file BEGIN SELECT RAISE(ABORT, 'injected media_file_put failure'); END",
        ),
    ];
    for (leg, trigger) in insert_legs {
        let tree = Tree::new("rollback-insert");
        tree.put("album/one", "known.mp3");
        let (store, db) = fresh_store("rollback-insert", 2);
        seed_library(&store, 1, "rollback-insert", &tree.root);
        let before = state_snapshot(&store);
        assert_eq!(
            WRITABLE_TABLES
                .iter()
                .map(|t| count(&store, &format!("SELECT COUNT(*) FROM {t}")))
                .sum::<i64>(),
            0,
            "{leg}: the seeded library is the only row, so every writable table starts empty"
        );
        execute(&store, trigger, &[]);

        let error = scan_error(&store, &tree, "album/one", stamp());
        assert_persist_failure(&error, leg, "album/one");
        assert_eq!(state_snapshot(&store), before, "{leg}: nothing survived");
        assert_eq!(
            store.idle_count().expect("idle count"),
            2,
            "{leg}: lease returned"
        );
        assert!(pool_healthy(&store), "{leg}: pool not poisoned");
        drop(store);
        let reopened = reopen_store(&db, 1);
        assert_eq!(
            state_snapshot(&reopened),
            before,
            "{leg}: the rollback is durable across a fresh pool"
        );
        remove_db(&db);
    }

    // The participant/tag DELETE legs run inside the MediaFile Put of a RE-IMPORT
    // (`updateParticipants`/`updateTags` always delete first). The injection is
    // installed after a committed first pass, so the surviving-state assertion is the
    // interesting one: a rolled-back second transaction cannot disturb the first.
    let delete_legs: &[(&str, &str)] = &[
        (
            "participant_delete",
            "CREATE TRIGGER sc05_inject BEFORE DELETE ON media_file_artists BEGIN SELECT RAISE(ABORT, 'injected participant_delete failure'); END",
        ),
        (
            "tag_delete",
            "CREATE TRIGGER sc05_inject BEFORE DELETE ON media_file_tags BEGIN SELECT RAISE(ABORT, 'injected tag_delete failure'); END",
        ),
    ];
    for (leg, trigger) in delete_legs {
        let tree = Tree::new("rollback-delete");
        tree.put("album/one", "known.mp3");
        let (store, db) = fresh_store("rollback-delete", 2);
        seed_library(&store, 1, "rollback-delete", &tree.root);
        let mapping = mapping();
        let first = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
        // A committed `item_artwork` row for the re-imported track. The artwork invalidation
        // leg (`scanner_writes.rs:776-789`, Go `:436-441`) DELETEs exactly these rows, so with a
        // row present the snapshot below can actually observe an art deletion: a rolled-back
        // second transaction that left it deleted — or one that deleted it in a transaction that
        // then committed the earlier legs — changes the count and fails the equality.
        let text_value = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
        execute(
            &store,
            "INSERT INTO item_artwork (item_kind, item_id, image_type, hash, source) \
             VALUES ('mf', ?1, 'primary', 'seeded-hash', 'cover')",
            &[text_value(&first.committed.media_ids[0])],
        );
        let committed = state_snapshot(&store);
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM item_artwork"),
            1,
            "{leg}: the artwork row is in the snapshot, so the delete leg has work to lose"
        );
        execute(&store, trigger, &[]);

        let error = scan_error(&store, &tree, "album/one", later_stamp());
        assert_persist_failure(&error, leg, "album/one");
        assert_eq!(
            state_snapshot(&store),
            committed,
            "{leg}: the committed first pass is untouched by the rolled-back second"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM item_artwork"),
            1,
            "{leg}: the artwork delete leg's row survives — art was not deleted-and-not-replaced"
        );
        assert_eq!(
            scalar(&store, "SELECT title FROM media_file"),
            Some("Real café".to_string()),
            "{leg}: no half-replaced row"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM media_file_artists"),
            2,
            "{leg}: junction rows survived intact, not deleted-and-not-replaced"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM media_file_fts"),
            1,
            "{leg}: no FTS ghost from the aborted update"
        );
        assert!(pool_healthy(&store));
        drop(store);
        let reopened = reopen_store(&db, 1);
        assert_eq!(
            state_snapshot(&reopened),
            committed,
            "{leg}: durable across a fresh pool"
        );
        assert_eq!(reopened.idle_count().expect("idle"), 1);
        remove_db(&db);
        let _ = first.folder_id;
    }

    // The UPDATE legs (MarkMissing `:443-452`, Album Touch `:454-459`) only run when a
    // stored child disappeared from disk, so the folder is pre-seeded with a child
    // pointing at an absent path.
    let update_legs: &[(&str, &str)] = &[
        (
            "mark_missing",
            "CREATE TRIGGER sc05_inject BEFORE UPDATE ON media_file BEGIN SELECT RAISE(ABORT, 'injected mark_missing failure'); END",
        ),
        (
            "album_touch",
            "CREATE TRIGGER sc05_inject BEFORE UPDATE ON album BEGIN SELECT RAISE(ABORT, 'injected album_touch failure'); END",
        ),
    ];
    for (leg, trigger) in update_legs {
        let tree = Tree::new("rollback-update");
        let (store, db) = fresh_store("rollback-update", 2);
        seed_library(&store, 1, "rollback-update", &tree.root);
        seed_stored_child_that_vanished(&store, &tree);
        let before = state_snapshot(&store);
        execute(&store, trigger, &[]);

        let error = scan_error(&store, &tree, "album/gone", later_stamp());
        assert_persist_failure(&error, leg, "album/gone");
        assert_eq!(
            state_snapshot(&store),
            before,
            "{leg}: the child is still nonmissing, the album still un-touched, and the \
             folder leg that ran earlier in the same transaction rolled back with it"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM artwork_queue"),
            0,
            "{leg}: the queue leg never reached a committed transaction"
        );
        assert!(pool_healthy(&store), "{leg}: pool not poisoned");
        drop(store);
        let reopened = reopen_store(&db, 1);
        assert_eq!(
            state_snapshot(&reopened),
            before,
            "{leg}: durable across a fresh pool"
        );
        remove_db(&db);
    }
}

/// Seed the `missingTracks` state: a folder row whose one stored child points at a path
/// absent from disk, with an album so Album Touch (`:454-459`) has a target.
fn seed_stored_child_that_vanished(store: &nd_db::store::SQLStore, tree: &Tree) {
    let gone = tree.dir("album/gone");
    assert!(
        gone.is_dir(),
        "the folder exists on disk with no files in it"
    );
    let folder_id = nd_db::folder_repo::folder_id(1, "", "album/gone");
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    let at = nd_db::time::to_text(&stamp());
    execute(
        store,
        "INSERT INTO folder (id, library_id, path, name, parent_id, missing, hash, \
         num_audio_files, num_playlists, images_updated_at, updated_at, created_at, image_files) \
         VALUES (?1, 1, 'album', 'gone', '', 0, 'pre-hash', 1, 0, ?2, ?2, ?2, '[]')",
        &[text(&folder_id), text(&at)],
    );
    execute(
        store,
        "INSERT INTO media_file (id, path, library_id, folder_id, title, participants, \
         tags, missing, created_at, updated_at, birth_time, album_id) \
         VALUES ('stored-child', 'album/gone/gone.mp3', 1, ?1, 'Stored', '{}', '{}', 0, ?2, ?2, ?2, 'stored-album')",
        &[text(&folder_id), text(&at)],
    );
    execute(
        store,
        "INSERT INTO album (id, name, song_count, updated_at) \
         VALUES ('stored-album', 'Stored Album', 1, ?1)",
        &[text(&at)],
    );
}

/// Run `scan_folder` with the default job and require it to fail.
fn scan_error(
    store: &nd_db::store::SQLStore,
    tree: &Tree,
    folder: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Phase1Error {
    let mapping = mapping();
    phase1::scan_folder(
        store,
        &job(1, &tree.root, folder, at, &mapping),
        &CancelToken::new(),
    )
    .expect_err("the injected leg must fail the transaction")
}

fn assert_persist_failure(error: &Phase1Error, leg: &str, folder: &str) {
    match error {
        Phase1Error::Persist {
            folder: got,
            source,
        } => {
            assert_eq!(got, folder, "{leg}: the error names the folder");
            assert!(
                source.to_string().contains("injected"),
                "{leg}: the driver's message is retained, got {source}"
            );
        }
        other => panic!("{leg}: expected Persist, got {other}"),
    }
}

/// The rows the vertical may touch, as one comparable string: counts of every writable
/// table plus the state the UPDATE legs and the warn-only annotation leg would change. Any FTS
/// ghost, partial junction row, or annotation reassign changes it.
///
/// `album.average_rating` and the `annotation` rows are pinned deliberately, as VALUES rather
/// than counts. `persist_folder_delta` runs ReassignAnnotation on an album id transition
/// (`nd-db/src/scanner_writes.rs:705-716`), which is an UPDATE-only leak: `update or ignore
/// annotation set item_id = …` (`nd-db/src/sql_annotations.rs:393-398`) moves rows between ids
/// without changing `COUNT(*) FROM annotation`, and the follow-up `UPDATE album SET
/// average_rating = …` (`:330-334`) changes a column and no count. A count-only snapshot would
/// pass both, so the moved-row identity set and the cached rating are compared as text.
#[must_use]
fn state_snapshot(store: &nd_db::store::SQLStore) -> String {
    let counts: Vec<String> = WRITABLE_TABLES
        .iter()
        .map(|table| {
            format!(
                "{table}={}",
                count(store, &format!("SELECT COUNT(*) FROM {table}"))
            )
        })
        .collect();
    let child_missing = strings(
        store,
        "SELECT IFNULL(CAST(missing AS TEXT), '<none>') FROM media_file ORDER BY id LIMIT 1",
    )
    .join(",");
    let folder_hash = strings(
        store,
        "SELECT IFNULL(hash, '<none>') FROM folder ORDER BY id LIMIT 1",
    )
    .join(",");
    let album_updated = strings(
        store,
        "SELECT IFNULL(updated_at, '<null>') FROM album ORDER BY id LIMIT 1",
    )
    .join(",");
    let album_rating = strings(
        store,
        "SELECT IFNULL(CAST(average_rating AS TEXT), '<null>') FROM album ORDER BY id",
    )
    .join(",");
    // The annotation rows by identity, not just in count: a reassign moves `item_id`, so the
    // (user, item, type, rating) tuple set is what a leaked leg actually changes.
    let annotation_state = strings(
        store,
        "SELECT user_id || '/' || item_id || '/' || item_type || '/' || CAST(rating AS TEXT) \
         || '/' || CAST(play_count AS TEXT) FROM annotation ORDER BY user_id, item_id, item_type",
    )
    .join(",");
    format!(
        "{}|missing={child_missing}|hash={folder_hash}|album_updated={album_updated}\
         |album_rating={album_rating}|annotation={annotation_state}",
        counts.join("|")
    )
}

/// `busy_retry_replays_once_without_external_side_effects`: a REAL in-process writer
/// lock (a second connection holding a transaction with `busy_timeout = 0`, the same
/// seam `nd-db`'s own retry test uses — `nd-db/tests/runtime_folder_persistence.rs:115-134`)
/// makes the FIRST attempt of the folder transaction hit `SQLITE_BUSY`. The retry lives
/// in the DB (`nd_db::store::try_with_tx_retry_sync`, 3 retries / 4 attempts with the
/// production `(attempt+1) * TX_RETRY_DELAY` backoff, `nd-db/src/store.rs:23-23,302-306`);
/// this lane adds no retry of its own, so a scan through the real `scan_folder` entry is
/// what gets replayed.
///
/// Proof of ONE replay: elapsed time is at least `TX_RETRY_DELAY` (the first attempt hit
/// BUSY) and under twice that (not two retries). Proof of no side effects: exactly one
/// row per entity, one queue item set, no duplicate ids, `folder_inserted` reported once,
/// and the locker's own rolled-back `property` row absent. The lane publishes no progress
/// events at all (SC-05 has no sink), so "no duplicate success event" is structurally
/// guaranteed here and reported as such rather than asserted against a mock.
#[test]
fn busy_retry_replays_once_without_external_side_effects() {
    let tree = Tree::new("busy-replay");
    tree.put("album/one", "known.mp3");
    // `busy_store` gives EVERY handle `busy_timeout = 0`, so contention reports BUSY
    // immediately instead of waiting out the driver's 5s timeout.
    let (store, db) = busy_store("busy-replay", 2);
    seed_library(&store, 1, "busy-replay", &tree.root);
    let mapping = mapping();

    let locker_path = common::dsn(&db);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let locker = std::thread::Builder::new()
        .name("sc05-busy-locker".to_string())
        .spawn(move || {
            let mut conn = nd_dbschema::schema::open(&locker_path)
                .unwrap_or_else(|e| panic!("cannot open the locker connection: {e}"));
            conn.busy_timeout(std::time::Duration::ZERO)
                .expect("no busy wait on the locker");
            let tx = conn.transaction().expect("locker transaction");
            tx.execute(
                "INSERT INTO property(id, value) VALUES('sc05BusyLock', 'held')",
                [],
            )
            .expect("the locker must write inside its transaction");
            ready_tx
                .send(())
                .expect("the locker signals that it holds the write lock");
            std::thread::sleep(std::time::Duration::from_millis(1_500));
            drop(tx);
            drop(conn);
        })
        .expect("spawn the locker");
    // Ordered, not raced: the lock is really held before the delta starts.
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the locker holds the write lock");

    let started = std::time::Instant::now();
    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let elapsed = started.elapsed();
    locker.join().expect("the locker thread returns");

    assert!(
        elapsed >= nd_db::store::TX_RETRY_DELAY,
        "the first attempt hit BUSY and the replay waited the production backoff: {elapsed:?}"
    );
    assert!(
        elapsed < nd_db::store::TX_RETRY_DELAY * 2,
        "exactly ONE replay, not two: {elapsed:?}"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM property"),
        0,
        "the locker rolled back rather than committing"
    );

    // One row per entity, with no duplicates from the replayed attempt.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM folder"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM tag"), 3);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM library_artist"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM library_tag"), 3);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_artists"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_tags"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album_artists"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album_tags"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file_fts"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM album_fts"), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artist_fts"), 2);

    // Queue policy is stable: EnqueueIfMissing ran twice (once per attempt) over the same
    // three items, and its PK collapsed them — no second item per entity.
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM artwork_queue"),
        3,
        "2 artists + 1 album, not doubled by the replay"
    );
    assert_eq!(outcome.queued_item_count, 3);
    assert!(outcome.enqueued_if_missing, "full-scan mode's queue arm");
    assert!(
        outcome.folder_inserted,
        "the folder row was inserted by the replayed attempt"
    );
    assert_eq!(outcome.committed.inserted_media_ids.len(), 1);
    assert_eq!(outcome.new_media_count, 1);
    assert!(
        outcome.committed.warnings.is_empty(),
        "{:?}",
        outcome.committed.warnings
    );

    // The durable id is the one the outcome reports, and a replay of the whole scan finds
    // the same id rather than minting a second one (no id escaped the failed attempt).
    let durable = outcome.committed.media_ids[0].clone();
    assert_eq!(
        strings(&store, "SELECT id FROM media_file"),
        vec![durable.clone()]
    );
    let third = scan_ok(
        &store,
        &job(1, &tree.root, "album/one", later_stamp(), &mapping),
    );
    assert_eq!(third.committed.media_ids, vec![durable]);
    assert_eq!(
        third.new_media_count, 0,
        "the replay matched, not re-inserted"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// The other half of the retry contract: a HARD error is not retried. With no `library`
/// row the folder Put's FK reference fails on every attempt, so if `is_busy` were wrong
/// the same scan would burn 5s+10s+15s and still fail. It fails fast instead, and only
/// the first failing operation is reported.
#[test]
fn hard_error_is_not_retried_and_reports_the_first_leg() {
    let tree = Tree::new("hard-error");
    tree.put("album/one", "known.mp3");
    let (store, db) = busy_store("hard-error", 2);
    let mapping = mapping();

    let started = std::time::Instant::now();
    let before = common::writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/one", stamp(), &mapping),
        &CancelToken::new(),
    )
    .expect_err("the FK violation is fatal");
    let elapsed = started.elapsed();

    match error {
        Phase1Error::Persist { folder, source } => {
            assert_eq!(folder, "album/one");
            assert!(
                source.to_string().to_lowercase().contains("foreign key"),
                "the first failing leg is the FK reference, got {source}"
            );
        }
        other => panic!("expected Persist, got {other}"),
    }
    assert!(
        elapsed < nd_db::store::TX_RETRY_DELAY,
        "a hard error is not retried: {elapsed:?} is under one backoff gap"
    );
    common::assert_no_writes(&store, &before, "hard-error (no retry) rollback");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM folder"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM artwork_queue"), 0);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// The commit-boundary half of `cancel_before_commit_and_after_commit_boundary`: a
/// cancelled scan writes nothing AND leaves no library scan-complete stamp, because
/// SC-05 is phase1-only and never runs ScanEnd. After a successful scan the same fields
/// are still untouched, so "no success stamp" is proved in both directions rather than
/// assumed.
#[test]
fn cancellation_and_success_both_leave_no_scan_end_stamp() {
    let tree = Tree::new("no-scan-end");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("no-scan-end", 2);
    seed_library(&store, 1, "no-scan-end", &tree.root);
    let mapping = mapping();
    let before = strings(
        &store,
        "SELECT CAST(total_songs AS TEXT) || '|' || CAST(total_albums AS TEXT) \
         || '|' || CAST(total_artists AS TEXT) || '|' || CAST(total_folders AS TEXT) \
         || '|' || CAST(last_scan_at AS TEXT) || '|' || CAST(full_scan_in_progress AS TEXT) \
         FROM library WHERE id = 1",
    )
    .join("");

    let token = CancelToken::new();
    token.cancel();
    let before_cancel = common::writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/one", stamp(), &mapping),
        &token,
    )
    .expect_err("a cancelled scan must not proceed");
    assert!(
        matches!(error, Phase1Error::Cancelled { .. }),
        "expected Cancelled, got {error}"
    );
    common::assert_no_writes(&store, &before_cancel, "cancel before any DB work");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        0,
        "nothing written"
    );
    assert_eq!(
        strings(
            &store,
            "SELECT CAST(total_songs AS TEXT) || '|' || CAST(total_albums AS TEXT) \
             || '|' || CAST(total_artists AS TEXT) || '|' || CAST(total_folders AS TEXT) \
             || '|' || CAST(last_scan_at AS TEXT) || '|' || CAST(full_scan_in_progress AS TEXT) \
             FROM library WHERE id = 1"
        )
        .join(""),
        before,
        "a cancelled scan stamps nothing, not even a library counter"
    );

    // Success path: the same fields stay exactly as seeded, because phase1 has no
    // ScanEnd leg (SC-10 owns library completion).
    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    assert_eq!(outcome.audio_count, 1, "the scan really did work");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 1);
    assert_eq!(
        strings(
            &store,
            "SELECT CAST(total_songs AS TEXT) || '|' || CAST(total_albums AS TEXT) \
             || '|' || CAST(total_artists AS TEXT) || '|' || CAST(total_folders AS TEXT) \
             || '|' || CAST(last_scan_at AS TEXT) || '|' || CAST(full_scan_in_progress AS TEXT) \
             FROM library WHERE id = 1"
        )
        .join(""),
        before,
        "a successful phase1 scan does not mark the library scan complete"
    );
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// Non-vacuity proof for [`state_snapshot`]: the warn-only ReassignAnnotation leg
/// (`nd-db/src/scanner_writes.rs:705-716`) leaks state that `COUNT(*)` alone cannot see, so this
/// test drives each of its two statement shapes directly and requires the snapshot to change.
///
/// Two of the three steps hold `COUNT(*) FROM annotation` constant on purpose — a reassign moves
/// `item_id` and `UPDATE album SET average_rating` touches only a column — so if either half of
/// the snapshot were dropped (the `annotation` identity set or `album_rating`), this test fails
/// rather than silently proving nothing. The insert step pins the third shape (a leaked row). The
/// `user` row exists only to satisfy `annotation.user_id`'s FK; `user` is not a writable table in
/// the scan's snapshot, and no scan runs in this test.
#[test]
fn state_snapshot_detects_update_only_annotation_leaks() {
    let tree = Tree::new("snapshot-annotation");
    let (store, db) = fresh_store("snapshot-annotation", 1);
    seed_library(&store, 1, "snapshot-annotation", &tree.root);
    seed_stored_child_that_vanished(&store, &tree);
    execute(
        &store,
        "INSERT INTO user (id, user_name, name, email, password, created_at, updated_at) \
         VALUES ('u1', 'u1', 'u1', 'u1@x', 'p', '2020-01-01 00:00:00', '2020-01-01 00:00:00')",
        &[],
    );
    let before = state_snapshot(&store);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM annotation"), 0);

    // A leaked annotation ROW: changes the count and the identity set.
    execute(
        &store,
        "INSERT INTO annotation (user_id, item_id, item_type, rating, play_count) \
         VALUES ('u1', 'stored-album', 'album', 5, 3)",
        &[],
    );
    assert_ne!(
        state_snapshot(&store),
        before,
        "an inserted annotation row is invisible to the snapshot"
    );
    let inserted = state_snapshot(&store);

    // ReassignAnnotation's move (`sql_annotations.rs:393-398`): UPDATE-only, count unchanged.
    execute(
        &store,
        "UPDATE annotation SET item_id = 'moved' WHERE item_id = 'stored-album'",
        &[],
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM annotation"),
        1,
        "the reassign moves the row without changing its count, which is the blind spot"
    );
    assert_ne!(
        state_snapshot(&store),
        inserted,
        "an UPDATE-only annotation reassign is invisible to the snapshot"
    );
    let moved = state_snapshot(&store);

    // updateAvgRating's follow-up (`sql_annotations.rs:330-334`): column-only, count unchanged.
    execute(
        &store,
        "UPDATE album SET average_rating = 4.25 WHERE id = 'stored-album'",
        &[],
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album"),
        1,
        "the rating refresh changes no count"
    );
    assert_ne!(
        state_snapshot(&store),
        moved,
        "UPDATE album SET average_rating is invisible to the snapshot"
    );
    drop(store);
    remove_db(&db);
}
