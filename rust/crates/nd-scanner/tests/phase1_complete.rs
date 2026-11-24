//! SC-06 narrow phase-1 slice (`D-SC06-SLICE`): multi-folder traversal with Go's change
//! detection and import-selection arms, driven through the PUBLIC entry
//! ([`nd_scanner::phase1::scan_library`]) against a real tree and a real pool, and verified
//! by REAL repository reads.
//!
//! What each test pins, in Go's terms:
//! * `quick_identical_hash_skips_folder_without_writes` — `isOutdated`'s hash arm
//!   (`folder_entry.go:74-76`): a folder whose walker fingerprint equals `folder.hash` is
//!   skipped, and the skip provably writes nothing.
//! * `quick_new_empty_folder_is_skipped_and_a_sibling_is_not` — the `:187-194` arm: a NEW
//!   leaf folder with no audio files and no subfolders is skipped in quick mode while a
//!   changed sibling is ingested, and a full pass does not skip it.
//! * `quick_same_mtime_hash_change_and_missing_return` — the import filter (`:252-262`): a
//!   stored child with an unchanged mtime is NOT re-imported, a file with no stored row IS,
//!   and a stored row flagged `missing` IS re-imported even with an old mtime.
//! * `full_scan_reimports_unchanged_tracks_and_hashes_stay_equal` — the `FullScanInProgress`
//!   arm (`:253`) imports every file without consulting the DB, `matchMediaFile` (`:249`)
//!   reuses each existing id, and the folder fingerprint is byte-identical across passes.
//! * `persist_error_stops_walk_and_untouched_folders_remain` — `stopWalk(err)` (`:358`): the
//!   walk ends at the failing folder, nothing it wrote survives, and folders it never
//!   reached keep their stored rows and are reported as residue.
//! * `multi_folder_tree_imports_all_changed_folders` — postorder traversal ingests every
//!   changed folder of a multi-folder tree in one scan, each through its own transaction,
//!   and a selective target reaches one folder without touching siblings.
//! * `per_folder_missing_marking_touches_only_that_folders_rows` — `entry.missingTracks` is
//!   folder-scoped (`:266-270`), so `MarkMissing` (`:446`) and `Album Touch` (`:456`) move
//!   exactly the vanished folder's rows.
//! * `ingest_error_continues_the_walk_and_only_persist_stops_it` — Go's abort SPLIT: a stage
//!   error drops one folder (`:234-243`, `go-pipeline/pipeline.go:524-537`) while the walk
//!   continues, and only `persistChanges`' failure calls `stopWalk` (`:358`).
//! * `injected_follow_symlinks_off_skips_a_symlinked_target_and_writes_nothing` and
//!   `injected_ignore_dot_folders_off_scans_a_dot_folder_the_default_would_hide` — the scanner's
//!   storage policy is INJECTED on `LibraryScan`, so `conf.Server.Scanner.FollowSymlinks` and
//!   `IgnoreDotFolders` (`conf/configuration.go:1071-1072`) reach the walk AND every folder the
//!   controller ingests, instead of being pinned `true` inside `folder_job`.
//!
//! No persistence is mocked and no batch is hand-built. Every claim is checked with a
//! repository read (`folder.hash`, `media_file.updated_at`, `media_file.missing`,
//! `album.imported_at`) or with a nonzero affected-row count from `CommittedFolderOutcome`,
//! and every "wrote nothing" claim is paired with a matching "wrote something" claim on the
//! same layout.

mod common;

use std::path::Path;

use chrono::{DateTime, Utc};
use common::{
    assert_no_writes, assert_stamp_ordering, assert_wrote, children, copy_pinned, count, execute,
    file_stamp, fresh_store, later_file_time, later_stamp, laterest_stamp, library_scan,
    library_scan_with_policy, mapping, remove_db, scalar, scan_all_ok, seed_library_scan_row,
    set_file_time, set_media_missing, stamp, update_info, writable_rows, ScannerPolicy, Tree,
};
use nd_db::store::SQLStore;
use nd_scanner::contract::CancelToken;
use nd_scanner::phase1::{LibraryScanOutcome, ScanScope, ScanStop};

/// The UTC spelling of a `SystemTime`, for comparing a storage-derived column against the
/// instant a test pinned with [`set_file_time`].
#[must_use]
fn utc_of(at: std::time::SystemTime) -> String {
    nd_db::time::to_text(&DateTime::<Utc>::from(at))
}

/// Assert one folder's stored `hash` equals the fingerprint the scan reported for it.
/// Reading `folder.hash` is the repository half of every skip and re-import claim: an
/// outcome field alone would only prove the ingest thought it wrote something.
fn assert_stored_hash(store: &SQLStore, folder_id: &str, hash: &str, case: &str) {
    assert_eq!(
        scalar(
            store,
            &format!("SELECT hash FROM folder WHERE id = '{folder_id}'")
        ),
        Some(hash.to_string()),
        "{case}: folder.hash does not carry the fingerprint the scan computed"
    );
}

/// Quick scan of the whole library through the public controller.
async fn quick_all(store: &SQLStore, root: &Path, scan_stamp: DateTime<Utc>) -> LibraryScanOutcome {
    let mapping = mapping();
    let scan = library_scan(1, root, &mapping, scan_stamp, scan_stamp, false);
    scan_all_ok(store, &scan, &ScanScope::WholeLibrary).await
}

/// Quick scan of ONE library-relative target, exactly as a selective scan would scope it.
async fn quick_target(
    store: &SQLStore,
    root: &Path,
    target: &str,
    scan_stamp: DateTime<Utc>,
) -> LibraryScanOutcome {
    let mapping = mapping();
    let scan = library_scan(1, root, &mapping, scan_stamp, scan_stamp, false);
    scan_all_ok(store, &scan, &ScanScope::Targets(vec![target.to_string()])).await
}

/// `quick_identical_hash_skips_folder_without_writes`: once a full pass has committed every
/// folder's fingerprint, a quick pass over the unchanged tree finds `folder.hash` equal to
/// the walker's own hash and ingests NOTHING.
///
/// The first pass is the non-vacuity half: it must write rows and it must store one hash per
/// folder, otherwise "no writes" would only compare an empty database with itself.
#[tokio::test]
async fn quick_identical_hash_skips_folder_without_writes() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-identical-hash");
    let folders = ["album/one", "album/two", "album/three"];
    for folder in folders {
        tree.put(folder, "known.mp3");
    }
    let (store, db) = fresh_store("sc06-identical-hash", 2);
    seed_library_scan_row(&store, 1, "sc06-identical-hash", &tree.root, stamp(), true);
    let mapping = mapping();

    // Pass 1: full scan (`lib.FullScanInProgress`, `scanner.go:97`), so every folder —
    // including the fileless root, which Go keeps because it has subfolders — is ingested
    // and its hash stored.
    let before_first = writable_rows(&store);
    let first = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, stamp(), stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    assert!(!first.is_failed(), "pass 1: {:?}", first.failures);
    assert_eq!(
        first.folders_visited, 5,
        "three leaf folders, their `album` ancestor, and the library root"
    );
    assert_eq!(
        first.folders_persisted, 5,
        "a full pass ingests every folder"
    );
    assert_eq!(first.folders_skipped_new_empty, 0, "full mode never skips");
    assert_wrote(&store, &before_first, "full pass that stored the hashes");

    // The map the next scan compares against is a real repository read, not the outcome.
    let stored = update_info(&store, 1);
    assert_eq!(
        stored.len(),
        5,
        "one update-info row per folder, got {stored:?}"
    );
    for report in &first.reports {
        assert_stored_hash(&store, &report.folder_id, &report.hash, "pass 1");
        assert_eq!(
            stored.get(&report.folder_id).map(|info| info.hash.as_str()),
            Some(report.hash.as_str()),
            "{}: the hash the next scan reads is the one this pass computed",
            report.folder_path
        );
        assert_eq!(
            stored.get(&report.folder_id).map(|info| info.updated_at),
            Some(stamp()),
            "{}: stored updated_at is pass 1's stamp, so the quick pass below has a real \
             value to compare with",
            report.folder_path
        );
    }

    // Pass 2: quick, unchanged tree. Nothing is outdated, so nothing is written at all.
    let before_second = writable_rows(&store);
    let second = quick_all(&store, &tree.root, later_stamp()).await;
    assert_eq!(
        second.folders_skipped_up_to_date, second.folders_visited,
        "every visited folder's hash matched its stored hash"
    );
    assert_eq!(second.folders_visited, 5);
    assert_eq!(
        second.folders_changed, 0,
        "isOutdated returned false throughout"
    );
    assert_eq!(second.folders_persisted, 0);
    assert!(second.reports.is_empty(), "no folder was ingested");
    assert!(!second.is_failed());
    assert_no_writes(&store, &before_second, "quick pass over an unchanged tree");
    assert_eq!(
        second.folders_unvisited, 0,
        "the walk popped every update-info entry"
    );
    drop(store);
    remove_db(&db);
}

/// `:187-194`: in quick mode a folder that is NEW (no stored row) and has neither audio
/// files nor subfolders is skipped; a changed sibling is still ingested, and a full pass
/// does not skip the empty one.
#[tokio::test]
async fn quick_new_empty_folder_is_skipped_and_a_sibling_is_not() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-new-empty");
    tree.put("album/changed", "known.mp3");
    let _ = tree.dir("album/empty");
    let (store, db) = fresh_store("sc06-new-empty", 2);
    seed_library_scan_row(&store, 1, "sc06-new-empty", &tree.root, stamp(), false);
    let mapping = mapping();

    // Quick pass. `album/empty` is new, has no files and no subfolders → skipped.
    // `album/changed` is new WITH files → ingested. The root has no files but DOES have
    // subfolders, so Go keeps it (`isEmpty()` also requires `numSubFolders == 0`).
    let outcome = quick_all(&store, &tree.root, stamp()).await;
    assert!(!outcome.is_failed(), "{:?}", outcome.failures);
    assert_eq!(
        outcome.folders_visited, 4,
        "`album/changed`, `album/empty`, their `album` ancestor, and the root"
    );
    assert_eq!(outcome.folders_skipped_new_empty, 1, "only `album/empty`");
    assert_eq!(
        outcome.folders_persisted, 3,
        "the changed folder plus the two ancestors Go keeps (`isEmpty` also requires          `numSubFolders == 0`)"
    );
    let report = outcome
        .report_of("album/changed")
        .expect("the folder with audio was ingested");
    assert_eq!(report.audio_count, 1);
    assert_eq!(report.new_media_count, 1);
    assert!(
        outcome.report_of("album/empty").is_none(),
        "the empty folder was skipped"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        1,
        "exactly the changed folder's track exists"
    );
    let empty_id = nd_db::folder_repo::folder_id(1, "", "album/empty");
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{empty_id}'")
        ),
        None,
        "the skipped new-empty folder got no row at all"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        3,
        "the changed folder, its `album` ancestor, and the root"
    );

    // A full pass does NOT skip it — the arm is guarded by `!FullScanInProgress` (`:187`),
    // which is what makes the skip above a real arm and not an artifact of the layout.
    let before_full = writable_rows(&store);
    let full = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, later_stamp(), later_stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    assert_eq!(full.folders_skipped_new_empty, 0, "full mode never skips");
    assert_eq!(
        full.folders_persisted, 4,
        "all four folders, including the empty one"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        4,
        "the empty folder now has its row"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{empty_id}'")
        ),
        full.report_of("album/empty")
            .map(|report| report.hash.clone()),
        "the empty folder's row carries the fingerprint the full pass computed"
    );
    assert_wrote(
        &store,
        &before_full,
        "full pass that wrote the empty folder's row",
    );
    drop(store);
    remove_db(&db);
}

/// `quick_same_mtime_hash_change_and_missing_return`: the three import-selection arms of
/// `processFolder`'s loop (`phase_1_folders.go:252-262`), one per arm, on ONE folder so the
/// outcomes cannot be confounded by different layouts.
///
/// * a file with no stored row → imported (`:252`'s `toImport[filePath] == nil`);
/// * stored row + mtime not newer than `updated_at` + not missing → NOT imported, so its
///   `updated_at` (derived by `mapping_bridge` from the file's own mtime, never from the
///   scan stamp) stays exactly what the first pass wrote;
/// * stored row flagged `missing` → imported again even with an old mtime (`:262`).
#[tokio::test]
async fn quick_same_mtime_hash_change_and_missing_return() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-filter-arms");
    tree.put("album/one", "known.mp3");
    tree.put("album/one", "known.flac");
    let (store, db) = fresh_store("sc06-filter-arms", 2);
    seed_library_scan_row(&store, 1, "sc06-filter-arms", &tree.root, stamp(), true);
    let mapping = mapping();

    // Pass 1 (full): both files imported, and each row's `updated_at` is the file's mtime.
    let first = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, stamp(), stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    let report = first
        .report_of("album/one")
        .expect("pass 1 ingested the folder");
    assert_eq!(report.new_media_count, 2, "both files were new");
    let mp3_updated_at = scalar(
        &store,
        "SELECT updated_at FROM media_file WHERE path = 'album/one/known.mp3'",
    )
    .expect("the mp3 row exists");
    assert_eq!(
        mp3_updated_at,
        nd_db::time::to_text(&file_stamp()),
        "media_file.updated_at is the file's own mtime, never the scan stamp"
    );

    // Non-vacuity for the skip arm: a FULL pass re-imports that same row. Quick mode must be
    // the only reason it is skipped below, not a state a full pass happened to leave behind.
    let full_again = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, later_stamp(), later_stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    let again = full_again
        .report_of("album/one")
        .expect("full mode re-ingests the same folder");
    assert_eq!(
        again.reimported_paths.len(),
        2,
        "full mode re-imports both stored tracks (`:253` skips the comparison entirely)"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE path = 'album/one/known.mp3'"
        ),
        Some(mp3_updated_at.clone()),
        "a re-import re-stamps nothing: updated_at is still the file's mtime"
    );

    // Quick pass over the unchanged tree: the folder itself is skipped, so no file is
    // considered at all.
    let quiet = quick_all(&store, &tree.root, later_stamp()).await;
    assert_eq!(
        quiet.folders_persisted, 0,
        "the folder itself was up to date"
    );

    // Arms 1 and 2: a NEW file changes the folder hash, so the folder is ingested, but only
    // the file with no stored row is imported.
    copy_pinned(&tree.dir("album/one"), "known.m4a", "known.m4a");
    let third = quick_all(&store, &tree.root, later_stamp()).await;
    let report = third
        .report_of("album/one")
        .expect("the hash change made the folder outdated");
    assert_eq!(third.folders_changed, 1, "the folder's hash changed");
    assert_eq!(
        report.new_media_count, 1,
        "only the file with no stored row was imported"
    );
    assert!(
        report.reimported_paths.is_empty(),
        "the two unchanged-mtime children were NOT re-imported: {:?}",
        report.reimported_paths
    );
    assert_stored_hash(&store, &report.folder_id, &report.hash, "the hash change");
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE path = 'album/one/known.mp3'"
        ),
        Some(mp3_updated_at.clone()),
        "the skipped child's row is unchanged, so the filter skipped its PUT rather than \
         rewriting the same value"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE path = 'album/one/known.m4a'"
        ),
        Some(nd_db::time::to_text(&file_stamp())),
        "the new child was imported with its own mtime"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 3);
    assert_eq!(
        third.folders_persisted, 1,
        "only the changed folder was ingested"
    );

    // Arm 3 (`:262`): a stored row flagged missing is re-imported even though its mtime is
    // not newer than `updated_at`. Change the folder hash WITHOUT touching any file mtime —
    // adding a subfolder changes `numSubFolders`, which the hash preimage includes.
    set_media_missing(&store, "album/one/known.mp3", true);
    let _ = tree.dir("album/one/nested");
    let fourth = quick_all(&store, &tree.root, laterest_stamp()).await;
    let report = fourth
        .report_of("album/one")
        .expect("the num_sub_folders change made the folder outdated");
    assert_eq!(
        report.reimported_paths,
        vec!["album/one/known.mp3".to_string()],
        "the missing-flagged child came back through the `:262` arm and no other child did: \
         {:?}",
        report.reimported_paths
    );
    assert_eq!(
        report.new_media_count, 0,
        "nothing new was on disk, so nothing was inserted"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        3,
        "the re-import reused the existing row instead of adding one"
    );
    assert_eq!(
        report.missing_count, 0,
        "every file on disk was accounted for, so nothing was newly flagged"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE path = 'album/one/known.mp3'"
        ),
        Some(mp3_updated_at),
        "the re-import left updated_at at the file's mtime, as `map_mediafile` requires"
    );
    assert_eq!(
        fourth.folders_skipped_new_empty, 1,
        "`album/one/nested` is new and empty, so the `:187` arm skips it"
    );
    assert_eq!(
        fourth.folders_persisted, 1,
        "and only `album/one` was ingested"
    );
    drop(store);
    remove_db(&db);
}

/// `full_scan_reimports_unchanged_tracks_and_hashes_stay_equal`: a full pass over an
/// unchanged tree re-imports every track, `matchMediaFile` (`:249`) reuses each existing id,
/// and both passes compute the SAME folder fingerprint.
#[tokio::test]
async fn full_scan_reimports_unchanged_tracks_and_hashes_stay_equal() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-full-reimport");
    // Two fixtures with DIFFERENT album ids, so the album-count claim below is not
    // satisfied by two files that happen to share one album.
    tree.put("album/one", "known.mp3");
    tree.put("album/two", "known.m4a");
    let (store, db) = fresh_store("sc06-full-reimport", 2);
    seed_library_scan_row(&store, 1, "sc06-full-reimport", &tree.root, stamp(), true);
    let mapping = mapping();

    let first = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, stamp(), stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    let mut first_ids: Vec<String> = first
        .reports
        .iter()
        .flat_map(|report| report.committed.media_ids.clone())
        .collect();
    first_ids.sort();
    assert_eq!(first_ids.len(), 2, "one track per folder");
    for folder in ["album/one", "album/two"] {
        let report = first.report_of(folder).expect("pass 1 ingested the folder");
        assert_eq!(
            report.committed.inserted_media_ids.len(),
            1,
            "{folder}: its Put took the INSERT leg on the first pass"
        );
    }
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album"),
        2,
        "two distinct albums"
    );

    // Second full pass, later stamp AND later `last_scan_started_at`: `isOutdated`'s
    // full-scan arm (`folder_entry.go:73`) fires on the stored `updated_at`, and the import
    // filter's `FullScanInProgress` arm (`:253`) then imports both files without any DB
    // comparison.
    let second = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, later_stamp(), later_stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    assert!(!second.is_failed(), "{:?}", second.failures);
    assert_eq!(second.folders_changed, second.folders_visited);
    assert_eq!(second.folders_persisted, second.folders_visited);
    let mut second_ids: Vec<String> = second
        .reports
        .iter()
        .flat_map(|report| report.committed.media_ids.clone())
        .collect();
    second_ids.sort();
    assert_eq!(
        second_ids, first_ids,
        "matchMediaFile reused the ids — a re-import that minted new ids would orphan the old \
         rows"
    );
    for folder in ["album/one", "album/two"] {
        let report = second
            .report_of(folder)
            .unwrap_or_else(|| panic!("{folder} was not re-ingested"));
        assert!(
            report.committed.inserted_media_ids.is_empty(),
            "{folder}: no Put took the INSERT leg on the second pass"
        );
        assert_eq!(
            report.reimported_paths.len(),
            1,
            "{folder}: its track came back"
        );
        assert_eq!(report.new_media_count, 0);
        assert_stored_hash(&store, &report.folder_id, &report.hash, folder);
        let first_report = first
            .report_of(folder)
            .unwrap_or_else(|| panic!("{folder} was not ingested on pass 1"));
        assert_eq!(
            report.hash, first_report.hash,
            "{}: the fingerprint drifted across a full re-import of an unchanged folder",
            folder
        );
    }
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        2,
        "still exactly two rows, so the re-import updated rather than duplicated"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM album"),
        2,
        "the album set is unchanged by the second pass"
    );
    drop(store);
    remove_db(&db);
}

/// `persist_error_stops_walk_and_untouched_folders_remain`: Go's `stopWalk(err)` (`:358`)
/// runs ONLY on a `persistChanges` failure, so the walk ends at the failing folder, the
/// failed folder's transaction rolls back completely, and the folders never reached keep
/// their stored rows. Those rows are also the reported residue — the set Go's deferred
/// finalizer (`:530-533`) would mark missing, which this slice deliberately does not mutate.
#[tokio::test]
async fn persist_error_stops_walk_and_untouched_folders_remain() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-persist-stop");
    let folders = ["album/one", "album/two", "album/three"];
    for folder in folders {
        tree.put(folder, "known.mp3");
    }
    let (store, db) = fresh_store("sc06-persist-stop", 2);
    seed_library_scan_row(&store, 1, "sc06-persist-stop", &tree.root, stamp(), false);
    let mapping = mapping();

    // Pre-seed one stored row per folder with a hash that cannot match the walker's, so
    // every folder is outdated AND the residue set exists before the walk starts. With an
    // empty `folder` table the update-info map would be empty and the residue claim vacuous.
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    let seeded: Vec<(String, String)> = folders
        .iter()
        .map(|folder| {
            let id = nd_db::folder_repo::folder_id(1, "", folder);
            execute(
                &store,
                "INSERT INTO folder (id, library_id, path, name, parent_id, missing, hash, num_audio_files, num_playlists, images_updated_at, updated_at, created_at, image_files) \
                 VALUES (?1, 1, ?2, 'seeded', '', 0, 'pre-existing-hash', 1, 0, ?3, ?4, ?4, '[]')",
                &[
                    text(&id),
                    text(folder),
                    text(&nd_db::time::to_text(&file_stamp())),
                    text(&nd_db::time::to_text(&stamp())),
                ],
            );
            (folder.to_string(), id)
        })
        .collect();
    let before = writable_rows(&store);
    assert!(
        before.iter().any(|(_, rows)| *rows > 0),
        "the seeded rows are in the snapshot, so the equality below is not comparing an \
         empty database with itself: {before:?}"
    );

    // Both folder legs fail, so whichever the batch takes stops the walk.
    execute(
        &store,
        "CREATE TRIGGER sc06_inject_insert BEFORE INSERT ON folder BEGIN SELECT RAISE(ABORT, 'injected folder_put failure'); END",
        &[],
    );
    execute(
        &store,
        "CREATE TRIGGER sc06_inject_update BEFORE UPDATE ON folder BEGIN SELECT RAISE(ABORT, 'injected folder_put failure'); END",
        &[],
    );

    let outcome = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, later_stamp(), stamp(), false),
        &ScanScope::WholeLibrary,
    )
    .await;
    assert!(outcome.is_failed(), "the injected failure must be reported");
    match &outcome.stopped {
        Some(ScanStop::Persist { folder, source }) => {
            assert_eq!(
                folder, "album/one",
                "the FIRST emitted folder is the one whose persist failed"
            );
            assert!(
                source.to_string().contains("injected folder_put failure"),
                "the DB failure text survives: {source}"
            );
        }
        other => panic!("expected ScanStop::Persist, got {other:?}"),
    }
    assert_eq!(outcome.folders_persisted, 0, "nothing committed");
    assert!(
        outcome.reports.is_empty(),
        "no ingest report may be published for a rolled-back folder"
    );
    assert_eq!(
        outcome.folders_visited, 1,
        "the walk emitted exactly the failing folder before its context was cancelled"
    );

    // Residue: the update-info entries the walk never popped.
    let mut residue: Vec<String> = seeded.iter().skip(1).map(|(_, id)| id.clone()).collect();
    residue.sort();
    assert_eq!(
        outcome.unvisited_folder_ids, residue,
        "the entries the walk never popped are reported, in sorted order"
    );
    assert_eq!(outcome.folders_unvisited, residue.len());

    // Nothing was written anywhere, per table, and every seeded row is intact.
    assert_no_writes(&store, &before, "persist failure that stopped the walk");
    for (folder, id) in &seeded {
        assert_eq!(
            scalar(
                &store,
                &format!("SELECT hash FROM folder WHERE id = '{id}'")
            ),
            Some("pre-existing-hash".to_string()),
            "{folder}: its stored hash survives, so no partial write reached it"
        );
        assert_eq!(
            scalar(
                &store,
                &format!("SELECT updated_at FROM folder WHERE id = '{id}'")
            ),
            Some(nd_db::time::to_text(&stamp())),
            "{folder}: its stored updated_at survives"
        );
    }
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        0,
        "no track rows from the stopped scan"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file WHERE missing = 1"),
        0,
        "no finalizer ran, so nothing was marked missing either"
    );
    drop(store);
    remove_db(&db);
}

/// `multi_folder_tree_imports_all_changed_folders`: one quick scan over a multi-folder tree
/// ingests every folder that has files (plus the ancestors Go keeps), in postorder, each
/// through its own transaction; a second quick pass writes nothing; and a selective target
/// reaches one folder without touching siblings.
#[tokio::test]
async fn multi_folder_tree_imports_all_changed_folders() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-multi-folder");
    tree.put("album/one", "known.mp3");
    tree.put("album/two", "known.flac");
    tree.put("album/three/four", "known.m4a");
    let (store, db) = fresh_store("sc06-multi-folder", 2);
    seed_library_scan_row(&store, 1, "sc06-multi-folder", &tree.root, stamp(), false);

    let outcome = quick_all(&store, &tree.root, stamp()).await;
    assert!(!outcome.is_failed(), "{:?}", outcome.failures);
    assert_eq!(
        outcome.folders_visited, 6,
        "album/one, album/three/four, album/three, album/two, their album ancestor, and the root"
    );
    assert_eq!(
        outcome.folders_persisted, 6,
        "every folder: the two empty ancestors are kept because they have subfolders, so \
         nothing is skipped as new-empty here"
    );
    assert_eq!(outcome.folders_skipped_new_empty, 0);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        3,
        "one row per folder that holds audio"
    );
    // Postorder: a child is emitted before its parent, siblings sorted by name.
    let paths: Vec<&str> = outcome
        .reports
        .iter()
        .map(|r| r.folder_path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![
            "album/one",
            "album/three/four",
            "album/three",
            "album/two",
            "album",
            "."
        ],
        "children before parents, siblings sorted by name"
    );
    for folder in ["album/one", "album/three/four", "album/two"] {
        let report = outcome
            .report_of(folder)
            .unwrap_or_else(|| panic!("{folder} was not ingested: {paths:?}"));
        assert_eq!(report.audio_count, 1, "{folder}: one audio file");
        assert_eq!(report.new_media_count, 1, "{folder}: its track was new");
        assert_eq!(report.committed.missing_rows_marked, 0);
        assert_stored_hash(&store, &report.folder_id, &report.hash, folder);
        assert_eq!(
            count(
                &store,
                &format!(
                    "SELECT COUNT(*) FROM media_file WHERE folder_id = '{}'",
                    report.folder_id
                )
            ),
            1,
            "{folder}: its track is scoped to its own folder row"
        );
    }
    // The two fileless ancestors got rows (Go keeps ancestors of a changed subtree) but no
    // media of their own.
    for folder in ["album", "album/three", "."] {
        let report = outcome
            .report_of(folder)
            .unwrap_or_else(|| panic!("{folder} (an ancestor) was not ingested: {paths:?}"));
        assert_eq!(report.audio_count, 0, "{folder}: no audio of its own");
        assert_eq!(report.new_media_count, 0);
        assert!(report.folder_inserted, "{folder}: its row was inserted");
    }

    // A second quick pass writes nothing: every folder's stored hash now matches.
    let before = writable_rows(&store);
    let second = quick_all(&store, &tree.root, later_stamp()).await;
    assert_eq!(second.folders_persisted, 0);
    assert_eq!(second.folders_skipped_up_to_date, second.folders_visited);
    assert_no_writes(
        &store,
        &before,
        "quick pass over the tree the first pass fully imported",
    );

    // Selective scope: the target is entered as its own walk root, so only that folder is
    // emitted — and with an unchanged hash nothing is written.
    let selective = quick_target(&store, &tree.root, "album/three/four", later_stamp()).await;
    assert_eq!(
        selective.targets_walked, 1,
        "the target was walked, not skipped"
    );
    assert_eq!(
        selective.folders_visited, 1,
        "only the selected folder is emitted"
    );
    assert_eq!(
        selective.folders_skipped_up_to_date, 1,
        "its stored hash still matches"
    );
    assert_no_writes(
        &store,
        &before,
        "selective target whose folder is unchanged",
    );

    // Now make ONLY that folder changed (newer mtime) and re-run the same scope: the change
    // is picked up through the target path and no sibling folder is touched.
    let deeper = tree.root.join("album/three/four/known.m4a");
    set_file_time(&deeper, later_file_time());
    let changed = quick_target(&store, &tree.root, "album/three/four", later_stamp()).await;
    assert_eq!(changed.folders_changed, 1);
    assert_eq!(changed.folders_persisted, 1);
    let report = changed
        .report_of("album/three/four")
        .expect("the selective target was re-ingested");
    assert_eq!(
        report.reimported_paths,
        vec!["album/three/four/known.m4a".to_string()],
        "the `mtime > updated_at` arm (`:260`) re-imported the newer file"
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT updated_at FROM media_file WHERE path = 'album/three/four/known.m4a'"
        ),
        Some(utc_of(later_file_time())),
        "the row's updated_at is the NEW mtime, taken from the file rather than the scan stamp"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        3,
        "the selective scope re-stamped one row and added nothing"
    );
    let tree_folder_ids: Vec<String> = [
        "album",
        "album/one",
        "album/three",
        "album/three/four",
        "album/two",
        ".",
    ]
    .iter()
    .map(|folder| nd_db::folder_repo::folder_id(1, "", folder))
    .collect();
    let listed = tree_folder_ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        count(
            &store,
            &format!("SELECT COUNT(*) FROM media_file WHERE folder_id NOT IN ({listed})")
        ),
        0,
        "every row still belongs to a folder of this tree, so the selective scope created no          orphan"
    );
    drop(store);
    remove_db(&db);
}

/// `per_folder_missing_marking_touches_only_that_folders_rows`: `entry.missingTracks` is
/// built from the folder's own DB children minus the files it saw on disk (`:266-270`), so
/// `MarkMissing` (`:446`) and `Album Touch` (`:456`) — both inside the SAME folder
/// transaction — move only the folder whose file vanished.
#[tokio::test]
async fn per_folder_missing_marking_touches_only_that_folders_rows() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-missing-scope");
    // mp3 and m4a derive DIFFERENT album ids, so "only that folder's album was touched" is
    // a real distinction rather than two names for one row.
    tree.put("album/one", "known.mp3");
    tree.put("album/two", "known.m4a");
    let (store, db) = fresh_store("sc06-missing-scope", 2);
    seed_library_scan_row(&store, 1, "sc06-missing-scope", &tree.root, stamp(), true);
    let mapping = mapping();

    let first = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, stamp(), stamp(), true),
        &ScanScope::WholeLibrary,
    )
    .await;
    let one = first.report_of("album/one").expect("album/one imported");
    let two = first.report_of("album/two").expect("album/two imported");
    assert_eq!(one.missing_count, 0);
    assert_eq!(two.missing_count, 0);
    let album_one = one.album_ids[0].clone();
    let album_two = two.album_ids[0].clone();
    assert_ne!(album_one, album_two, "the two fixtures are distinct albums");
    let untouched_album_imported_at = scalar(
        &store,
        &format!("SELECT imported_at FROM album WHERE id = '{album_two}'"),
    )
    .expect("album/two exists");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file WHERE missing = 0"),
        2,
        "both rows start nonmissing, so the marking below has work to do"
    );

    // Remove ONE file, quick scan. Only `album/one`'s hash changes, so only that folder is
    // ingested and only its child is flagged.
    std::fs::remove_file(tree.root.join("album/one/known.mp3"))
        .unwrap_or_else(|e| panic!("cannot remove the file: {e}"));
    let second = quick_all(&store, &tree.root, later_stamp()).await;
    assert!(!second.is_failed(), "{:?}", second.failures);
    assert_eq!(
        second.folders_persisted, 1,
        "only the folder whose file vanished was ingested"
    );
    let report = second
        .report_of("album/one")
        .expect("album/one was re-ingested with its file gone");
    assert_eq!(report.audio_count, 0, "no file remains on disk");
    assert_eq!(
        report.missing_count, 1,
        "Go's missingTracks carried exactly one row"
    );
    assert_eq!(
        report.committed.missing_rows_marked, 1,
        "MarkMissing affected a nonzero number of rows, so the marking really ran"
    );
    assert_eq!(
        report.committed.touched_album_ids,
        vec![album_one.clone()],
        "Touch's album set is the missing track's own album (`:450-453`)"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file WHERE missing = 1"),
        1,
        "exactly one row flagged"
    );
    assert_eq!(
        scalar(&store, "SELECT path FROM media_file WHERE missing = 1"),
        Some("album/one/known.mp3".to_string()),
        "the flagged row is the vanished one"
    );
    assert_eq!(
        scalar(&store, "SELECT id FROM media_file WHERE missing = 1"),
        Some(one.committed.media_ids[0].clone()),
        "and it is the first pass's child, id and all"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM media_file WHERE path = 'album/two/known.m4a' AND missing = 1"
        ),
        0,
        "the sibling folder's row is NOT flagged"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT imported_at FROM album WHERE id = '{album_two}'")
        ),
        Some(untouched_album_imported_at.clone()),
        "the sibling album was not touched"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT imported_at FROM album WHERE id = '{album_one}'")
        ),
        Some(nd_db::time::to_text(&later_stamp())),
        "the touched album carries the ingest's own stamp, so Touch ran in this transaction"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file"),
        2,
        "missing marking flags rows, it does not delete them"
    );
    assert_eq!(
        children(&store, &one.folder_id).len(),
        1,
        "the folder's child list still holds its one row, now flagged"
    );

    // A folder with NO missing rows marks nothing even when ingested: the missing leg is
    // empty, so Go runs no statement at all (`:444-446`'s guard) — and an unrelated folder's
    // ingest cannot undo the earlier marking.
    set_file_time(&tree.root.join("album/two/known.m4a"), later_file_time());
    let third = quick_all(&store, &tree.root, laterest_stamp()).await;
    let report = third
        .report_of("album/two")
        .expect("album/two was re-ingested after its mtime moved");
    assert_eq!(report.missing_count, 0);
    assert_eq!(report.committed.missing_rows_marked, 0);
    assert!(
        report.committed.touched_album_ids.is_empty(),
        "no Touch set"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM media_file WHERE path = 'album/one/known.mp3' AND missing = 1"
        ),
        1,
        "the earlier marking is not undone by an unrelated folder's ingest"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM media_file WHERE missing = 1"),
        1,
        "still exactly the one row from the vanished file"
    );
    drop(store);
    remove_db(&db);
}

/// The library row is the source of the scan's mode, its `last_scan_started_at` and its
/// previous PID spec — Go reads all three in `newScanJob` (`:82-97`) before anything is
/// walked, so [`LibraryScan::from_library_row`] must be a real repository read, not an
/// injected guess. Asserted through `LibraryRepo::get` (the public seam B5 names) plus one
/// scan actually driven by the derived value.
#[tokio::test]
async fn scan_fields_come_from_the_stored_library_row() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-library-row");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("sc06-library-row", 2);
    // The row says a full scan is in progress and started at `later_stamp()`.
    seed_library_scan_row(
        &store,
        1,
        "sc06-library-row",
        &tree.root,
        later_stamp(),
        true,
    );
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    execute(
        &store,
        "UPDATE library SET scanned_pid_album = ?1",
        &[text(common::OVERRIDE_ALBUM_PID)],
    );
    let mapping = mapping();

    let scan = nd_scanner::phase1::LibraryScan::from_library_row(
        &store,
        1,
        stamp(),
        &mapping,
        /* full_scan_requested */ false,
    )
    .expect("the seeded row is readable");
    assert_eq!(scan.library_id, 1);
    assert_eq!(scan.library_name, "sc06-library-row");
    assert_eq!(
        scan.library_root,
        tree.root.as_os_str(),
        "the root comes from `library.path`, so the walk is scoped by the row"
    );
    assert!(
        scan.full_scan_in_progress,
        "Go's `lib.FullScanInProgress || fullScan` (`:97`) — the row alone is enough"
    );
    assert_eq!(
        scan.last_scan_started_at,
        later_stamp(),
        "isOutdated's full-scan arm compares against the stored start instant"
    );
    assert_eq!(
        scan.prev_album_pid.as_deref(),
        Some(common::OVERRIDE_ALBUM_PID),
        "Go's `prevAlbumPIDConf` (`:93-95`): the stored scanned spec, because it differs \
         from the effective album spec"
    );
    assert_eq!(
        scan.pid.album, "",
        "the row's own override is kept verbatim"
    );

    // A quick request against a row that says `false` still yields `false`, and one against
    // a row that says `true` yields `true` — the OR is the whole rule.
    execute(
        &store,
        "UPDATE library SET full_scan_in_progress = 0, scanned_pid_album = ''",
        &[],
    );
    let quick =
        nd_scanner::phase1::LibraryScan::from_library_row(&store, 1, stamp(), &mapping, false)
            .expect("the updated row is readable");
    assert!(
        !quick.full_scan_in_progress,
        "a quick scan of a quick row stays quick"
    );
    assert_eq!(
        quick.prev_album_pid, None,
        "an empty scanned spec disables the re-derivation arm, as Go's empty string does"
    );
    let promoted = nd_scanner::phase1::LibraryScan::from_library_row(
        &store,
        1,
        stamp(),
        &mapping,
        /* full_scan_requested */ true,
    )
    .expect("the same row, full request");
    assert!(
        promoted.full_scan_in_progress,
        "the request promotes it (`:97`), which is what the import filter's full arm reads"
    );

    // Drive a scan with the derived value: its root and mode came from the row, and the
    // folder is ingested through them.
    // Drive a scan with the derived value: its root and mode came from the row, and every
    // folder of the tree is ingested because the row said a full scan is in progress.
    let before = writable_rows(&store);
    let outcome = scan_all_ok(&store, &scan, &ScanScope::WholeLibrary).await;
    assert!(!outcome.is_failed(), "{:?}", outcome.failures);
    assert_eq!(
        outcome.folders_persisted, 3,
        "album/one, its `album` ancestor, and the root — the row's full-scan mode ingests \
         the fileless ancestors too"
    );
    assert_eq!(
        outcome.folders_skipped_new_empty, 0,
        "full mode never skips"
    );
    assert_wrote(&store, &before, "scan driven by the derived library row");

    // A library with no row gets no job: Go's `newScanJob` error means the library is
    // skipped, and nothing is walked or written. The snapshot is taken AFTER the derived
    // scan above, so this compares real state, not an empty database with itself.
    let after_scan = writable_rows(&store);
    let missing =
        nd_scanner::phase1::LibraryScan::from_library_row(&store, 99, stamp(), &mapping, false);
    match missing {
        Err(nd_scanner::phase1::LibraryScanError::LastUpdates { library_id, source }) => {
            assert_eq!(library_id, 99);
            assert!(
                matches!(
                    source,
                    nd_scanner::phase1::ScanReadSource::Library(
                        nd_db::library_repo::LibraryError::NotFound
                    )
                ),
                "the row read's own typed failure, got {source}"
            );
        }
        other => panic!("expected LastUpdates/NotFound, got {other:?}"),
    }
    assert_no_writes(&store, &after_scan, "no-row library");
    drop(store);
    remove_db(&db);
}

/// A cancel requested before the walk starts ingests nothing and writes nothing. Go's
/// `finalize` returns early on a cancelled phase (`:524-526`), which is exactly why the
/// residue is REPORTED rather than mutated — and with nothing stored yet there is no
/// residue to report.
#[tokio::test]
async fn cancellation_before_the_walk_writes_nothing_and_reports_residue() {
    let tree = Tree::new("sc06-cancelled");
    tree.put("album/one", "known.mp3");
    let (store, db) = fresh_store("sc06-cancelled", 2);
    seed_library_scan_row(&store, 1, "sc06-cancelled", &tree.root, stamp(), false);
    let mapping = mapping();
    let before = writable_rows(&store);

    let token = CancelToken::new();
    token.cancel();
    let scan = library_scan(1, &tree.root, &mapping, stamp(), stamp(), false);
    let outcome = nd_scanner::phase1::scan_library(&store, &scan, &ScanScope::WholeLibrary, &token)
        .await
        .expect("a cancelled scan still returns an outcome, not an error");
    assert_eq!(outcome.folders_persisted, 0, "no folder was ingested");
    assert!(outcome.reports.is_empty());
    assert!(
        matches!(outcome.stopped, Some(ScanStop::Cancelled)),
        "{:?}",
        outcome.stopped
    );
    assert_no_writes(&store, &before, "scan cancelled before the walk started");
    assert_eq!(
        outcome.folders_unvisited, 0,
        "nothing had been stored yet, so there is no residue — and none was mutated"
    );
    drop(store);
    remove_db(&db);
}

/// `ingest_error_continues_the_walk_and_only_persist_stops_it`: Go's abort policy is a SPLIT,
/// not one rule. `persistChanges`' failure calls `stopWalk(err)` (`phase_1_folders.go:358`), but a
/// stage error drops only that folder — `out, err = s.fn(in)`; `if err == nil && s.emitToOutCh`
/// (`go-pipeline/pkg/pipeline/pipeline.go:524-537`) — so `persistChanges` never sees it, it writes
/// nothing, and the walk keeps emitting the rest.
///
/// Injected on the FIRST emitted folder by poisoning its stored child's `participants` cell:
/// Go's `dbMediaFile.PostScan` decodes that shim UNGUARDED (`persistence/mediafile_repository.go:55`),
/// so the children read — the very first DB step of `processFolder` (`:231-243`) — fails for that
/// folder alone. That is Go's `log.Error` + `return entry, err`, i.e. the stage-error class.
///
/// Mutating every ingest error into a walk-stop keeps the other eight tests green, which is exactly
/// why this assertion exists: folder #2 must still be persisted while `failures` is non-empty, and
/// the failing folder must still be popped from the residue (it was popped at emit time, before its
/// ingest ran).
#[tokio::test]
async fn ingest_error_continues_the_walk_and_only_persist_stops_it() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-ingest-error");
    // Two folders whose names decide the postorder emission order deterministically: children are
    // emitted before parents and siblings are sorted by name, so `album/aaa` is the first folder
    // the controller ingests.
    tree.put("album/aaa", "known.mp3");
    tree.put("album/zzz", "known.flac");
    let (store, db) = fresh_store("sc06-ingest-error", 2);
    seed_library_scan_row(&store, 1, "sc06-ingest-error", &tree.root, stamp(), false);

    // Seed one row per leaf folder with a hash that cannot match the walker's, so both are
    // outdated in quick mode and both reach the ingest. `aaa`'s child carries a malformed
    // `participants` cell, which is the injected stage failure.
    let text = |value: &str| nd_db::sqlrepo::Value::Text(value.to_string());
    // One row per leaf folder, each with its own fixture: mp3 and flac derive DIFFERENT album
    // ids, so "the sibling's rows only" stays a real distinction.
    let ids: Vec<(String, String)> = [("album/aaa", "mp3"), ("album/zzz", "flac")]
        .into_iter()
        .map(|(folder, suffix)| {
            let id = nd_db::folder_repo::folder_id(1, "", folder);
            execute(
                &store,
                "INSERT INTO folder (id, library_id, path, name, parent_id, missing, hash, \
                 num_audio_files, num_playlists, images_updated_at, updated_at, created_at, image_files) \
                 VALUES (?1, 1, ?2, 'seeded', '', 0, 'pre-existing-hash', 1, 0, ?3, ?4, ?4, '[]')",
                &[
                    text(&id),
                    text(folder),
                    text(&nd_db::time::to_text(&file_stamp())),
                    text(&nd_db::time::to_text(&stamp())),
                ],
            );
            execute(
                &store,
                "INSERT INTO media_file (id, path, folder_id, library_id, album_id, missing, \
                 updated_at, created_at, birth_time, participants) \
                 VALUES (?1, ?2, ?3, 1, ?4, 0, ?5, ?6, ?6, ?7)",
                &[
                    text(format!("m-{folder}").as_str()),
                    text(format!("{folder}/known.{suffix}").as_str()),
                    text(&id),
                    text(format!("a-{folder}").as_str()),
                    text(&nd_db::time::to_text(&file_stamp())),
                    text(&nd_db::time::to_text(&stamp())),
                    // The injection: `{` is not decodable JSON, so Go's unguarded
                    // `unmarshalParticipants` fails the row scan for THIS folder's cursor read.
                    text(if folder == "album/aaa" { "{" } else { "{}" }),
                ],
            );
            (folder.to_string(), id)
        })
        .collect();
    let (poisoned_folder, poisoned_id) = ids.first().cloned().expect("two seeded folders");
    assert_eq!(poisoned_folder, "album/aaa");

    let before = writable_rows(&store);
    let outcome = quick_all(&store, &tree.root, later_stamp()).await;

    // The failure is reported and marks the library failed — Go's `eg.Wait` surfaces the first
    // stage error as a failed phase — but it is NOT the stop-the-walk arm.
    assert!(outcome.is_failed(), "a folder abort must fail the library");
    assert_eq!(
        outcome.failures.len(),
        1,
        "exactly one folder abort: {:?} (a persist failure would have stopped the walk)",
        outcome.failures
    );
    let failure = outcome.failures.first().expect("one failure");
    match failure {
        nd_scanner::phase1::Phase1Error::ChildrenRead { folder, source } => {
            assert_eq!(folder, "album/aaa", "the abort names the poisoned folder");
            assert!(
                source.to_string().contains("parsing participants"),
                "the cause is Go's PostScan decode failure, not a fabricated one: {source}"
            );
        }
        other => panic!("expected ChildrenRead (Go's `:234-243`), got {other:?}"),
    }
    assert!(
        outcome.stopped.is_none(),
        "ONLY a persist failure stops the walk (Go's `:358`); a stage error does not call \
         stopWalk: {:?}",
        outcome.stopped
    );

    // The walk kept going: folder #2 and both ancestors were reached, each committed through its
    // own transaction. Only the aborted folder is absent from the reports — Go's pipeline never
    // emits a stage-error item, so persistChanges never saw it.
    assert!(
        outcome.report_of("album/aaa").is_none(),
        "the aborted folder produced no report, so nothing of it was published"
    );
    assert_eq!(
        outcome.folders_persisted, 3,
        "the sibling plus the two ancestors: every emitted folder except the aborted one"
    );
    let zzz = outcome
        .report_of("album/zzz")
        .expect("the sibling folder was ingested");
    assert_stored_hash(&store, &zzz.folder_id, &zzz.hash, "album/zzz");
    assert!(
        !zzz.committed.folder_inserted,
        "its row already existed, so the sibling took the Put's UPDATE leg"
    );
    assert_eq!(
        scalar(
            &store,
            &format!(
                "SELECT updated_at FROM folder WHERE id = '{}'",
                zzz.folder_id
            )
        ),
        Some(nd_db::time::to_text(&later_stamp())),
        "the sibling's folder.updated_at is THIS scan's stamp, so its transaction committed"
    );
    // Its one child is NOT re-imported: the stored row's `updated_at` equals the file's mtime, so
    // Go's `mtime > updated_at` arm is false (`:261`). The persist is folder-row-only, which is
    // precisely what makes the aborted folder's untouched row a contrast rather than a tautology.
    assert_eq!(zzz.new_media_count, 0, "nothing new was imported");
    assert!(
        zzz.reimported_paths.is_empty(),
        "its child was not re-imported"
    );

    // The aborted folder wrote nothing of its own: its stored row is untouched.
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT hash FROM folder WHERE id = '{poisoned_id}'")
        ),
        Some("pre-existing-hash".to_string()),
        "the aborted folder's hash is unchanged — Go's stage error never reaches persistChanges"
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM media_file WHERE folder_id = '{poisoned_id}' AND updated_at \
                 > '{}'",
                nd_db::time::to_text(&stamp())
            )
        ),
        0,
        "no row of the aborted folder was re-stamped"
    );

    // Every folder the walk emitted left the residue set — including the aborted one, popped at
    // emit time. The ancestors are ingested too (they are not new-empty), so nothing is left.
    assert_eq!(
        outcome.unvisited_folder_ids,
        Vec::<String>::new(),
        "the aborted folder was popped at emit time, so the deferred finalizer would not see it"
    );
    assert_eq!(outcome.folders_unvisited, 0);
    assert_eq!(
        outcome.folders_visited, 4,
        "album/aaa, album/zzz, album and the library root — all emitted despite the abort"
    );
    assert_eq!(
        outcome.folders_changed, 4,
        "every seeded folder is outdated"
    );

    // Non-vacuity of the "wrote nothing" claims above: the scan definitely wrote.
    assert_wrote(
        &store,
        &before,
        "scan whose first folder aborted mid-ingest",
    );
    drop(store);
    remove_db(&db);
}

/// L1: `follow_symlinks` is INJECTED on `LibraryScan`, so a scan configured with following
/// disabled does not silently get links followed. Go reads it from `conf.Server.Scanner.FollowSymlinks`
/// (`conf/configuration.go:1071`), consumed at `scanner/walk_dir_tree.go:47-51`; the port must apply
/// the caller's value to both the [`WalkRequest`] and every folder it ingests.
///
/// The observable consequence is a selective target that IS a symlinked directory: with following
/// enabled the walker enters it and the folder is persisted (`tests/phase1_slice.rs` pins the
/// single-folder half of the same rule), with it disabled the target is skipped and nothing is
/// written. The enabled scan in the same database is the non-vacuity half.
#[cfg(unix)]
#[tokio::test]
async fn injected_follow_symlinks_off_skips_a_symlinked_target_and_writes_nothing() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-policy-symlink");
    tree.put("album/real", "known.mp3");
    std::os::unix::fs::symlink(tree.root.join("album/real"), tree.root.join("album/alias"))
        .expect("symlink the folder");
    let (store, db) = fresh_store("sc06-policy-symlink", 2);
    seed_library_scan_row(&store, 1, "sc06-policy-symlink", &tree.root, stamp(), true);
    let mapping = mapping();
    let before = writable_rows(&store);

    let disabled = scan_all_ok(
        &store,
        &library_scan_with_policy(
            1,
            &tree.root,
            &mapping,
            stamp(),
            stamp(),
            true,
            ScannerPolicy::without_symlinks(),
        ),
        &ScanScope::Targets(vec!["album/alias".to_string()]),
    )
    .await;
    assert_eq!(
        disabled.targets_skipped, 1,
        "the symlinked target is skipped"
    );
    assert_eq!(disabled.targets_walked, 0, "nothing was traversed");
    assert_eq!(disabled.folders_visited, 0, "no folder was emitted");
    assert_eq!(disabled.folders_persisted, 0);
    assert_no_writes(&store, &before, "symlinked target with following disabled");

    // Non-vacuity: the same target with Go's default policy is walked and persisted, so the skip
    // above is the injected flag's doing rather than a constant refusal.
    let enabled = scan_all_ok(
        &store,
        &library_scan(1, &tree.root, &mapping, later_stamp(), later_stamp(), true),
        &ScanScope::Targets(vec!["album/alias".to_string()]),
    )
    .await;
    assert_eq!(
        enabled.targets_skipped, 0,
        "following enabled walks the link"
    );
    assert_eq!(enabled.targets_walked, 1);
    assert_eq!(
        enabled.folders_persisted, 1,
        "the folder under the link is persisted"
    );
    let alias = enabled
        .report_of("album/alias")
        .expect("the folder under the link was ingested");
    assert_eq!(alias.audio_count, 1, "its audio file is classified");
    assert_eq!(alias.new_media_count, 1, "and imported");
    assert_wrote(&store, &before, "symlinked target with following enabled");

    drop(store);
    remove_db(&db);
}

/// L1's other injected value: `ignore_dot_folders`. Go's `isIgnoredEntry`
/// (`scanner/walk_dir_tree.go:332-337`) hides a dot-named CHILD folder only while
/// `conf.Server.Scanner.IgnoreDotFolders` is set (`conf/configuration.go:1072`), so a caller that
/// opts out must get that folder scanned — and a scan that hard-codes `true` cannot.
///
/// The target here is the library root, so the dot folder is reached as a CHILD (Go does not apply
/// the name policy to a walk target itself, and neither does the walker). Quick mode makes the two
/// outcomes maximally distinct: with the folder ignored, the root is a new empty folder and is
/// skipped (`:187-194`) and nothing is stored; with the opt-out, the dot folder is emitted, holds a
/// file, and the root gains a subfolder, so both are persisted.
#[tokio::test]
async fn injected_ignore_dot_folders_off_scans_a_dot_folder_the_default_would_hide() {
    assert_stamp_ordering();
    let tree = Tree::new("sc06-policy-dotfolder");
    tree.put(".hidden", "known.m4a");
    let (store, db) = fresh_store("sc06-policy-dotfolder", 2);
    seed_library_scan_row(
        &store,
        1,
        "sc06-policy-dotfolder",
        &tree.root,
        stamp(),
        false,
    );
    let mapping = mapping();

    let ignored = quick_all(&store, &tree.root, stamp()).await;
    assert!(!ignored.is_failed(), "{:?}", ignored.failures);
    assert_eq!(
        ignored.folders_visited, 1,
        "only the library root is emitted: the dot folder is hidden by the default policy"
    );
    assert_eq!(
        ignored.folders_skipped_new_empty, 1,
        "with no visible child the root is new and empty, so Go skips it (`:187-194`)"
    );
    assert_eq!(ignored.folders_persisted, 0, "nothing is written");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM folder"), 0);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 0);

    // Same tree, same stamp, one flag changed: the dot folder is entered.
    let scanned = scan_all_ok(
        &store,
        &library_scan_with_policy(
            1,
            &tree.root,
            &mapping,
            stamp(),
            stamp(),
            false,
            ScannerPolicy::without_dot_folder_ignoring(),
        ),
        &ScanScope::WholeLibrary,
    )
    .await;
    assert!(!scanned.is_failed(), "{:?}", scanned.failures);
    assert_eq!(
        scanned.folders_visited, 2,
        "the dot folder plus the root (which now has a subfolder)"
    );
    assert_eq!(scanned.folders_skipped_new_empty, 0);
    assert_eq!(scanned.folders_persisted, 2, "both folders get rows");
    let hidden = scanned
        .report_of(".hidden")
        .expect("the dot folder was ingested");
    assert_eq!(hidden.audio_count, 1, "its audio file is classified");
    assert_eq!(hidden.new_media_count, 1, "and imported");
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM media_file WHERE path = '.hidden/known.m4a'"
        ),
        1,
        "the row is in the database, keyed by the library-relative path"
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        2,
        "the root and the dot folder, so the default scan really wrote nothing"
    );
    drop(store);
    remove_db(&db);
}
