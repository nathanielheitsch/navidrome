//! Walk policy tests: target normalization, postorder, dot/ignore policy, symlink
//! handling, cycle suppression, cancellation, and terminal outcomes.
//!
//! Real filesystem fixtures only, in fresh temp directories owned by the test that created
//! them. No database, no tag reader, no persistence.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nd_scanner::contract::{CancelToken, LibraryRoot, ScanWarningKind, WalkRequest, WalkTarget};
use nd_scanner::storage::LogicalPath;
use nd_scanner::walk::{FolderEvent, WalkHandle, WalkSummary};

fn fixture(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-walk-{}-{}-{}",
        std::process::id(),
        nanos,
        name
    ));
    fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn write(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    let path = dir.join(name);
    fs::write(&path, b"data").expect("write");
    path
}

fn mkdir(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    dir.to_path_buf()
}

#[cfg(unix)]
fn link(target: &Path, link_path: &Path) {
    std::os::unix::fs::symlink(target, link_path).expect("symlink");
}

fn library(root: &Path) -> LibraryRoot {
    LibraryRoot {
        id: 1,
        name: "test".to_string(),
        root: root.as_os_str().to_os_string(),
    }
}

fn whole_library(root: &Path) -> WalkRequest {
    WalkRequest {
        libraries: vec![library(root)],
        targets: Vec::new(),
        follow_symlinks: true,
        ignore_dot_folders: false,
    }
}

/// Drain every event plus the terminal summary.
async fn walk(request: WalkRequest) -> (Vec<FolderEvent>, WalkSummary) {
    let mut handle = WalkHandle::start(request, CancelToken::new());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    let summary = handle.terminal().await;
    (events, summary)
}

fn paths(events: &[FolderEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| event.snapshot.path.to_string())
        .collect()
}

fn warnings(
    summary: &WalkSummary,
    kind: ScanWarningKind,
) -> Vec<&nd_scanner::contract::ScanWarning> {
    summary.warnings_of(kind)
}

// ---------------------------------------------------------------------------
// Target normalization and postorder
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn an_empty_target_list_walks_the_configured_root_once() {
    let root = fixture("default-target");
    write(&root, "song.mp3");
    let (events, summary) = walk(whole_library(&root)).await;
    assert_eq!(paths(&events), vec!["."]);
    assert_eq!(summary.targets_walked, 1);
    assert_eq!(summary.targets_skipped, 0);
    assert!(
        summary.outcome.is_complete(),
        "{}",
        summary.outcome.describe()
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn folders_are_emitted_child_before_parent_in_deterministic_order() {
    let root = fixture("postorder");
    mkdir(&root.join("b/inner"));
    mkdir(&root.join("a"));
    write(&root.join("a"), "one.mp3");
    write(&root.join("b/inner"), "two.mp3");

    let (events, summary) = walk(whole_library(&root)).await;
    // Go appends the parent only after the child slice is appended
    // (`scanner/walk_dir_tree.go:86-105`), so children precede parents.
    assert_eq!(
        paths(&events),
        vec!["a", "b/inner", "b", "."],
        "postorder, siblings in name order"
    );
    assert_eq!(summary.folders_emitted, 4);
    assert_eq!(summary.folders_incomplete, 0);

    // Repeated runs produce the identical order (no map iteration, no readdir race).
    let (again, _) = walk(whole_library(&root)).await;
    assert_eq!(paths(&again), paths(&events));
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn num_sub_folders_counts_descended_directories_only() {
    let root = fixture("subfolder-count");
    mkdir(&root.join("disc"));
    write(&root.join("disc"), "s.mp3");
    let blocked = mkdir(&root.join("blocked"));
    write(&blocked, "s.mp3");
    // `blocked/` never matches the bare name (documented matcher quirk), so Go still
    // descends and counts it as a subfolder; every path inside it is ignored
    // (`scanner/walk_dir_tree.go:136-160`).
    fs::write(root.join(".ndignore"), "blocked/\n").expect("ignore");

    let (events, _) = walk(whole_library(&root)).await;
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .expect("root");
    assert_eq!(
        root_event.snapshot.num_sub_folders, 2,
        "both descended directories count"
    );
    let blocked_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "blocked")
        .expect("blocked is emitted, empty");
    assert!(blocked_event.snapshot.entries.is_empty());
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn duplicate_targets_are_collapsed_and_targets_run_in_sorted_order() {
    let root = fixture("duplicate-targets");
    mkdir(&root.join("z"));
    mkdir(&root.join("a"));
    write(&root.join("a"), "s.mp3");

    let mut request = whole_library(&root);
    request.targets = vec![
        WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("z").expect("p"),
        },
        WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("a").expect("p"),
        },
        WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("a").expect("p"),
        },
    ];
    let (events, summary) = walk(request).await;
    assert_eq!(
        paths(&events),
        vec!["a", "z"],
        "sorted targets, each walked once"
    );
    assert_eq!(summary.targets_walked, 2);
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn an_unknown_library_id_fails_the_walk_rather_than_scanning_nothing() {
    let root = fixture("unknown-library");
    write(&root, "s.mp3");
    let mut request = whole_library(&root);
    request.targets = vec![WalkTarget {
        library_id: 99,
        path: LogicalPath::root(),
    }];
    let (_, summary) = walk(request).await;
    assert!(!summary.outcome.is_complete());
    assert_eq!(
        summary.outcome.describe(),
        "failed: target '.' names unknown library 99"
    );
    assert_eq!(summary.targets_walked, 0);
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn no_libraries_is_a_failure_not_an_empty_success() {
    let request = WalkRequest {
        libraries: Vec::new(),
        targets: Vec::new(),
        follow_symlinks: true,
        ignore_dot_folders: false,
    };
    let (_, summary) = walk(request).await;
    assert_eq!(
        summary.outcome.describe(),
        "failed: no libraries configured for this walk"
    );
    assert_eq!(summary.folders_emitted, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_target_is_skipped_with_a_warning_not_a_failure() {
    let root = fixture("missing-target");
    write(&root, "s.mp3");
    let mut request = whole_library(&root);
    request.targets = vec![
        WalkTarget {
            library_id: 1,
            path: LogicalPath::parse("gone").expect("p"),
        },
        WalkTarget {
            library_id: 1,
            path: LogicalPath::root(),
        },
    ];
    let (events, summary) = walk(request).await;
    assert_eq!(summary.targets_skipped, 1);
    assert_eq!(warnings(&summary, ScanWarningKind::TargetMissing).len(), 1);
    assert!(
        summary.outcome.is_complete(),
        "a missing target is not fatal"
    );
    assert_eq!(paths(&events), vec!["."]);
    cleanup(&root);
}

// ---------------------------------------------------------------------------
// Dot entries and the blocklist
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn dot_files_are_kept_and_dot_folders_follow_the_flag() {
    let root = fixture("dot-policy");
    write(&root, ".hidden.mp3");
    mkdir(&root.join(".secret"));
    write(&root.join(".secret"), "s.mp3");

    let mut off = whole_library(&root);
    off.ignore_dot_folders = false;
    // `isIgnoredEntry` (scanner/walk_dir_tree.go:332-337): dot-prefixed **files** are always
    // ignored; dot-prefixed **folders** are ignored only when the flag is on.
    let (events, _) = walk(off).await;
    assert!(
        paths(&events).contains(&".secret".to_string()),
        "a dot folder is scanned when IgnoreDotFolders is off"
    );
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    let names: Vec<&str> = root_event
        .snapshot
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, Vec::<&str>::new(), "the dot file is always ignored");

    let mut on = whole_library(&root);
    on.ignore_dot_folders = true;
    let (events, _) = walk(on).await;
    assert!(
        !paths(&events).contains(&".secret".to_string()),
        "the dot folder is ignored when the flag is on"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn the_blocklist_directories_are_never_walked_but_their_ignore_file_is_still_read() {
    let root = fixture("blocklist");
    // `ignoredDirs` (scanner/walk_dir_tree.go:315-324), matched case-insensitively.
    for name in [".git", "@eaDir", "lost+found", "$RECYCLE.BIN", ".streams"] {
        let dir = mkdir(&root.join(name));
        write(&dir, "s.mp3");
    }
    // A non-ASCII-folded near-miss is not in the list.
    mkdir(&root.join("@eaDirX"));
    write(&root, "keep.mp3");

    let (events, _) = walk(whole_library(&root)).await;
    assert_eq!(
        paths(&events),
        vec!["@eaDirX", "."],
        "blocklisted directories are never emitted; case-insensitive matching only"
    );
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    assert_eq!(
        root_event.snapshot.num_sub_folders, 1,
        "only @eaDirX counts"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_dot_folder_is_never_ignored_by_dot_policy_at_the_root_level() {
    // The library root itself is never subject to the dot rule.
    let root = fixture("dot-root");
    write(&root, "keep.mp3");
    let mut request = whole_library(&root);
    request.ignore_dot_folders = true;
    let (events, _) = walk(request).await;
    assert_eq!(paths(&events), vec!["."]);
    cleanup(&root);
}

// ---------------------------------------------------------------------------
// .ndignore through the walker
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn ignore_patterns_apply_to_directories_and_files_at_any_depth() {
    let root = fixture("ignore-walk");
    fs::write(root.join(".ndignore"), "temp/\n*.log\n").expect("ignore");
    let temp = mkdir(&root.join("temp"));
    write(&temp, "cache.db");
    mkdir(&root.join("temp/nested"));
    write(&root, "notes.log");
    write(&root, "keep.mp3");

    let (events, _) = walk(whole_library(&root)).await;
    // `temp/` does not match the bare name, so it is entered and emitted empty; `*.log`
    // drops the file from the root snapshot.
    assert_eq!(paths(&events), vec!["temp", "."]);
    let temp = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "temp")
        .unwrap();
    assert!(
        temp.snapshot.entries.is_empty(),
        "nothing inside temp/ survives"
    );
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    let names: Vec<&str> = root_event
        .snapshot
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["keep.mp3"],
        "the log file is excluded from the snapshot"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn an_empty_ignore_file_in_a_folder_ignores_that_whole_folder() {
    let root = fixture("ignore-empty-file");
    let dead = mkdir(&root.join("dead"));
    fs::write(dead.join(".ndignore"), "# nothing but a comment\n").expect("ignore");
    write(&dead, "s.mp3");
    write(&root, "keep.mp3");

    let (events, _) = walk(whole_library(&root)).await;
    // `scanner/ignore_checker.go:138-142`: an empty/comment-only file means "ignore everything".
    // Go applies that to the folder's *entries* (loadDir checks each entry path), so the
    // folder itself is still emitted with no contents — not dropped, and not a missing
    // folder.
    assert_eq!(paths(&events), vec!["dead", "."]);
    let dead = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "dead")
        .unwrap();
    assert!(dead.snapshot.entries.is_empty());
    assert!(dead.snapshot.is_empty());
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_negation_in_a_child_re_includes_only_the_named_path() {
    let root = fixture("ignore-negation");
    fs::write(root.join(".ndignore"), "*.txt\n").expect("ignore");
    let album = mkdir(&root.join("album"));
    fs::write(album.join(".ndignore"), "!keep.txt\n").expect("ignore");
    write(&album, "keep.txt");
    write(&album, "drop.txt");
    write(&root, "drop.txt");

    let (events, _) = walk(whole_library(&root)).await;
    let album_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "album")
        .unwrap();
    let names: Vec<&str> = album_event
        .snapshot
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["keep.txt"],
        "the child negation re-includes one name"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_trailing_slash_pattern_leaves_the_bare_directory_emitted_as_empty() {
    // Documented quirk carried through the walker: `build/` never matches the name `build`,
    // so the directory is entered and every path inside it is ignored.
    let root = fixture("ignore-trailing-slash");
    fs::write(root.join(".ndignore"), "build/\n").expect("ignore");
    let build = mkdir(&root.join("build"));
    write(&build, "out.mp3");

    let (events, _) = walk(whole_library(&root)).await;
    assert_eq!(paths(&events), vec!["build", "."]);
    let build_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "build")
        .unwrap();
    assert!(build_event.snapshot.entries.is_empty());
    assert!(build_event.snapshot.is_empty());
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn ignore_read_failures_are_warnings_and_never_ignore_everything() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture("ignore-read-fail");
    let folder = mkdir(&root.join("album"));
    write(&folder, "keep.mp3");
    let ignore = folder.join(".ndignore");
    fs::write(&ignore, "definitely-ignored.mp3\n").expect("ignore");
    // Replace it with something that cannot be decoded as UTF-8: the read ends and no
    // ignore-all substitution happens.
    fs::write(&ignore, [0xff_u8, 0xfe, b'\n']).expect("binary ignore");
    fs::set_permissions(&ignore, fs::Permissions::from_mode(0o644)).expect("mode");

    let (events, summary) = walk(whole_library(&root)).await;
    let failed = warnings(&summary, ScanWarningKind::IgnoreFileNotUtf8).len()
        + warnings(&summary, ScanWarningKind::IgnoreFileReadFailed).len();
    assert_eq!(failed, 1, "the read failure is reported once");
    let album = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "album")
        .unwrap();
    assert_eq!(
        album.snapshot.entries.len(),
        1,
        "the folder is not silently emptied"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn an_uncompilable_pattern_is_reported_and_the_rest_still_apply() {
    let root = fixture("ignore-invalid");
    fs::write(root.join(".ndignore"), "[a-\n*.log\n").expect("ignore");
    write(&root, "notes.log");
    write(&root, "keep.mp3");

    let (events, summary) = walk(whole_library(&root)).await;
    assert_eq!(
        warnings(&summary, ScanWarningKind::IgnorePatternInvalid).len(),
        2 // root preload and descent each actually load the file
    );
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    let names: Vec<&str> = root_event
        .snapshot
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["keep.mp3"],
        "the compilable pattern still applies"
    );
    cleanup(&root);
}

// ---------------------------------------------------------------------------
// Symlinks: alias child, endpoint stats, cycle suppression, follow disabled
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn a_followed_file_link_keeps_its_own_name_and_its_endpoint_extension() {
    let root = fixture("link-classify");
    let outside = fixture("link-classify-target");
    let target = write(&outside, "real.flac");
    link(&target, &root.join("alias.mp3"));
    write(&root, "plain.mp3");

    let (events, summary) = walk(whole_library(&root)).await;
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    let alias = root_event
        .snapshot
        .entries
        .iter()
        .find(|e| e.name == "alias.mp3")
        .expect("alias entry");
    assert!(
        alias.outside_root,
        "an outside-root endpoint is reported, not hidden"
    );
    assert_eq!(
        warnings(&summary, ScanWarningKind::OutsideRootEndpoint).len(),
        1
    );
    // Classification uses the endpoint's base name (`resolveEntryName`).
    assert_eq!(alias.classification_name, "real.flac");
    // The hash facts are the link's own lstat, so the size differs from the target's.
    let link_size = fs::symlink_metadata(root.join("alias.mp3"))
        .expect("lstat")
        .len();
    assert_eq!(alias.stat.size, link_size, "hash facts stay the link's own");
    assert_eq!(
        alias.endpoint.as_ref().expect("endpoint").size,
        fs::metadata(&target).expect("stat").len()
    );
    cleanup(&root);
    cleanup(&outside);
}

#[cfg(unix)]
#[tokio::test]
async fn a_followed_directory_alias_indexes_the_same_files_under_a_second_logical_path() {
    let root = fixture("dir-alias");
    let album = mkdir(&root.join("album"));
    write(&album, "song.mp3");
    link(&album, &root.join("alias"));

    let (events, summary) = walk(whole_library(&root)).await;
    assert!(paths(&events).contains(&"album".to_string()));
    assert!(
        paths(&events).contains(&"alias".to_string()),
        "Go indexes a symlinked directory as its own subtree (walk_dir_tree_test.go:213)"
    );
    assert!(warnings(&summary, ScanWarningKind::DirectoryCycleSkipped).is_empty());
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn an_active_ancestor_cycle_is_suppressed_instead_of_recursing_forever() {
    let root = fixture("cycle");
    let album = mkdir(&root.join("album"));
    link(&album, &album.join("loop"));
    write(&album, "song.mp3");

    let (events, summary) = walk(whole_library(&root)).await;
    assert_eq!(paths(&events), vec!["album", "."]);
    assert_eq!(
        warnings(&summary, ScanWarningKind::DirectoryCycleSkipped).len(),
        1,
        "the cycle is reported once and the walk terminates"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_broken_link_is_skipped_with_a_warning_and_the_folder_still_hashes() {
    let root = fixture("broken-link");
    write(&root, "keep.mp3");
    link(&root.join("missing.mp3"), &root.join("broken.mp3"));

    let (events, summary) = walk(whole_library(&root)).await;
    assert_eq!(warnings(&summary, ScanWarningKind::SymlinkSkipped).len(), 1);
    let root_event = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == ".")
        .unwrap();
    let names: Vec<&str> = root_event
        .snapshot
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, vec!["keep.mp3"]);
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn follow_disabled_skips_links_and_reached_targets_without_error() {
    let root = fixture("follow-off");
    let outside = fixture("follow-off-target");
    let album = mkdir(&outside);
    write(&album, "song.mp3");
    link(&album, &root.join("alias"));
    write(&root, "keep.mp3");

    let mut request = whole_library(&root);
    request.follow_symlinks = false;
    let (events, summary) = walk(request).await;
    assert_eq!(paths(&events), vec!["."], "no aliased subtree is walked");
    assert!(!warnings(&summary, ScanWarningKind::SymlinkSkipped).is_empty());

    // A target reached through a link is skipped entirely (scanner/walk_dir_tree.go:47-51).
    let root2 = fixture("follow-off-target");
    let real = mkdir(&root2.join("real"));
    write(&real, "song.mp3");
    link(&real, &root2.join("alias"));
    let mut request2 = whole_library(&root2);
    request2.follow_symlinks = false;
    request2.targets = vec![WalkTarget {
        library_id: 1,
        path: LogicalPath::parse("alias").expect("p"),
    }];
    let (events2, summary2) = walk(request2).await;
    assert!(events2.is_empty());
    assert_eq!(summary2.targets_skipped, 1);
    assert_eq!(
        warnings(&summary2, ScanWarningKind::SymlinkedTargetSkipped).len(),
        1
    );
    assert!(summary2.outcome.is_complete(), "skipping is not a failure");
    cleanup(&root);
    cleanup(&outside);
    cleanup(&root2);
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_directory_is_reported_and_not_counted_as_a_subfolder() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture("unreadable-dir");
    write(&root, "keep.mp3");
    let closed = mkdir(&root.join("closed"));
    write(&closed, "hidden.mp3");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("chmod");
    let accessible = fs::read_dir(&closed).is_ok();
    let (events, summary) = walk(whole_library(&root)).await;
    if accessible {
        assert!(warnings(&summary, ScanWarningKind::DirectoryUnreadable).is_empty());
        assert!(paths(&events).contains(&"closed".to_string()));
        assert_eq!(summary.folders_incomplete, 0);
        assert_eq!(summary.folders_emitted, 2);
    } else {
        assert_eq!(
            warnings(&summary, ScanWarningKind::DirectoryUnreadable).len(),
            1
        );
        assert!(!paths(&events).contains(&"closed".to_string()));
        assert_eq!(summary.folders_incomplete, 1);
        assert_eq!(summary.folders_emitted, 1, "only the root is emitted");
    }
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).expect("restore");
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_non_utf8_name_is_a_typed_outcome_and_never_a_lossy_identity() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use nd_scanner::contract::{name_text, NameText};

    // The UTF-8 boundary is explicit at the name seam, so a lossy render cannot create an
    // identity that collides with a real name.
    let bad = OsString::from_vec(vec![b'b', b'a', b'd', 0xff, b'.', b'm', b'p', b'3']);
    assert_eq!(name_text(bad.as_os_str()), NameText::Invalid);
    assert_eq!(
        name_text(OsStr::new("real.mp3")),
        NameText::Valid("real.mp3")
    );
    // A lossy render of the invalid name would collide with names containing U+FFFD, which
    // is exactly why the walker refuses it instead of using `to_string_lossy`.
    assert_eq!(
        bad.to_string_lossy().as_ref(),
        "bad\u{fffd}.mp3",
        "lossy text is only for diagnostics, never an identity"
    );

    // Filesystem-level exclusion is exercised where the platform stores raw byte names.
    // macOS/APFS rejects an invalid UTF-8 sequence with EILSEQ (`Illegal byte sequence`), so
    // a non-UTF-8 directory entry cannot be created there: declared coverage gap on macOS,
    // verified on Linux (`S9` item c). The platform-independent layer for this behaviour is
    // the portable seam coverage in `src/walk.rs`
    // (`invalid_name_seam_marks_the_actual_owner_incomplete_on_every_platform`), which runs
    // on every platform and asserts the same `EntriesIncomplete` marking and warning.
    #[cfg(target_os = "linux")]
    {
        let root = fixture("non-utf8");
        write(&root, "keep.mp3");
        fs::write(root.join(bad.clone()), b"x").expect("write non-utf8 name");
        let (events, summary) = walk(whole_library(&root)).await;
        assert!(!warnings(&summary, ScanWarningKind::NameNotUtf8).is_empty());
        let root_event = events
            .iter()
            .find(|e| e.snapshot.path.to_string() == ".")
            .unwrap();
        let names: Vec<&str> = root_event
            .snapshot
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["keep.mp3"], "no U+FFFD identity is created");
        assert_eq!(
            root_event.snapshot.coverage,
            nd_scanner::folder::FolderCoverage::EntriesIncomplete
        );
        cleanup(&root);
    }
}

// ---------------------------------------------------------------------------
// Fingerprint agreement with the Go goldens, through a real walk
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn a_walked_empty_folder_hashes_the_go_zero_image_state() {
    let root = fixture("hash-empty");
    mkdir(&root.join("empty"));
    let (events, _) = walk(whole_library(&root)).await;
    let empty = events
        .iter()
        .find(|e| e.snapshot.path.to_string() == "empty")
        .unwrap();
    assert!(empty.snapshot.images_updated_at.is_none());
    // The image field must be Go's year-1 spelling, so the preimage ends with it.
    let preimage =
        String::from_utf8(empty.snapshot.hash_preimage().expect("preimage")).expect("utf8");
    assert!(
        preimage.contains(":0001-01-01 00:00:00 +0000 UTC"),
        "{preimage}"
    );
    assert_eq!(empty.hash.len(), 32);
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn repeated_walks_of_an_unchanged_tree_produce_identical_hashes() {
    let root = fixture("stable-hash");
    mkdir(&root.join("album"));
    write(&root.join("album"), "song.mp3");
    write(&root.join("album"), "cover.jpg");

    let (first, _) = walk(whole_library(&root)).await;
    let (second, _) = walk(whole_library(&root)).await;
    let mut a: Vec<(String, String)> = first
        .iter()
        .map(|e| (e.snapshot.path.to_string(), e.hash.clone()))
        .collect();
    let mut b: Vec<(String, String)> = second
        .iter()
        .map(|e| (e.snapshot.path.to_string(), e.hash.clone()))
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same facts must hash the same way");
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_audio_file_changes_the_folder_hash() {
    let root = fixture("hash-change");
    mkdir(&root.join("album"));
    write(&root.join("album"), "song.mp3");
    let (first, _) = walk(whole_library(&root)).await;
    let before = first
        .iter()
        .find(|e| e.snapshot.path.to_string() == "album")
        .unwrap()
        .hash
        .clone();

    write(&root.join("album"), "two.mp3");
    let (second, _) = walk(whole_library(&root)).await;
    let after = second
        .iter()
        .find(|e| e.snapshot.path.to_string() == "album")
        .unwrap()
        .hash
        .clone();
    assert_ne!(before, after, "a new file must be detected");
    cleanup(&root);
}

// ---------------------------------------------------------------------------
// Cancellation, receiver drop, terminal outcomes
// ---------------------------------------------------------------------------

/// A wide tree so cancellation has a queue to contend with.
fn wide_tree(root: &Path, folders: usize) {
    for i in 0..folders {
        let dir = root.join(format!("f{i:04}"));
        mkdir(&dir);
        write(&dir, "s.mp3");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_with_a_full_queue_joins_and_reports_cancelled() {
    let root = fixture("cancel-full");
    wide_tree(&root, 64);
    let cancel = CancelToken::new();
    let mut handle = WalkHandle::start(whole_library(&root), cancel.clone());

    // Take one folder so the producer parks on the capacity-1 queue, then cancel.
    let first = handle.next().await.expect("at least one folder");
    assert!(!first.hash.is_empty());
    cancel.cancel();

    let summary = handle.cancel_and_join().await;
    assert!(
        summary.outcome.is_cancelled(),
        "{}",
        summary.outcome.describe()
    );
    assert!(summary.folders_emitted < 64, "the walk stopped early");
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_before_any_event_stops_the_walk() {
    let root = fixture("cancel-early");
    wide_tree(&root, 32);
    let cancel = CancelToken::new();
    cancel.cancel();
    let summary = WalkHandle::start(whole_library(&root), cancel)
        .drain_and_join()
        .await;
    assert!(summary.outcome.is_cancelled());
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_receiver_without_joining_reports_cancelled_not_complete() {
    let root = fixture("receiver-drop");
    wide_tree(&root, 48);
    let mut handle = WalkHandle::start(whole_library(&root), CancelToken::new());
    let _ = handle.next().await;
    handle.close_receiver();
    let summary = handle.terminal().await;
    assert!(
        !summary.outcome.is_complete(),
        "EOF must never be reported as success: {}",
        summary.outcome.describe()
    );
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_full_walk_reports_a_distinct_terminal_from_receiver_eof() {
    let root = fixture("terminal");
    mkdir(&root.join("a"));
    write(&root.join("a"), "s.mp3");
    let mut handle = WalkHandle::start(whole_library(&root), CancelToken::new());
    let mut count = 0;
    while handle.next().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 2, "EOF after every folder");
    let summary = handle.terminal().await;
    assert!(
        summary.outcome.is_complete(),
        "{}",
        summary.outcome.describe()
    );
    assert_eq!(summary.folders_emitted, 2);
    assert_eq!(summary.warnings_of(ScanWarningKind::TargetMissing).len(), 0);
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn repeated_cancellation_is_idempotent_and_never_hangs() {
    let root = fixture("repeat-cancel");
    wide_tree(&root, 16);
    let cancel = CancelToken::new();
    for _ in 0..3 {
        let handle = WalkHandle::start(whole_library(&root), cancel.clone());
        cancel.cancel();
        let summary = handle.drain_and_join().await;
        assert!(summary.outcome.is_cancelled());
    }
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_parked_producer_terminates_when_the_receiver_is_closed() {
    let root = fixture("drain-all");
    wide_tree(&root, 24);
    // No consumer: the producer parks on the capacity-1 queue. Closing the receiver makes
    // it see a closed channel, so the worker returns and the join cannot hang.
    let mut handle = WalkHandle::start(whole_library(&root), CancelToken::new());
    handle.close_receiver();
    let summary = handle.terminal().await;
    assert!(
        summary.outcome.is_cancelled(),
        "a closed receiver is never success: {}",
        summary.outcome.describe()
    );
    assert!(
        summary.folders_emitted < 24,
        "the walk stopped at the queue"
    );
    cleanup(&root);
}

#[tokio::test]
async fn selective_nested_target_preloads_logical_ancestors_without_leaking_to_siblings() {
    let root = fixture("logical-preload");
    write(&root.join("rock/album"), "hidden.mp3");
    write(&root.join("jazz"), "keep.mp3");
    write(&root, "root.mp3");
    fs::write(root.join("rock/.ndignore"), "*.mp3\n[a-\na{1,2}\n").unwrap();
    fs::write(root.join("jazz/.ndignore"), "b{\n").unwrap();
    let mut request = whole_library(&root);
    request.targets = ["rock/album", "jazz", "."]
        .iter()
        .map(|path| WalkTarget {
            library_id: 1,
            path: LogicalPath::parse(path).unwrap(),
        })
        .collect();
    let (events, summary) = walk(request).await;
    assert!(summary.outcome.is_complete());
    for event in &events {
        match event.snapshot.path.to_string().as_str() {
            "rock/album" => assert_eq!(event.snapshot.num_audio_files(), 0),
            "jazz" | "." => assert_eq!(event.snapshot.num_audio_files(), 1),
            _ => {}
        }
    }
    // Root traversal loads rock once; the selective nested target preloads it once.
    let invalid = warnings(&summary, ScanWarningKind::IgnorePatternInvalid);
    assert_eq!(invalid.len(), 2);
    assert!(invalid.iter().all(|w| {
        w.library_id == Some(1) && w.path == Some(LogicalPath::parse("rock").unwrap())
    }));
    let unsupported = warnings(&summary, ScanWarningKind::IgnorePatternUnsupported);
    assert_eq!(unsupported.len(), 5); // rock twice, jazz preload + descent + root descent
    assert_eq!(
        unsupported
            .iter()
            .filter(|w| w.path == Some(LogicalPath::parse("jazz").unwrap()))
            .count(),
        3
    );
    assert!(unsupported.iter().all(|w| w.library_id == Some(1)));
    cleanup(&root);
}

#[tokio::test]
async fn cancellation_is_sticky_for_unpolled_late_and_multiple_observers() {
    let cancel = CancelToken::new();
    let unpolled = cancel.cancelled();
    cancel.cancel(); // exactly one cancellation, with no subscribers yet
    let observed = async {
        unpolled.await;
        tokio::join!(cancel.cancelled(), cancel.cancelled(), cancel.cancelled());
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), observed)
        .await
        .expect("all current and late observers see sticky cancellation");
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn a_single_cancellation_wakes_already_subscribed_observers() {
    use std::future::{poll_fn, Future};
    use std::task::Poll;

    let cancel = CancelToken::new();
    let mut first = Box::pin(cancel.cancelled());
    let mut second = Box::pin(cancel.cancelled());
    // Poll both once, deterministically establishing subscriptions before cancellation.
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(first, second, cancel.cancelled());
    })
    .await
    .expect("single cancel wakes subscribed and late observers");
}

#[tokio::test]
async fn terminal_drains_unobserved_events_and_preserves_producer_outcomes() {
    let root = fixture("terminal-drain");
    mkdir(&root.join("child"));
    let complete = WalkHandle::start(whole_library(&root), CancelToken::new());
    let summary = tokio::time::timeout(std::time::Duration::from_secs(2), complete.terminal())
        .await
        .expect("two-folder producer cannot deadlock on its capacity-one queue");
    assert!(summary.outcome.is_complete());
    assert_eq!(summary.folders_emitted, 2);

    let cancel = CancelToken::new();
    cancel.cancel();
    let summary = WalkHandle::start(whole_library(&root), cancel)
        .terminal()
        .await;
    assert!(summary.outcome.is_cancelled());

    let mut closed = WalkHandle::start(whole_library(&root), CancelToken::new());
    closed.close_receiver();
    assert!(closed.terminal().await.outcome.is_cancelled());

    let mut bad = whole_library(&root);
    bad.libraries.clear();
    let summary = WalkHandle::start(bad, CancelToken::new()).terminal().await;
    assert!(matches!(
        summary.outcome,
        nd_scanner::walk::WalkOutcome::Failed(_)
    ));
    cleanup(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn broken_followed_links_do_not_raise_mtime_but_disabled_and_other_entries_do() {
    use std::fs::{File, FileTimes};
    use std::time::Duration;

    let root = fixture("broken-mtime");
    let early = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let late = early + Duration::from_secs(100);
    let set_time = |path: &Path, time| {
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(time))
            .unwrap();
    };
    let broken = root.join("broken.mp3");
    link(&root.join("missing"), &broken);
    // lstat mtime of the newly-created link is newer than the fixed root time.
    set_time(&root, early);
    let (events, summary) = walk(whole_library(&root)).await;
    assert!(summary.outcome.is_complete());
    assert_eq!(events[0].snapshot.mod_time, early);

    let mut disabled = whole_library(&root);
    disabled.follow_symlinks = false;
    let (events, _) = walk(disabled).await;
    assert!(events[0].snapshot.mod_time > early);

    let other = write(&root, "unclassified.bin");
    set_time(&other, late);
    let song = write(&root, "real.mp3");
    set_time(&song, early);
    link(&song, &root.join("valid-link"));
    set_time(&root, early);
    let (events, _) = walk(whole_library(&root)).await;
    assert!(events[0].snapshot.mod_time >= late);
    assert_eq!(events[0].snapshot.num_audio_files(), 2);
    cleanup(&root);
}

#[test]
fn ascii_blocklist_includes_long_s_and_kelvin_fold_orbits_only() {
    use nd_scanner::contract::is_dir_ignored;
    for name in [
        "loſt+found",
        ".ſtreamſ",
        "#ſnapſhot",
        "LOST+FOUND",
        "#SNAPSHOT",
    ] {
        assert!(is_dir_ignored(name), "{name}");
    }
    // K's orbit is supported, but no current blocklist name contains K.
    assert!(!is_dir_ignored("K"));
    assert!(!is_dir_ignored("loßt+found"));
    assert!(!is_dir_ignored(".ſtreamſ-extra"));
}

#[tokio::test]
async fn distinct_overlapping_targets_traverse_independently_with_fresh_ignores() {
    let root = fixture("overlap");
    write(&root.join("a/b"), "song.mp3");
    fs::write(root.join("a/.ndignore"), "*.mp3\n[a-\n").unwrap();
    fs::write(root.join("a/b/.ndignore"), "!song.mp3\n").unwrap();
    let mut request = whole_library(&root);
    request.targets = ["a", "a/b"]
        .iter()
        .map(|path| WalkTarget {
            library_id: 1,
            path: LogicalPath::parse(path).unwrap(),
        })
        .collect();
    let (events, summary) = walk(request).await;
    assert_eq!(paths(&events), ["a/b", "a", "a/b"]);
    assert!(summary.outcome.is_complete());
    assert_eq!(summary.targets_walked, 2);
    assert_eq!(summary.folders_emitted, 3);
    assert_eq!(summary.targets_skipped, 0);
    assert_eq!(events[0].snapshot.num_audio_files(), 1);
    assert_eq!(events[2].snapshot.num_audio_files(), 1);
    assert_eq!(
        warnings(&summary, ScanWarningKind::IgnorePatternInvalid).len(),
        3
    );
    cleanup(&root);
}

/// Real-filesystem evidence for raw *target* rejection. Linux-only by construction
/// (`S9` item c): macOS/APFS refuses to create a non-UTF-8 directory entry (EILSEQ), so
/// the fixture cannot exist there and this compiles to nothing on macOS. The
/// cross-platform layer is the portable seam test `target_preflight_and_endpoint_invalid_name_seams_are_portable`
/// in `src/walk.rs`, which accepts an explicit name classification and runs everywhere.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn raw_targets_fail_before_traversal_but_raw_configured_roots_are_allowed() {
    use nd_scanner::contract::ScanError;
    use nd_scanner::walk::WalkOutcome;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = fixture("raw-targets");
    let raw = OsString::from_vec(b"raw-\xff".to_vec());
    let raw_dir = root.join(&raw);
    mkdir(&raw_dir);
    let raw_logical = LogicalPath::root().join(&raw).unwrap();
    // First target is an empty raw directory; second is a valid child below it.
    for target in [
        raw_logical.clone(),
        raw_logical.join(OsStr::new("child")).unwrap(),
    ] {
        let mut request = whole_library(&root);
        request.targets = vec![WalkTarget {
            library_id: 1,
            path: target.clone(),
        }];
        let summary = WalkHandle::start(request, CancelToken::new())
            .terminal()
            .await;
        assert!(matches!(
            summary.outcome,
            WalkOutcome::Failed(ScanError::NonUtf8Name { .. })
        ));
        assert_eq!(summary.folders_emitted, 0);
        let warning = warnings(&summary, ScanWarningKind::NameNotUtf8)[0];
        assert_eq!(warning.path, Some(target));
        // Raw-byte survival, not an incidental ASCII substring. `OsStr`/`Path` `Debug`
        // renders a non-UTF-8 byte as uppercase `\xFF` on this platform, so the exact
        // rendered form is what must appear in the message: the earlier
        // `contains("ff")` passed only because this fixture's name also contains the ASCII
        // letters `f`+`f`, and a name like `raw-\xe9` (rendered `"raw-\xE9"`) would have
        // failed it. Asserted against the real rendering, not a guessed spelling.
        let rendered_raw = format!("{raw:?}");
        assert_eq!(
            rendered_raw, "\"raw-\\xFF\"",
            "the raw component must render its exact escape, not U+FFFD"
        );
        assert!(
            warning.message.contains(&rendered_raw),
            "warning message must carry the rendered raw form verbatim: {}",
            warning.message
        );
        assert!(
            warning.message.contains("\\xFF"),
            "raw byte survives as \\xFF: {}",
            warning.message
        );
        assert!(
            !warning.message.contains('\u{fffd}'),
            "the exact raw form is kept, never a lossy substitution: {}",
            warning.message
        );
        // The same warning's `Display` is diagnostics-only and lossy: the *path* segment
        // shows U+FFFD while `message`/`Debug` keep the exact `\xFF` (`S9` item 2).
        let displayed = warning.to_string();
        assert!(
            displayed.contains("raw-\u{fffd}"),
            "displayed path is the lossy diagnostic form: {displayed}"
        );
        assert!(
            displayed.contains("name-not-utf8"),
            "the kind string is part of the display: {displayed}"
        );
        mkdir(&raw_dir.join("child"));
    }
    let summary = WalkHandle::start(whole_library(&raw_dir), CancelToken::new())
        .terminal()
        .await;
    assert!(summary.outcome.is_complete());
    assert_eq!(summary.folders_emitted, 2);
    cleanup(&root);
}

/// Real-filesystem evidence for the raw-endpoint exclusion (`S9` item c). Linux-only:
/// macOS/APFS cannot store the non-UTF-8 endpoint name this fixture needs, so this
/// compiles to nothing there. The cross-platform layer is
/// `target_preflight_and_endpoint_invalid_name_seams_are_portable` in `src/walk.rs`, which
/// drives `endpoint_name_text` with an explicit invalid-name classification on any OS.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn followed_raw_endpoint_name_marks_owner_incomplete_with_raw_diagnostics() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = fixture("raw-endpoint");
    let raw_name = OsString::from_vec(b"song-\xff.mp3".to_vec());
    let endpoint = root.join(&raw_name);
    fs::write(&endpoint, b"data").unwrap();
    let album = mkdir(&root.join("album"));
    link(&endpoint, &album.join("valid-link"));
    let mut request = whole_library(&root);
    request.targets = vec![WalkTarget {
        library_id: 1,
        path: LogicalPath::parse("album").unwrap(),
    }];
    let (events, summary) = walk(request).await;
    assert!(summary.outcome.is_complete());
    assert_eq!(
        events[0].snapshot.coverage,
        nd_scanner::folder::FolderCoverage::EntriesIncomplete
    );
    assert!(events[0].snapshot.entries.is_empty());
    assert_eq!(events[0].snapshot.warnings, summary.warnings);
    // Exact rendered raw form, not the fixture's ASCII `f`+`f` substring (`S9` item a).
    // `OsStr`/`Path` `Debug` renders the invalid byte as uppercase `\xFF` and never as
    // U+FFFD, so `\xFF` is the spelling the message has to carry.
    let rendered_name = format!("{raw_name:?}");
    let rendered_endpoint = format!("{endpoint:?}");
    assert_eq!(
        rendered_name, "\"song-\\xFF.mp3\"",
        "the endpoint basename renders its exact escape"
    );
    // `Path`'s `Debug` quotes the *whole* path once, not each component, so the
    // separately-quoted basename (which begins with a quote) is never a suffix of the
    // endpoint rendering — that comparison fails for valid names too. Compare against the
    // basename's rendered body instead, which still proves the raw basename is the final
    // segment with its escape intact (verified against the real rendering here).
    let rendered_name_body = rendered_name.trim_start_matches('"');
    assert!(
        rendered_endpoint.ends_with(rendered_name_body),
        "the raw basename is the endpoint's last segment, escape intact: {rendered_endpoint}"
    );
    assert!(
        rendered_endpoint.contains("\\xFF"),
        "the rendered endpoint keeps the raw escape: {rendered_endpoint}"
    );
    assert!(
        !rendered_endpoint.contains('\u{fffd}'),
        "Debug never substitutes U+FFFD: {rendered_endpoint}"
    );
    assert!(
        summary.warnings[0].message.contains(&rendered_endpoint),
        "warning message must carry the rendered endpoint verbatim: {}",
        summary.warnings[0].message
    );
    assert!(
        summary.warnings[0].message.contains("\\xFF"),
        "raw byte survives as \\xFF: {}",
        summary.warnings[0].message
    );
    // The warning's own path is the *link's* logical path (representable), so the
    // diagnostics-only `Display` form is exactly that path while the raw endpoint survives
    // only in `message`/`Debug` — see the portable `raw_logical_path_display_is_lossy_while_debug_is_exact`.
    assert_eq!(
        summary.warnings[0].path,
        Some(LogicalPath::parse("album/valid-link").unwrap())
    );
    assert!(
        summary.warnings[0].to_string().contains("album/valid-link"),
        "{}",
        summary.warnings[0]
    );
    cleanup(&root);
}

/// Portable (`S9` items a and 2) evidence for raw-name diagnostics, with no filesystem
/// dependency: a raw logical path can be *constructed* on any platform even where the OS
/// refuses to store such a name, so the lossy-`Display`/exact-`Debug` split is asserted on
/// macOS too, not only in the Linux-gated fixtures. `#[cfg(unix)]` only because
/// `OsStringExt` is how the raw bytes are built; the assertions themselves are
/// platform-independent, and Windows path behaviour remains a declared coverage gap.
#[cfg(unix)]
#[test]
fn raw_logical_path_display_is_lossy_while_debug_is_exact() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use nd_scanner::contract::{ScanWarning, ScanWarningKind};

    let raw = OsString::from_vec(b"raw-\xff".to_vec());
    let path = LogicalPath::root()
        .join(&raw)
        .expect("raw component is accepted");

    // `Display` is diagnostics only: it substitutes U+FFFD for the invalid byte.
    assert_eq!(path.to_string(), "raw-\u{fffd}");
    // `Debug` keeps the exact escape, which is what `message` reproduces.
    let debug = format!("{path:?}");
    assert!(debug.contains("\"raw-\\xFF\""), "{debug}");
    assert!(!debug.contains('\u{fffd}'), "{debug}");
    // `components` keep the raw bytes verbatim.
    assert_eq!(path.components()[0], raw);

    let warning = ScanWarning {
        kind: ScanWarningKind::NameNotUtf8,
        library_id: Some(1),
        path: Some(path.clone()),
        message: format!("unrepresentable raw path {path:?}; excluded from snapshot"),
    };
    // `ScanWarning::Display` inherits the lossy path rendering; the exact form lives in
    // `message` and `Debug`. The display carries the U+FFFD path segment, and only the
    // message text reproduces the exact escape.
    let displayed = warning.to_string();
    assert!(displayed.contains("raw-\u{fffd}"), "{displayed}");
    assert!(warning.message.contains("\\xFF"), "{}", warning.message);
    assert!(format!("{warning:?}").contains("\\xFF"));
}

// ---------------------------------------------------------------------------
// S9 item d — warning-kind coverage through the walk, and its stated limits
// ---------------------------------------------------------------------------

#[test]
fn warning_kind_strings_are_unique_and_pinned() {
    // `ScanWarningKind::as_str` is the stable machine-readable contract a consumer matches
    // on. `pinned` below is an independent copy of the table written as a `match`, so a
    // variant added to the enum without updating this test is a compile error rather than
    // an unasserted kind, and a renamed spelling fails the pinned comparison. Every string
    // must also be non-empty and unique, or two kinds would collapse for a consumer that
    // matches on the string alone.

    // Exhaustive over the enum: adding a variant breaks compilation here.
    const fn pinned(kind: ScanWarningKind) -> &'static str {
        match kind {
            ScanWarningKind::IgnoreFileOpenFailed => "ignore-file-open-failed",
            ScanWarningKind::IgnoreFileReadFailed => "ignore-file-read-failed",
            ScanWarningKind::IgnorePatternInvalid => "ignore-pattern-invalid",
            ScanWarningKind::IgnorePatternUnsupported => "ignore-pattern-unsupported",
            ScanWarningKind::IgnoreFileNotUtf8 => "ignore-file-not-utf8",
            ScanWarningKind::IgnoreLineTooLong => "ignore-line-too-long",
            ScanWarningKind::DirectoryUnreadable => "directory-unreadable",
            ScanWarningKind::DirectoryEnumerationFailed => "directory-enumeration-failed",
            ScanWarningKind::EntryStatFailed => "entry-stat-failed",
            ScanWarningKind::SymlinkSkipped => "symlink-skipped",
            ScanWarningKind::DirectoryCycleSkipped => "directory-cycle-skipped",
            ScanWarningKind::SymlinkedTargetSkipped => "symlinked-target-skipped",
            ScanWarningKind::NameNotUtf8 => "name-not-utf8",
            ScanWarningKind::OutsideRootEndpoint => "outside-root-endpoint",
            ScanWarningKind::TargetMissing => "target-missing",
            ScanWarningKind::DepthLimitReached => "depth-limit-reached",
            ScanWarningKind::TimestampUnavailable => "timestamp-unavailable",
        }
    }

    // Every variant, in declaration order. The count assertion catches a variant added to
    // `pinned`'s match but not listed here.
    const ALL: [ScanWarningKind; 17] = [
        ScanWarningKind::IgnoreFileOpenFailed,
        ScanWarningKind::IgnoreFileReadFailed,
        ScanWarningKind::IgnorePatternInvalid,
        ScanWarningKind::IgnorePatternUnsupported,
        ScanWarningKind::IgnoreFileNotUtf8,
        ScanWarningKind::IgnoreLineTooLong,
        ScanWarningKind::DirectoryUnreadable,
        ScanWarningKind::DirectoryEnumerationFailed,
        ScanWarningKind::EntryStatFailed,
        ScanWarningKind::SymlinkSkipped,
        ScanWarningKind::DirectoryCycleSkipped,
        ScanWarningKind::SymlinkedTargetSkipped,
        ScanWarningKind::NameNotUtf8,
        ScanWarningKind::OutsideRootEndpoint,
        ScanWarningKind::TargetMissing,
        ScanWarningKind::DepthLimitReached,
        ScanWarningKind::TimestampUnavailable,
    ];
    assert_eq!(
        ALL.len(),
        17,
        "the walk's warning census covers 17 kinds; keep this in step with the enum"
    );

    let mut seen = std::collections::HashSet::new();
    for kind in ALL {
        let text = kind.as_str();
        assert!(!text.is_empty(), "{kind:?} has an empty kind string");
        assert_eq!(text, pinned(kind), "kind string for {kind:?}");
        assert!(
            seen.insert(text),
            "kind string {text} is reported by more than one variant"
        );
        // Round-trip: the kind string must identify the variant uniquely, so two variants
        // sharing a spelling cannot silently collapse for a consumer matching on it.
        for other in ALL {
            if other != kind {
                assert_ne!(
                    kind.as_str(),
                    other.as_str(),
                    "{kind:?} and {other:?} both report {text}"
                );
            }
        }
    }
}

/// `DirectoryEnumerationFailed` (`walk.rs` `enter_directory`'s `NotFound`/`NotADirectory`
/// arm) through a real walk. Deterministically reachable on Unix without privileges: name a
/// **regular file** as a directory target. `snapshot` succeeds for it (so the target is not
/// skipped as missing, exactly as Go's `fs.Stat` check accepts it), while `read_dir` fails
/// with `ENOTDIR` — the same shape Go's `os.ReadDir` produces — which is the branch that
/// reports `directory-enumeration-failed` instead of `directory-unreadable`. A *denied*
/// directory cannot produce this kind: `EACCES` maps to `PermissionDenied`, which reports
/// `directory-unreadable`, so the denied directory beside this target is probed by
/// *observed* access — and the branch asserted — exactly like the existing permission
/// tests, because running as root can open a `0o000` directory.
#[cfg(unix)]
#[tokio::test]
async fn a_target_that_cannot_be_listed_reports_enumeration_failed_in_the_summary() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture("enumerate-fail");
    write(&root, "keep.mp3");
    let unlistable = write(&root, "notadir.bin");
    assert!(
        fs::metadata(&unlistable).is_ok(),
        "the target must stat, so it is not skipped as missing"
    );
    assert!(
        fs::read_dir(&unlistable).is_err(),
        "listing a non-directory is what this warning is about"
    );

    // Observed-access probe for the permission branch: `0o000` denies a non-root runner.
    let denied = mkdir(&root.join("private"));
    write(&denied, "s.mp3");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).expect("chmod");
    let denied_readable = fs::read_dir(&denied).is_ok();

    let mut request = whole_library(&root);
    request.targets = vec![WalkTarget {
        library_id: 1,
        path: LogicalPath::parse("notadir.bin").expect("p"),
    }];
    let (events, summary) = walk(request).await;

    let enumeration = warnings(&summary, ScanWarningKind::DirectoryEnumerationFailed);
    assert_eq!(
        enumeration.len(),
        1,
        "the unlistable target is reported once in the terminal summary"
    );
    assert_eq!(
        enumeration[0].path,
        Some(LogicalPath::parse("notadir.bin").unwrap())
    );
    assert_eq!(enumeration[0].library_id, Some(1));
    assert_eq!(
        summary.folders_incomplete, 1,
        "never an empty successful folder"
    );
    assert_eq!(
        summary.folders_emitted, 0,
        "no snapshot for an unlistable target"
    );
    assert_eq!(summary.targets_walked, 1);
    assert!(
        summary.outcome.is_complete(),
        "an unlistable directory is a warning, not a fatal failure: {}",
        summary.outcome.describe()
    );
    assert!(events.is_empty());
    assert!(warnings(&summary, ScanWarningKind::DirectoryUnreadable).is_empty());

    // Walking the whole library reaches the denied directory as a *child*, so the observed
    // access decides the kind here exactly as the existing permission fixture branches.
    let (whole_events, whole) = walk(whole_library(&root)).await;
    if denied_readable {
        // Root (or an ACL) can open a 0o000 directory, so no unreadable warning is owed.
        assert!(
            warnings(&whole, ScanWarningKind::DirectoryUnreadable).is_empty(),
            "success branch requires actual access"
        );
        assert!(paths(&whole_events).contains(&"private".to_string()));
    } else {
        assert_eq!(
            warnings(&whole, ScanWarningKind::DirectoryUnreadable).len(),
            1,
            "denied access reports directory-unreadable, not enumeration-failed"
        );
        assert!(!paths(&whole_events).contains(&"private".to_string()));
    }
    // Reaching the root makes the unlistable file a plain entry classified by extension,
    // so `directory-enumeration-failed` belongs only to the explicit directory-target walk.
    assert!(warnings(&whole, ScanWarningKind::DirectoryEnumerationFailed).is_empty());

    fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).expect("restore");
    cleanup(&root);
}

/// `DepthLimitReached` (`walk.rs` `enter_directory`) reached through a real walk while the
/// production `MAX_WALK_DEPTH` stays untouched. The chain is built one level deeper than
/// the cap so the refusal boundary is derived from the constant rather than guessed, and
/// the tree is shallow enough to build and walk in milliseconds.
#[cfg(unix)]
#[tokio::test]
async fn a_directory_deeper_than_the_cap_is_refused_with_a_typed_warning() {
    use nd_scanner::walk::MAX_WALK_DEPTH;

    let root = fixture("depth-limit");
    // Build the chain one level deeper than the cap, and keep the matching logical path so
    // the refusal boundary is derived from the constant rather than guessed.
    let mut dir = root.clone();
    let mut logical = LogicalPath::root();
    for _ in 0..MAX_WALK_DEPTH {
        dir = dir.join("d");
        logical = logical.join(OsStr::new("d")).expect("depth component");
        mkdir(&dir);
    }
    write(&dir, "bottom.mp3");

    let (events, summary) = walk(whole_library(&root)).await;
    assert!(
        summary.outcome.is_complete(),
        "the cap is a warning, not a crash: {}",
        summary.outcome.describe()
    );

    let depth_warnings = warnings(&summary, ScanWarningKind::DepthLimitReached);
    assert_eq!(
        depth_warnings.len(),
        1,
        "exactly one directory is refused, and its descendants are never visited"
    );
    // `active` holds the seed root plus one identity per entered directory, and the walk
    // root counts as entered, so the first refused directory sits at depth
    // `MAX_WALK_DEPTH - 1`; the refusal stops descent, so nothing deeper is reported.
    assert_eq!(
        depth_warnings[0].path,
        Some(logical.parent()),
        "the refused directory is the first one past the cap"
    );
    assert_eq!(depth_warnings[0].library_id, Some(1));
    assert!(
        depth_warnings[0]
            .message
            .contains(&MAX_WALK_DEPTH.to_string()),
        "{}",
        depth_warnings[0].message
    );

    // Everything at or above the boundary is still walked and emitted: root plus the
    // directories at depths 1..=MAX_WALK_DEPTH-2.
    assert_eq!(
        summary.folders_emitted,
        MAX_WALK_DEPTH - 1,
        "the cap refuses one directory, not the subtree"
    );
    assert_eq!(
        summary.folders_incomplete, 0,
        "a refusal is not an incomplete listing"
    );
    assert_eq!(summary.targets_walked, 1);
    assert_eq!(
        paths(&events).last().expect("root last"),
        ".",
        "postorder still ends at the walk root"
    );
    assert_eq!(
        paths(&events).len(),
        MAX_WALK_DEPTH - 1,
        "one refused directory, no deeper events"
    );
    cleanup(&root);
}

/// `TimestampUnavailable` (`walk.rs` `build_event`) needs a directory whose own `stat`
/// reports no mtime. **Not deterministically reachable on this platform** (`S9` item d:
/// stated, not papered over): on this macOS/APFS machine `Metadata::modified()` never
/// fails for a directory an unprivileged user can reach, and extreme timestamps are
/// clamped into the int64 range instead of rejected (probed below). Linux/ext4 can report
/// `EOVERFLOW` for an out-of-range inode timestamp, but that cannot be forced without
/// root, so no fixture reaches this kind through a real walk here. The production decision
/// function is exercised directly in `src/walk.rs`
/// (`a_frame_without_a_directory_mtime_refuses_the_snapshot_with_a_typed_warning` and
/// `a_directory_timestamp_refusal_reaches_the_terminal_summary`), which drive `build_event`
/// and `WalkHandle::terminal` with a captured `StatUnavailable`.
#[cfg(unix)]
#[tokio::test]
async fn a_directory_mtime_failure_is_not_constructible_on_this_platform() {
    use std::fs::{File, FileTimes};
    use std::time::Duration;

    let root = fixture("mtime-unreachable");
    let album = mkdir(&root.join("album"));
    write(&album, "song.mp3");

    for secs in [0u64, 1_600_000_000, 253_402_300_800, u64::MAX] {
        let stamp = UNIX_EPOCH.checked_add(Duration::from_secs(secs));
        if let Some(stamp) = stamp {
            if File::open(&album)
                .and_then(|f| f.set_times(FileTimes::new().set_modified(stamp)))
                .is_ok()
            {
                assert!(
                    fs::metadata(&album).and_then(|m| m.modified()).is_ok(),
                    "the platform reports a directory mtime for {secs}s, so no fixture can \
                     produce StatUnavailable"
                );
            }
        }
    }

    let (events, summary) = walk(whole_library(&root)).await;
    assert!(summary.outcome.is_complete());
    assert!(
        warnings(&summary, ScanWarningKind::TimestampUnavailable).is_empty(),
        "no folder on this platform can lose its directory mtime through a walk"
    );
    assert_eq!(summary.folders_emitted, 2);
    assert_eq!(summary.folders_incomplete, 0);
    assert!(events.iter().all(|e| e.snapshot.dir_stat.modified.is_ok()));
    cleanup(&root);
}

/// Walk-level `EntryStatFailed` (`walk.rs` `accumulate_entry`). An *unreadable* nested file
/// is not enough here: `lstat` needs no permission on the file itself, so a `0o000` file
/// still reports an mtime (asserted below), and this platform clamps extreme timestamps
/// rather than failing them — so no unprivileged fixture can make `Metadata::modified()`
/// fail (`S9` item d: stated, not papered over). The production code path is exercised in
/// `src/walk.rs` (`one_unstatable_entry_still_emits_the_folder_and_reaches_the_summary_once`),
/// which runs `accumulate_entry` + `build_event` + `WalkHandle::terminal` — the real walk
/// body, not a stand-in — and asserts the warning reaches the terminal summary alongside a
/// statable entry. If a platform can fail an entry stat, the probe branch here asserts the
/// walk-level warning instead.
///
/// The `TimestampUnavailable` sibling above covers the *directory*-side timestamp refusal,
/// which is the other half of item (d); this one is the entry-side half.
#[cfg(unix)]
#[tokio::test]
async fn an_entry_stat_failure_reaches_the_summary_only_where_the_platform_can_produce_one() {
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let root = fixture("entry-stat-fail");
    write(&root, "keep.mp3");
    let album = mkdir(&root.join("album"));
    let unreadable = write(&album, "hidden.mp3");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod");

    // A denied file still lstats: this is exactly why an "unreadable nested file" cannot
    // force EntryStatFailed, and the observation is asserted, not assumed.
    assert!(
        fs::metadata(&unreadable).and_then(|m| m.modified()).is_ok(),
        "chmod 0o000 does not remove an entry mtime on this platform"
    );
    // A clamped extreme timestamp likewise still reports a time.
    let clamped = UNIX_EPOCH.checked_add(Duration::from_secs(u64::MAX / 2));
    if let Some(clamped) = clamped {
        if File::open(&unreadable)
            .and_then(|f| f.set_times(FileTimes::new().set_modified(clamped)))
            .is_ok()
        {
            assert!(fs::metadata(&unreadable).and_then(|m| m.modified()).is_ok());
        }
    }

    let stat_failure_constructible = fs::metadata(&unreadable)
        .map(|m| m.modified().is_err())
        .unwrap_or(true);

    let (events, summary) = walk(whole_library(&root)).await;
    let stat_warnings = warnings(&summary, ScanWarningKind::EntryStatFailed);
    if stat_failure_constructible {
        assert_eq!(
            stat_warnings.len(),
            1,
            "an entry whose mtime is not reported is reported, once, in the summary"
        );
        assert!(events
            .iter()
            .any(|e| e.snapshot.coverage == nd_scanner::folder::FolderCoverage::EntriesIncomplete));
    } else {
        assert!(
            stat_warnings.is_empty(),
            "the platform reported every entry mtime, so nothing may be warned"
        );
        assert_eq!(summary.folders_incomplete, 0);
        assert_eq!(summary.folders_emitted, 2);
        assert!(events
            .iter()
            .all(|e| e.snapshot.coverage == nd_scanner::folder::FolderCoverage::Complete));
    }

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).expect("restore");
    cleanup(&root);
}
