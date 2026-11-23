//! SC-05 narrow slice (D-SC05-SLICE): the standalone selected-folder capture's policy
//! preflight, its shared cycle/depth semantics, and the fact that a rejected scan provably
//! writes nothing.
//!
//! Two claims drive every test here:
//!
//! 1. **Policy before persistence.** `scan_folder` preflights the selected target with the
//!    SC-02 walker's own skip rules (`scanner/walk_dir_tree.go:39-51`) *before* any
//!    `.ndignore` read, any listing, and any DB work, and reports the rejection as a typed
//!    [`Phase1Error::TargetRejected`] carrying the walker's warning. SC-06's controller turns
//!    that into a skipped target (`targets_skipped` + warning), not a fatal library error, so
//!    the rejection must be observable and must leave the database untouched.
//! 2. **One enterability semantics.** The selected folder is a fresh walker root whose live
//!    identities are `[canonical library root, canonical target]`, and its child directories
//!    go through the walker's shared gate (`walk.rs::enter_refusal`: depth cap, then the
//!    active-identity cycle check). A refused child still follows the walker's fallback
//!    accumulation — its `lstat` mtime folds into `mod_time`, and it is classified by name —
//!    rather than being silently omitted. Every parity assertion below therefore compares the
//!    ingest against the committed walker over the SAME scope, never against itself.

mod common;

use common::{
    children, count, fresh_store, job, job_without_symlinks, mapping, pool_healthy, remove_db,
    scalar, scan_ok, seed_library, stamp, walk_selected, writable_rows, Tree,
};
use nd_scanner::contract::{CancelToken, ScanWarning, ScanWarningKind};
use nd_scanner::folder::FolderCoverage;
use nd_scanner::phase1::{self, Phase1Error, TargetRejection};

/// A rejected scan must leave every writable table exactly as it found it, **table by table**.
/// Per table rather than one global sum deliberately: a summed count passes when one helper
/// inserts a row and another deletes one (`folder` +1 and `item_artwork` −1 net to zero), so the
/// per-helper equality is what makes a stray junction, an FTS ghost, a queue row, or a rolled-back
/// artwork deletion fail the claim.
fn assert_no_writes(store: &nd_db::store::SQLStore, before: &[(&'static str, i64)], case: &str) {
    common::assert_no_writes(store, before, case);
}

/// The fingerprint actually written for a folder, read back.
fn stored_hash(store: &nd_db::store::SQLStore, folder_id: &str) -> Option<String> {
    scalar(
        store,
        &format!("SELECT hash FROM folder WHERE id = '{folder_id}'"),
    )
}

/// Warnings of one kind from an outcome, as `(kind, path)` for readable failures.
fn kinds(warnings: &[ScanWarning], kind: ScanWarningKind) -> Vec<Option<String>> {
    warnings
        .iter()
        .filter(|warning| warning.kind == kind)
        .map(|warning| warning.path.as_ref().map(|p| p.to_string()))
        .collect()
}

/// A plain directory target is captured, fingerprinted, and committed: the accepted behavior
/// the preflight must not disturb. The one subdirectory under it is entered and counted, so a
/// preflight that refused ordinary folders would fail here rather than silently pass.
#[test]
fn a_regular_target_is_captured_and_committed() {
    let tree = Tree::new("slice-regular");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    let child = tree.dir("album/one/live");
    common::copy_pinned(&child, "known.flac", "hidden.flac");
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("slice-regular", 2);
    seed_library(&store, 1, "slice-regular", &tree.root);
    let mapping = mapping();

    let before = writable_rows(&store);
    assert!(
        before.iter().all(|(_, count)| *count == 0),
        "the seeded library row is not a writable-table row: {before:?}"
    );

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    assert_eq!(
        outcome.audio_count, 1,
        "only the folder's own track is audio"
    );
    assert_eq!(outcome.new_media_count, 1);
    assert_eq!(outcome.tag_failures, Vec::new());
    assert!(
        outcome.warnings.is_empty(),
        "an ordinary folder warns about nothing, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT num_audio_files FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        1,
        "the folder row carries the capture's audio count"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone()),
        "the fingerprint written is the capture's fingerprint"
    );
    assert_eq!(children(&store, &outcome.folder_id).len(), 1);
    // Per helper and per table in the accepted direction too: no writable table may lose rows,
    // and at least one must gain them, so the matching `assert_no_writes` calls in this suite are
    // non-vacuous rather than comparing an empty database with itself.
    common::assert_wrote(&store, &before, "accepted single-folder scan");
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// With following ENABLED a symlinked target is scanned and persisted with the link's own
/// logical identity — `folder.path`/`folder.name` stay the link's, the child keeps the link
/// path — and the fingerprint matches the walker over the same scope. The preflight rejects
/// nothing on this arm, so the configured-symlink-root exception and endpoint behavior are
/// unchanged.
#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_target_is_persisted_when_following_is_enabled() {
    let tree = Tree::new("slice-follow-on");
    let real = tree.dir("album/real");
    common::copy_pinned(&real, "known.mp3", "known.mp3");
    std::os::unix::fs::symlink(tree.root.join("album/real"), tree.root.join("album/alias"))
        .expect("symlink the folder");
    common::set_dir_time(&real, common::file_time());

    let (store, db) = fresh_store("slice-follow-on", 2);
    seed_library(&store, 1, "slice-follow-on", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(
        &store,
        &job(1, &tree.root, "album/alias", stamp(), &mapping),
    );

    assert_eq!(outcome.audio_count, 1);
    assert_eq!(outcome.new_media_count, 1);
    assert!(
        outcome.warnings.is_empty(),
        "a followed target is not a warning, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT name FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some("alias".to_string()),
        "folder.name is the link's basename"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT path FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some("album".to_string()),
        "folder.path is the link's cleaned directory, not the endpoint's"
    );
    let kid = children(&store, &outcome.folder_id)
        .pop()
        .expect("one child row");
    assert_eq!(
        kid.path, "album/alias/known.mp3",
        "the child keeps the logical link path"
    );

    // Independent oracle for the same scope: the walker enters the linked target as a fresh
    // walk root, so its fingerprint must equal the capture's.
    let (events, summary) = walk_selected(&tree.root, "album/alias", true).await;
    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/alias")
        .expect("the walk emits the selected target");
    assert_eq!(
        outcome.hash, walked.hash,
        "the capture and the walker fingerprint a symlinked target identically"
    );
    assert_eq!(
        summary.targets_skipped, 0,
        "following enabled skips nothing on this scope"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// With following DISABLED, a target that **is** a symlink is rejected before any DB work, and
/// the rejection carries the walker's own `SymlinkedTargetSkipped` warning so SC-06 can report
/// it as a skipped target instead of failing the library.
#[cfg(unix)]
#[tokio::test]
async fn follow_disabled_rejects_a_symlinked_target_without_writes() {
    let tree = Tree::new("slice-follow-off-target");
    let real = tree.dir("album/real");
    common::copy_pinned(&real, "known.mp3", "known.mp3");
    std::os::unix::fs::symlink(tree.root.join("album/real"), tree.root.join("album/alias"))
        .expect("symlink the folder");
    let (store, db) = fresh_store("slice-follow-off-target", 2);
    seed_library(&store, 1, "slice-follow-off-target", &tree.root);
    let mapping = mapping();

    let before = writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job_without_symlinks(1, &tree.root, "album/alias", stamp(), &mapping),
        &CancelToken::new(),
    )
    .expect_err("a symlinked target must be rejected with following disabled");
    let (rejection, warning) = match error {
        Phase1Error::TargetRejected {
            folder,
            rejection,
            warning,
        } => {
            assert_eq!(folder, "album/alias", "the error names the selected folder");
            (rejection, warning)
        }
        other => panic!("expected TargetRejected, got {other}"),
    };

    assert_eq!(
        rejection,
        TargetRejection::SymlinkedPath,
        "the rejection is the walker's link rule (`walk_dir_tree.go:47-51`)"
    );
    assert_eq!(warning.kind, ScanWarningKind::SymlinkedTargetSkipped);
    assert_eq!(
        warning.path.as_ref().map(|p| p.to_string()),
        Some("album/alias".to_string()),
        "the warning names the selected target"
    );
    assert_eq!(warning.library_id, Some(1));

    assert_no_writes(&store, &before, "symlinked target");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM folder"),
        0,
        "no folder row for the rejected target"
    );
    assert_eq!(
        store.idle_count().expect("pool idle count"),
        2,
        "the rejection never opened a transaction, so no lease was taken"
    );
    assert!(pool_healthy(&store));

    // The walker reports the same rule for the same scope, so a controller sees one policy:
    // same warning kind, path, and library, and the same "skipped, not failed" outcome.
    let (_events, summary) = walk_selected(&tree.root, "album/alias", false).await;
    let walker_warnings = summary.warnings_of(ScanWarningKind::SymlinkedTargetSkipped);
    assert_eq!(walker_warnings.len(), 1, "the walker skips the target once");
    assert_eq!(walker_warnings[0].kind, warning.kind);
    assert_eq!(walker_warnings[0].path, warning.path);
    assert_eq!(walker_warnings[0].library_id, warning.library_id);
    assert!(
        summary.outcome.is_complete(),
        "the walker treats it as a skip, not a failure: {}",
        summary.outcome.describe()
    );
    assert_eq!(summary.targets_skipped, 1, "skipped, not walked");
    assert_eq!(summary.targets_walked, 0);

    remove_db(&db);
}

/// With following DISABLED, a target reached through a symlinked **ancestor** is rejected the
/// same way — `is_symlinked_path` walks every ancestor, exactly as Go's `isSymlinkedPath`
/// (`scanner/walk_dir_tree.go:237-245`) does — and again writes nothing. With following
/// ENABLED the same target scans normally, which is what makes the rejection the policy flag
/// rather than an artifact of the tree.
#[cfg(unix)]
#[test]
fn follow_disabled_rejects_a_target_under_a_symlinked_ancestor_without_writes() {
    let tree = Tree::new("slice-follow-off-ancestor");
    let real = tree.dir("album/real");
    common::copy_pinned(&real, "known.mp3", "known.mp3");
    let nested = tree.dir("album/real/nested");
    common::copy_pinned(&nested, "known.flac", "known.flac");
    std::os::unix::fs::symlink(tree.root.join("album/real"), tree.root.join("alias"))
        .expect("link a mid-path ancestor");
    // `alias/nested` resolves through the linked ancestor to `album/real/nested`.

    let (store, db) = fresh_store("slice-follow-off-ancestor", 2);
    seed_library(&store, 1, "slice-follow-off-ancestor", &tree.root);
    let mapping = mapping();

    let before = writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job_without_symlinks(1, &tree.root, "alias/nested", stamp(), &mapping),
        &CancelToken::new(),
    )
    .expect_err("a target under a linked ancestor must be rejected");

    match error {
        Phase1Error::TargetRejected {
            folder,
            rejection,
            warning,
        } => {
            assert_eq!(
                folder, "alias/nested",
                "the error names the selected target"
            );
            assert_eq!(rejection, TargetRejection::SymlinkedPath);
            assert_eq!(warning.kind, ScanWarningKind::SymlinkedTargetSkipped);
            assert_eq!(
                warning.path.as_ref().map(|p| p.to_string()),
                Some("alias/nested".to_string()),
                "the warning names the target, not the ancestor"
            );
        }
        other => panic!("expected TargetRejected, got {other}"),
    }

    assert_no_writes(&store, &before, "ancestor link");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM media_file"), 0);
    assert!(pool_healthy(&store));

    // Following enabled: the same target scans normally, so the flag is the only difference.
    let outcome = scan_ok(
        &store,
        &job(1, &tree.root, "alias/nested", stamp(), &mapping),
    );
    assert_eq!(outcome.audio_count, 1, "the followed ancestor is traversed");
    assert_eq!(outcome.new_media_count, 1);
    assert_eq!(
        children(&store, &outcome.folder_id)
            .pop()
            .expect("one child")
            .path,
        "alias/nested/known.flac",
        "the logical path stays under the link"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// The configured library root is trusted configuration: when the root itself IS a symlink,
/// following-disabled scans of folders under it are still accepted, because `is_symlinked_path`
/// never examines the root component (Go's rule, ported in `walk.rs`). Non-vacuity comes from
/// the same tree reached one link deeper, where the symlink component IS inside the path.
#[cfg(unix)]
#[test]
fn a_symlinked_library_root_stays_exempt_with_following_disabled() {
    let base = common::external_root("slice-symlink-root");
    let real = base.join("real");
    std::fs::create_dir_all(real.join("album/one")).expect("create album/one under the real root");
    let real = std::fs::canonicalize(&real).expect("canonicalize real root");
    common::copy_pinned(&real.join("album/one"), "known.mp3", "known.mp3");
    // `<base>/link` → `<base>/real`: the ONLY link component is the configured root itself.
    std::os::unix::fs::symlink(&real, base.join("link")).expect("symlink the root");
    let linked_root = base.join("link");

    let (store, db) = fresh_store("slice-symlink-root", 2);
    seed_library(&store, 1, "slice-symlink-root", &linked_root);
    let mapping = mapping();

    let outcome = scan_ok(
        &store,
        &job_without_symlinks(1, &linked_root, "album/one", stamp(), &mapping),
    );
    assert_eq!(
        outcome.audio_count, 1,
        "a symlinked configured root does not reject its own folders"
    );
    assert_eq!(outcome.new_media_count, 1);
    assert!(
        outcome.warnings.is_empty(),
        "no symlink warning for the trusted root, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(children(&store, &outcome.folder_id).len(), 1);

    // Non-vacuity: the SAME linked root, but the selected folder reached through a symlink
    // component INSIDE the root. `is_symlinked_path` examines every component below the root
    // and never the root itself, so the exemption is specifically about the configured root —
    // without this half, "the root is exempt" could also be satisfied by a preflight that
    // rejects nothing at all.
    std::os::unix::fs::symlink(real.join("album/one"), real.join("album/link"))
        .expect("a link component inside the root");
    let before = writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job_without_symlinks(1, &linked_root, "album/link", stamp(), &mapping),
        &CancelToken::new(),
    )
    .expect_err("a target reached through a non-root symlink must be rejected");
    match error {
        Phase1Error::TargetRejected { rejection, .. } => {
            assert_eq!(rejection, TargetRejection::SymlinkedPath)
        }
        other => panic!("expected TargetRejected, got {other}"),
    }
    assert_no_writes(&store, &before, "link component inside the linked root");

    common::remove_external_root(&base);
    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A target that does not exist is rejected with the walker's `TargetMissing` rule and writes
/// nothing. Go skips such a target so a watcher event for a deleted folder stays in the job's
/// last-updates (`walk_dir_tree.go:39-45`), which is why this is a skip rather than an error.
#[tokio::test]
async fn a_missing_target_is_rejected_without_writes() {
    let tree = Tree::new("slice-missing-target");
    let _ = tree.dir("album/one");
    common::copy_pinned(&tree.dir("album/live"), "known.mp3", "known.mp3");

    let (store, db) = fresh_store("slice-missing-target", 2);
    seed_library(&store, 1, "slice-missing-target", &tree.root);
    let mapping = mapping();

    let before = writable_rows(&store);
    let error = phase1::scan_folder(
        &store,
        &job(1, &tree.root, "album/gone/deeper", stamp(), &mapping),
        &CancelToken::new(),
    )
    .expect_err("a nonexistent target cannot be captured");
    let (rejection, warning) = match error {
        Phase1Error::TargetRejected {
            folder,
            rejection,
            warning,
        } => {
            assert_eq!(folder, "album/gone/deeper");
            (rejection, warning)
        }
        other => panic!("expected TargetRejected, got {other}"),
    };
    assert_eq!(rejection, TargetRejection::Missing);
    assert_eq!(warning.kind, ScanWarningKind::TargetMissing);
    assert_eq!(
        warning.path.as_ref().map(|p| p.to_string()),
        Some("album/gone/deeper".to_string())
    );
    assert_no_writes(&store, &before, "missing target");

    // The walker reports the same missing target as a skip for the same scope.
    let (_events, summary) = walk_selected(&tree.root, "album/gone/deeper", true).await;
    assert_eq!(summary.targets_skipped, 1);
    assert_eq!(summary.warnings_of(ScanWarningKind::TargetMissing).len(), 1);
    assert!(summary.outcome.is_complete(), "skipping is not a failure");

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// Entering one sibling must not make a **later alias to that same directory** look like an
/// active cycle, and the capture's live identity count must not grow with its children.
///
/// Go's `walkFolder` (`scanner/walk_dir_tree.go:76-95`) recurses per child and pops on the way
/// out, so while a sibling is being considered the only live identities are the library root and
/// the target: `active.len()` is 2 for the whole listing. Two links to one sibling are therefore
/// two indexable directories (`walk_dir_tree_test.go:213` pins a sibling alias as normally
/// indexed), and an alias to a child that was already descended is likewise a separate folder.
///
/// The capture's accumulation broke both halves of that, and each assertion below is a case the
/// old shape got wrong:
///
/// * pushing each entered child without descending or popping made a LATER alias to that same
///   directory resolve to an already-live identity, so the shared gate reported
///   `DirectoryCycleSkipped` for a directory the walker descends into;
/// * `active.len()` grew by one per entered child, so a wide folder reported
///   `DepthLimitReached` on its last children at logical depth 2, where a descending walker
///   refuses nothing.
///
/// Entries are sorted by name bytes, so the capture's listing order is deterministic and the real
/// child precedes its alias: `a-child`, `alias-to-child`, `known.mp3`, `sibling-a`, `sibling-b`.
#[cfg(unix)]
#[tokio::test]
async fn entered_siblings_are_not_active_ancestors_for_later_aliases() {
    let tree = Tree::new("slice-sibling-aliases");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    // A real child directory, listed FIRST by name (`a-child` sorts before `alias-to-child`).
    let child = tree.dir("album/one/a-child");
    common::copy_pinned(&child, "known.flac", "nested.flac");
    // A sibling outside the selected folder, aliased twice.
    let other = tree.dir("album/other");
    common::copy_pinned(&other, "known.ogg", "sibling.ogg");
    std::os::unix::fs::symlink(
        tree.root.join("album/one/a-child"),
        folder.join("alias-to-child"),
    )
    .expect("alias to the child directory listed before it");
    std::os::unix::fs::symlink(tree.root.join("album/other"), folder.join("sibling-a"))
        .expect("first alias to the sibling");
    std::os::unix::fs::symlink(tree.root.join("album/other"), folder.join("sibling-b"))
        .expect("second alias to the SAME sibling");
    // Pin the folder's mtime AFTER the links exist, so an alias that is wrongly refused and then
    // falls through the accumulation raises `mod_time` observably.
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("slice-sibling-aliases", 2);
    seed_library(&store, 1, "slice-sibling-aliases", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let (events, summary) = walk_selected(&tree.root, "album/one", true).await;
    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the selected folder");

    // Headline parity on one scope, two code paths, one fingerprint.
    assert_eq!(
        outcome.hash, walked.hash,
        "entered siblings must not change the capture's fingerprint"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone()),
        "and that fingerprint is what the folder row stores"
    );

    // Nothing is refused here: no cycle warning from either path, and none naming an alias.
    let cycle = kinds(&outcome.warnings, ScanWarningKind::DirectoryCycleSkipped);
    assert_eq!(
        cycle,
        Vec::<Option<String>>::new(),
        "a sibling entered earlier in the same flat listing is not an active ancestor: {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        summary
            .warnings_of(ScanWarningKind::DirectoryCycleSkipped)
            .len(),
        0,
        "the walker refuses no cycle in this scope, so the capture must not either"
    );
    assert_eq!(
        kinds(&outcome.warnings, ScanWarningKind::SymlinkSkipped),
        Vec::<Option<String>>::new(),
        "an entered alias is a subfolder, not an unclassified skipped symlink"
    );

    // `numSubFolders`: the real child plus all three aliases, on both paths. The old
    // accumulation refused `alias-to-child` and `sibling-b`, so the capture reported 2 here while
    // the walker reported 4.
    assert_eq!(
        walked.snapshot.num_sub_folders, 4,
        "the walker descends the child, its alias, and both sibling aliases"
    );
    assert_eq!(
        outcome.num_sub_folders, walked.snapshot.num_sub_folders,
        "the capture agrees, because no entered sibling joins the live identity set"
    );

    // The walker indexes each alias as its own folder, which makes the count and hash
    // comparisons above non-vacuous: refusing one would change the emitted set.
    let mut emitted: Vec<String> = events
        .iter()
        .map(|event| event.snapshot.path.to_string())
        .collect();
    emitted.sort();
    assert_eq!(
        emitted,
        vec![
            "album/one".to_string(),
            "album/one/a-child".to_string(),
            "album/one/alias-to-child".to_string(),
            "album/one/sibling-a".to_string(),
            "album/one/sibling-b".to_string(),
        ],
        "every alias is walked as a folder of its own (the alias TARGET's own logical path is \
         outside the selected scope, so it is not emitted)"
    );
    assert_eq!(summary.folders_emitted, 5);
    assert_eq!(summary.folders_incomplete, 0);

    // Non-vacuity of the fingerprint: the same snapshot with a refused alias's `lstat` mtime
    // folded in — exactly what a wrongly-refused child produces — must hash differently, and the
    // links really are newer than the pinned folder stamp.
    let newest_link_time = ["alias-to-child", "sibling-a", "sibling-b"]
        .iter()
        .map(|name| {
            std::fs::symlink_metadata(folder.join(name))
                .unwrap_or_else(|e| panic!("lstat {name}: {e}"))
                .modified()
                .unwrap_or_else(|e| panic!("{name} mtime: {e}"))
        })
        .max()
        .expect("three link mtimes");
    assert!(
        newest_link_time > common::file_time(),
        "the links must be newer than the pinned folder stamp or the fingerprint claim is \
         vacuous: {newest_link_time:?} vs {:?}",
        common::file_time()
    );
    assert_eq!(
        walked.snapshot.mod_time,
        common::file_time(),
        "entered children contribute no mtime, so the walker's stamp is the pinned one"
    );
    assert_ne!(
        outcome.hash,
        {
            let mut refused_shape = walked.snapshot.clone();
            refused_shape.mod_time = newest_link_time;
            refused_shape.hash().expect("refused-child-shaped hash")
        },
        "non-vacuity: folding a refused alias's mtime changes the fingerprint, so the pinned hash \
         really pins the entered-child behavior"
    );

    assert_eq!(
        walked.snapshot.coverage,
        FolderCoverage::Complete,
        "a sibling alias excludes nothing from the listing"
    );

    // The ingest imports the folder's own file only: descended children belong to their own
    // folders, not to this one.
    assert_eq!(
        outcome.audio_count, 1,
        "only the folder's own file is its audio"
    );
    assert_eq!(
        children(&store, &outcome.folder_id)
            .iter()
            .map(|kid| kid.path.as_str())
            .collect::<Vec<_>>(),
        vec!["album/one/known.mp3"],
        "no media row comes from a child or an alias"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A folder with `MAX_WALK_DEPTH` readable children must report no depth refusal at all.
///
/// The capture lists ONE directory, so its live identity set is `[root, target]` and stays that
/// way for every sibling. Pushing an entered child without popping made `active.len()` grow to
/// `2 + children`, so from child number `MAX_WALK_DEPTH - 2` onward the gate's depth arm fired:
/// the capture reported `DepthLimitReached` for real, descendable children at logical depth 2,
/// stopped counting them, and folded their mtimes into the fingerprint. The descending walker
/// refuses nothing in this scope (its live length is 2 at the target, 3 inside a child), so any
/// refusal here is the capture's own artifact — asserted against the walker's numbers, never the
/// capture's.
#[cfg(unix)]
#[tokio::test]
async fn a_wide_folder_is_never_accumulated_into_the_depth_cap() {
    use nd_scanner::walk::MAX_WALK_DEPTH;

    let tree = Tree::new("slice-wide-folder");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    for index in 0..MAX_WALK_DEPTH {
        let _ = tree.dir(&format!("album/one/c{index:04}"));
    }
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("slice-wide-folder", 2);
    seed_library(&store, 1, "slice-wide-folder", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let (events, summary) = walk_selected(&tree.root, "album/one", true).await;
    assert!(
        summary.outcome.is_complete(),
        "a wide folder is not a failure: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the selected folder");

    assert_eq!(
        kinds(&outcome.warnings, ScanWarningKind::DepthLimitReached),
        Vec::<Option<String>>::new(),
        "the live identity count is 2, not 2 + the number of children: {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert!(
        summary
            .warnings_of(ScanWarningKind::DepthLimitReached)
            .is_empty(),
        "the descending walker refuses nothing in this scope either"
    );
    assert_eq!(
        walked.snapshot.num_sub_folders, MAX_WALK_DEPTH,
        "the walker descends every child"
    );
    assert_eq!(
        outcome.num_sub_folders,
        MAX_WALK_DEPTH,
        "so the capture counts every child too; the old accumulation stopped counting at \
         {} and refused the rest",
        MAX_WALK_DEPTH - 2,
    );
    assert_eq!(
        outcome.hash, walked.hash,
        "and fingerprints the scope identically, with no refused child's mtime folded in"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone())
    );
    assert_eq!(
        walked.snapshot.mod_time,
        common::file_time(),
        "entered children raise nothing, so the stamp stays the pinned one"
    );
    assert_eq!(summary.folders_emitted, MAX_WALK_DEPTH + 1);
    assert_eq!(summary.folders_incomplete, 0);

    // Nothing at all is refused in this scope, on either path: not just no depth warning, so a
    // regression that fired the cycle arm instead of the depth arm also fails here.
    assert!(
        outcome.warnings.is_empty(),
        "the capture warns about nothing in a wide folder: {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert!(
        summary.warnings.is_empty(),
        "the walker warns about nothing in this scope either: {:?}",
        summary
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );

    // Non-vacuity of the COUNT itself: `numSubFolders` is a hash fact
    // (`folder_entry.go:117-123`), so the shape the buggy accumulation produced — the cap minus
    // the refused children — must hash differently. Pinning the count alone could be satisfied
    // by a capture that counted wrongly and hashed its own wrong number; this pins that the
    // stored fingerprint depends on the count being right.
    assert_ne!(
        outcome.hash,
        {
            let mut refused_shape = walked.snapshot.clone();
            refused_shape.num_sub_folders = MAX_WALK_DEPTH - 2;
            refused_shape.hash().expect("refused-children-shaped hash")
        },
        "non-vacuity: refusing {} children changes the fingerprint, so the hash equality above \
         pins the full count",
        2
    );
    assert_eq!(
        outcome.num_sub_folders, MAX_WALK_DEPTH,
        "and the count the fingerprint was built from is the full one"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// The library root as a selected target: Go's `NewFolder` (`model/folder.go:57-75`) splits the
/// cleaned root path `"."` into `dir = ""` and `name = "."` — the `dir == "." && name == "."` arm
/// empties only `dir` — and `model/folder_test.go:115` pins `folder.Name == "."`. So the root row
/// stores `path = ""`, `name = "."`, `parent_id = ""`, and its id is the hash of `<lib>:"."`.
///
/// The root is also the one target whose live identity set is `[root]` alone: the target's
/// canonical identity IS the root's, so `selected_identity_context` adds no second entry
/// (`decisions.md` D-SC05-SLICE item 3). Reading all four columns back is what pins that, since a
/// root scan is the only scope where `folder_id()` and the identity context agree on one path.
#[tokio::test]
async fn scanning_the_library_root_stores_go_s_dot_name_and_root_identity() {
    let tree = Tree::new("slice-root-target");
    common::copy_pinned(&tree.root, "known.mp3", "root.mp3");
    let _ = tree.dir("album/one");
    common::copy_pinned(&tree.dir("album/one"), "known.flac", "one.flac");
    common::set_dir_time(&tree.root, common::file_time());

    let (store, db) = fresh_store("slice-root-target", 2);
    seed_library(&store, 1, "slice-root-target", &tree.root);
    let mapping = mapping();

    let job = job(1, &tree.root, ".", stamp(), &mapping);
    assert!(
        job.folder_path.is_root(),
        "the job really is the library root"
    );
    let outcome = scan_ok(&store, &job);

    // Go's NewFolder split for the root, read back from the row.
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT name FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(".".to_string()),
        "folder.name is Go's dot name for the library root, not an empty string"
    );
    assert_eq!(
        scalar(
            &store,
            &format!("SELECT path FROM folder WHERE id = '{}'", outcome.folder_id)
        ),
        Some(String::new()),
        "folder.path is the emptied directory part"
    );
    assert_eq!(
        scalar(
            &store,
            &format!(
                "SELECT parent_id FROM folder WHERE id = '{}'",
                outcome.folder_id
            )
        ),
        Some(String::new()),
        "the root has no parent row (model/folder.go:60-62)"
    );
    assert_eq!(
        outcome.folder_id,
        nd_db::folder_repo::folder_id(1, "", "."),
        "the id key is the cleaned root path (model/folder.go:46-52)"
    );
    assert_eq!(
        outcome.folder_path, ".",
        "the outcome reports the root's display path"
    );
    assert!(outcome.folder_inserted, "the root row is newly inserted");

    // Root scope: its own file is its audio, its one directory is a counted subfolder, and both
    // paths fingerprint the scope identically — the identity context being `[root]` alone changes
    // no child's enterability here, which is the point of asserting parity on this scope.
    assert_eq!(outcome.audio_count, 1, "the root's own file");
    assert_eq!(
        outcome.num_sub_folders, 1,
        "the root's one child directory is counted"
    );

    let (walk_events, summary) = walk_selected(&tree.root, ".", true).await;
    assert!(
        summary.outcome.is_complete(),
        "{}",
        summary.outcome.describe()
    );
    let walked = walk_events
        .iter()
        .find(|event| event.snapshot.path.is_root())
        .expect("the walk emits the library root");
    assert_eq!(
        walked.hash, outcome.hash,
        "one fingerprint for the root scope"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone())
    );
    assert_eq!(walked.snapshot.num_sub_folders, 1);
    assert_eq!(
        walked.snapshot.path.to_string(),
        ".",
        "the walker's logical root renders as the dot path too"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A selected folder whose child is a **self-link** (back to the selected folder) plus a
/// sibling alias must fingerprint, count subfolders, and warn exactly like the SC-02 walker on
/// the same scope — D-SC05-SLICE items 2 and 3, checked against the walker rather than against
/// this lane's own numbers.
///
/// The self link is refused by the shared gate and then falls through to the walker's
/// fallback accumulation (`EnterOutcome::Skipped` → `accumulate_entry`): its `lstat` mtime
/// folds into `mod_time` and it is reported with the walker's own warning rather than dropped
/// silently. The sibling alias is entered and counted, because a distinct identity outside the
/// active path is indexed separately in Go too (`walk_dir_tree_test.go:213`) — which is why the
/// identity context is `[root, target]` and not the target's logical ancestors.
#[cfg(unix)]
#[tokio::test]
async fn a_self_cyclying_child_hashes_and_counts_like_the_walker() {
    let tree = Tree::new("slice-self-cycle");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");
    // A sibling alias inside the selected folder: a distinct identity, so it is entered.
    let other = tree.dir("album/other");
    common::copy_pinned(&other, "known.flac", "hidden.flac");
    std::os::unix::fs::symlink(tree.root.join("album/other"), folder.join("sibling"))
        .expect("alias of a sibling directory");
    // A link back to the selected folder itself: its identity is live, so both paths refuse it.
    std::os::unix::fs::symlink(tree.root.join("album/one"), folder.join("self"))
        .expect("self link");
    // Pinned AFTER both links exist, so the links' own mtimes are strictly later than the
    // folder's — which is what makes the fallback-accumulation claim non-vacuous.
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("slice-self-cycle", 2);
    seed_library(&store, 1, "slice-self-cycle", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));

    let (events, summary) = walk_selected(&tree.root, "album/one", true).await;
    assert!(
        summary.outcome.is_complete(),
        "the walker's own run must succeed: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the selected folder");

    // Headline parity: same scope, two code paths, one fingerprint.
    assert_eq!(
        outcome.hash, walked.hash,
        "the selected capture and the SC-02 walker fingerprint a self-cyclying folder identically"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone()),
        "the fingerprint written is that fingerprint"
    );

    // Cycle refusal, once, on both sides — and it is the self link, not the sibling alias.
    // The walker reports a refusal at the WALK level (`enter_directory` pushes it into the
    // summary, never into the folder's snapshot), while this single-folder ingest reports the
    // same warning on its own folder. So the parity claim is: same kind, same path, same
    // library, once each.
    assert_eq!(
        kinds(&outcome.warnings, ScanWarningKind::DirectoryCycleSkipped),
        vec![Some("album/one/self".to_string())],
        "the self link is refused exactly once by the capture, got {:?}",
        outcome
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        kinds(&summary.warnings, ScanWarningKind::DirectoryCycleSkipped),
        vec![Some("album/one/self".to_string())],
        "the walker refuses the self link exactly once"
    );
    assert_eq!(
        summary
            .warnings_of(ScanWarningKind::DirectoryCycleSkipped)
            .iter()
            .map(|w| w.library_id)
            .collect::<Vec<_>>(),
        vec![Some(1)],
        "the refusal carries the library id"
    );
    // `numSubFolders`: the entered alias counts, the refused self link does not. The capture
    // counts its own subfolders the same way (`ScanOutcome::num_sub_folders`).
    assert_eq!(
        walked.snapshot.num_sub_folders, 1,
        "the sibling alias is entered; the refused self link is not a subfolder"
    );
    assert_eq!(
        outcome.num_sub_folders, 1,
        "the capture agrees with the walker about `numSubFolders`"
    );

    // The refused link keeps the walker's own downstream treatment: its mtime is folded in
    // (asserted below), and a followed link whose endpoint is a directory is reported as an
    // unclassified symlink rather than dropped silently (`resolveEntryName`,
    // `walk_dir_tree.go:253-298`; `walk.rs::accumulate_entry`). Both paths report it once.
    assert_eq!(
        kinds(&outcome.warnings, ScanWarningKind::SymlinkSkipped),
        vec![Some("album/one/self".to_string())],
        "the capture reports the refused self link as an unclassified symlink"
    );
    assert_eq!(
        kinds(&summary.warnings, ScanWarningKind::SymlinkSkipped),
        vec![Some("album/one/self".to_string())],
        "the walker reports the same entry the same way"
    );
    // The entered sibling alias is neither refused nor skipped: no warning names it.
    assert!(
        outcome
            .warnings
            .iter()
            .chain(summary.warnings.iter())
            .all(
                |w| w.path.as_ref().map(|p| p.to_string()) != Some("album/one/sibling".to_string())
            ),
        "the sibling alias is entered, not skipped"
    );
    // The only classified entries are the folder's real file: the entered alias is a subfolder
    // (Go counts it and never `Info()`s it), and the refused link is unclassified.
    assert_eq!(
        walked
            .snapshot
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec!["known.mp3".to_string()],
        "the walker's own classified set for this scope"
    );

    // Non-vacuity of the accumulation: the self link's mtime is later than the pinned folder
    // stamp, so folding it in must change the fingerprint. Without this half the two paths
    // could agree while one of them omitted the refused child's facts.
    let self_link_time = std::fs::symlink_metadata(folder.join("self"))
        .expect("lstat the self link")
        .modified()
        .expect("self link mtime");
    assert!(
        self_link_time > common::file_time(),
        "the self link's mtime must be later than the pinned folder stamp, or the accumulation \
         claim is vacuous: {self_link_time:?} vs {:?}",
        common::file_time()
    );
    assert_eq!(
        walked.snapshot.mod_time, self_link_time,
        "the walker folds the refused self link's mtime into the folder mtime"
    );
    assert_ne!(
        outcome.hash,
        {
            let mut oracle = walked.snapshot.clone();
            oracle.mod_time = common::file_time();
            oracle.hash().expect("omission-shaped hash")
        },
        "non-vacuity: omitting the refused child's mtime changes the fingerprint, so the pinned \
         hash really pins the fallback accumulation"
    );

    // Coverage is unaffected: a refusal excludes nothing from the listing.
    assert_eq!(
        walked.snapshot.coverage,
        FolderCoverage::Complete,
        "a cycle refusal is not an incomplete listing"
    );

    // The ingest still imports the folder's one real track and nothing from either link.
    assert_eq!(
        outcome.audio_count, 1,
        "only the folder's own file is audio"
    );
    assert_eq!(
        children(&store, &outcome.folder_id)
            .iter()
            .map(|kid| kid.path.as_str())
            .collect::<Vec<_>>(),
        vec!["album/one/known.mp3"],
        "no media row comes from the alias or the self link"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A child directory deeper than the cap, in the same scope, proves the two paths share one
/// gate — and pins where they legitimately differ.
///
/// The selected-folder capture lists exactly one directory, so its live identity count is two
/// (canonical root + canonical target) and the cap of `MAX_WALK_DEPTH` can never fire on its
/// own child: the depth arm is a DESCENT decision, and only a descending walker descends. The
/// walker's refusal therefore lands deep in the chain and never on the selected folder's own
/// entry — which is precisely why the fingerprints still agree: the refusal is in a different
/// folder's snapshot, and the selected folder descends into its one readable child on both
/// paths. The gate's depth boundary itself — including this one-level offset — is unit-tested
/// in `walk.rs::depth_gate_boundary_is_exact_and_keeps_the_production_cap`.
///
/// Chain length derives from `MAX_WALK_DEPTH`: the walker seeds one live identity (the root)
/// and pushes one per descended directory, so the first refused directory is the one entered
/// while `MAX_WALK_DEPTH` identities are live — the `d` at level `MAX_WALK_DEPTH - 1` below the
/// target, i.e. the last one built here.
#[cfg(unix)]
#[tokio::test]
async fn a_child_deeper_than_the_cap_is_refused_where_the_walker_refuses_it() {
    use std::ffi::OsStr;

    use nd_scanner::storage::LogicalPath;
    use nd_scanner::walk::MAX_WALK_DEPTH;

    let tree = Tree::new("slice-depth");
    let folder = tree.dir("album/one");
    common::copy_pinned(&folder, "known.mp3", "known.mp3");

    let mut dir = folder.clone();
    let mut logical = LogicalPath::parse("album/one").expect("logical path");
    for _ in 0..(MAX_WALK_DEPTH - 1) {
        dir = dir.join("d");
        logical = logical.join(OsStr::new("d")).expect("depth component");
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {dir:?}: {e}"));
    }
    common::copy_pinned(&dir, "known.flac", "bottom.flac");
    // Pin the selected folder's own mtime; every `d` directory carries the wall clock, which
    // is later, so a refusal that folds the refused directory's mtime is observable.
    common::set_dir_time(&folder, common::file_time());

    let (store, db) = fresh_store("slice-depth", 2);
    seed_library(&store, 1, "slice-depth", &tree.root);
    let mapping = mapping();

    let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
    let (events, summary) = walk_selected(&tree.root, "album/one", true).await;

    assert!(
        summary.outcome.is_complete(),
        "the cap is a warning, not a failure: {}",
        summary.outcome.describe()
    );
    let walked = events
        .iter()
        .find(|event| event.snapshot.path.to_string() == "album/one")
        .expect("the walk emits the selected folder");

    assert_eq!(
        outcome.hash, walked.hash,
        "one shared gate means one fingerprint for a tree deeper than the cap"
    );
    assert_eq!(
        stored_hash(&store, &outcome.folder_id),
        Some(outcome.hash.clone()),
        "and that fingerprint is what the folder row stores"
    );

    let ingest_depth = kinds(&outcome.warnings, ScanWarningKind::DepthLimitReached);
    let walker_depth = kinds(
        &walked.snapshot.warnings,
        ScanWarningKind::DepthLimitReached,
    );
    assert!(
        walker_depth.is_empty(),
        "the selected folder descends into its child on the walker too; the refusal is deeper \
         in the chain, got {walker_depth:?}"
    );
    assert!(
        ingest_depth.is_empty(),
        "and the capture reports no depth refusal for the selected folder either: it lists one \
         directory whose child sits two live identities from the cap, got {ingest_depth:?}"
    );
    assert_eq!(
        outcome.hash, walked.hash,
        "so the two paths still fingerprint the selected folder identically even though the \
         walker refuses further down"
    );
    let refusals = summary.warnings_of(ScanWarningKind::DepthLimitReached);
    assert_eq!(
        refusals
            .iter()
            .map(|w| w.path.as_ref().map(|p| p.to_string()))
            .collect::<Vec<_>>(),
        vec![Some(logical.to_string())],
        "exactly one directory is refused by the walker, at the depth the cap implies; got \
         {refusals:?}"
    );

    // Descent stops at the refusal instead of refusing the subtree: the target plus the levels
    // above the refusal are still emitted, and the refused directory is readable-but-not-
    // descendable, so it contributes the walker's depth warning and its own facts — never
    // `DirectoryUnreadable`, which belongs to `isDirReadable` failing.
    assert_eq!(
        summary.folders_emitted,
        MAX_WALK_DEPTH - 1,
        "the target plus levels 1..=MAX_WALK_DEPTH-2 are emitted; the refused one is not"
    );
    assert_eq!(
        summary.folders_incomplete, 0,
        "a depth refusal is not an incomplete listing"
    );
    assert_eq!(
        walked.snapshot.num_sub_folders, 1,
        "the selected folder descended into its one child; deeper levels are not its children"
    );
    // The rejected child's facts land in its OWN parent's snapshot, which is the level just
    // above the refusal: `walk.rs`'s `EnterOutcome::Skipped` arm feeds `accumulate_entry`, so
    // the refused directory's `lstat` mtime raises that folder's `mod_time` and it is not
    // silently omitted. Same claim, one level down the chain.
    let refused = dir.clone();
    let refused_time = std::fs::metadata(&refused)
        .expect("stat the refused directory")
        .modified()
        .expect("refused directory mtime");
    let owner = events
        .iter()
        .find(|event| event.snapshot.path == logical.parent())
        .expect("the level just above the refusal is emitted");
    assert_eq!(
        owner.snapshot.mod_time, refused_time,
        "the refused directory's own mtime raised its parent's folder mtime"
    );
    assert_eq!(
        owner.snapshot.num_sub_folders, 0,
        "the refused directory is not counted as a subfolder of it"
    );
    assert!(
        owner.snapshot.warnings.is_empty(),
        "the walker attributes a refusal to the terminal summary, not to a folder snapshot, so \
         the owner's snapshot stays clean: {:?}",
        owner
            .snapshot
            .warnings
            .iter()
            .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
            .collect::<Vec<_>>()
    );
    assert!(
        kinds(&outcome.warnings, ScanWarningKind::DirectoryUnreadable).is_empty(),
        "a depth refusal is not an unreadable directory, so the ingest must not warn that way"
    );
    assert_eq!(
        outcome.num_sub_folders, walked.snapshot.num_sub_folders,
        "the capture and the walker count the selected folder's subfolders identically"
    );
    assert_eq!(
        walked.snapshot.coverage,
        FolderCoverage::Complete,
        "the refusal stops descent but excludes no entry"
    );
    // Go raises a folder's `modTime` only for NON-directory entries (`walk_dir_tree.go:157-163`),
    // so the entered child contributes nothing to the selected folder's mtime and its fingerprint
    // stays the pinned stamp: the chain's mtimes fold only into the folder that owns the refused
    // directory, asserted above. That is the whole asymmetry this test exists to pin.
    assert_eq!(
        walked.snapshot.mod_time,
        common::file_time(),
        "an entered child directory does not raise the selected folder's mtime"
    );
    assert!(
        owner.snapshot.mod_time > common::file_time(),
        "the refused directory's mtime does raise its owner's, so the two levels differ"
    );

    // Non-vacuity: the same snapshot with the owner's `mod_time` left at the pinned stamp must
    // hash differently, so the hash agreement above pins the accumulation rather than a
    // coincidence.
    assert_ne!(
        owner.hash,
        {
            let mut oracle = owner.snapshot.clone();
            oracle.mod_time = common::file_time();
            oracle.hash().expect("omission-shaped hash")
        },
        "non-vacuity: omitting the refused directory's mtime changes its parent's fingerprint"
    );

    assert!(pool_healthy(&store));
    remove_db(&db);
}

/// A non-UTF-8 entry name in the selected folder: the capture's name seam (`name_text`) excludes
/// it with `NameNotUtf8` and marks the snapshot `EntriesIncomplete` rather than hashing a lossy
/// identity, and the rest of the folder is captured and fingerprinted exactly like the SC-02
/// walker over the same scope (`walk.rs::representable_name`, Go's `resolveEntryName` refusing a
/// name it cannot store).
///
/// Platform split, stated rather than hidden: Linux (ext4/tmpfs) stores arbitrary byte sequences,
/// so the claim is executed end-to-end there. macOS/APFS rejects such a name with `EILSEQ`, so no
/// directory entry with a non-UTF-8 name can exist to be captured — this test asserts that
/// rejection instead of pretending to cover the arm, and the portable seam for the same behavior
/// is the lib-side `walk::tests::invalid_name_seam_marks_the_actual_owner_incomplete_on_every_platform`.
#[cfg(unix)]
#[tokio::test]
async fn a_non_utf8_entry_name_is_excluded_and_the_folder_still_matches_the_walker() {
    let bad = common::os_name(&[b'b', b'a', b'd', 0xff, b'.', b'm', b'p', b'3']);

    #[cfg(target_os = "macos")]
    {
        // The gap, asserted: APFS refuses the name, so the capture never sees such an entry on
        // this platform. A silent skip would leave this arm looking covered on macOS.
        let tree = Tree::new("slice-non-utf8-macos");
        let folder = tree.dir("album/one");
        let error = std::fs::write(folder.join(&bad), b"x")
            .expect_err("APFS rejects a non-UTF-8 name (EILSEQ)");
        assert_eq!(error.raw_os_error(), Some(92), "EILSEQ, got {error:?}");
        eprintln!(
            "COVERAGE: the non-UTF-8 capture arm cannot be driven end-to-end on macOS \
             (EILSEQ at create); executed on Linux, and the portable seam is \
             walk::tests::invalid_name_seam_marks_the_actual_owner_incomplete_on_every_platform"
        );
        return;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let tree = Tree::new("slice-non-utf8");
        let folder = tree.dir("album/one");
        common::copy_pinned(&folder, "known.mp3", "known.mp3");
        std::fs::write(folder.join(&bad), b"x").expect("write a raw-byte name (ext4/tmpfs)");
        common::set_dir_time(&folder, common::file_time());

        let (store, db) = fresh_store("slice-non-utf8", 2);
        seed_library(&store, 1, "slice-non-utf8", &tree.root);
        let mapping = mapping();

        let outcome = scan_ok(&store, &job(1, &tree.root, "album/one", stamp(), &mapping));
        let (events, summary) = walk_selected(&tree.root, "album/one", true).await;
        assert!(
            summary.outcome.is_complete(),
            "an excluded name is not a failure: {}",
            summary.outcome.describe()
        );
        let walked = events
            .iter()
            .find(|event| event.snapshot.path.to_string() == "album/one")
            .expect("the walk emits the selected folder");

        // One warning, naming the raw entry, on both paths.
        let names = kinds(&outcome.warnings, ScanWarningKind::NameNotUtf8);
        // Both paths report the entry's LOGICAL path, whose Display is lossy by design
        // (`storage.rs::LogicalPath::fmt`) — diagnostics only, never an identity.
        assert_eq!(
            names,
            vec![Some(format!("album/one/{}", bad.to_string_lossy()))],
            "the capture reports the unrepresentable name once, got {:?}",
            outcome
                .warnings
                .iter()
                .map(|w| (w.kind, w.path.as_ref().map(|p| p.to_string())))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            kinds(&summary.warnings, ScanWarningKind::NameNotUtf8),
            names,
            "the walker reports the same entry the same way"
        );

        // The exclusion is explicit, not silent: the snapshot says it is incomplete.
        assert_eq!(
            outcome
                .warnings
                .iter()
                .filter(|w| w.kind == ScanWarningKind::NameNotUtf8)
                .count(),
            1,
            "exactly one excluded name"
        );
        assert_eq!(
            walked.snapshot.coverage,
            FolderCoverage::EntriesIncomplete,
            "the walker's snapshot declares the exclusion"
        );

        // Headline parity: one fingerprint for one scope, two code paths, and the excluded name
        // contributes no hash line and no U+FFFD identity.
        assert_eq!(
            outcome.hash, walked.hash,
            "the capture and the walker fingerprint a folder with an excluded name identically"
        );
        assert_eq!(
            stored_hash(&store, &outcome.folder_id),
            Some(outcome.hash.clone()),
            "and that is the fingerprint stored"
        );
        let hashed_names: Vec<&str> = walked
            .snapshot
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            hashed_names,
            vec!["known.mp3"],
            "no lossy identity enters the fingerprint"
        );
        assert!(
            !hashed_names.iter().any(|name| name.contains('\u{fffd}')),
            "a lossy render would collide with real names: {hashed_names:?}"
        );

        // The folder survives and is imported: the exclusion costs one entry, not the folder.
        assert_eq!(
            outcome.audio_count, 1,
            "the representable file is the audio"
        );
        assert_eq!(outcome.new_media_count, 1);
        assert_eq!(
            children(&store, &outcome.folder_id)
                .iter()
                .map(|kid| kid.path.as_str())
                .collect::<Vec<_>>(),
            vec!["album/one/known.mp3"],
            "the raw-named entry stores no media row under a fabricated path"
        );
        assert_eq!(
            walked.snapshot.num_sub_folders, outcome.num_sub_folders,
            "the excluded entry is not a subfolder on either path"
        );
        assert_eq!(
            summary.folders_emitted, 1,
            "the folder is emitted despite the exclusion"
        );
        assert_eq!(summary.folders_incomplete, 0, "excluded != incomplete walk");

        assert!(pool_healthy(&store));
        remove_db(&db);
    }
}
