//! Focused regressions for the **additive** SC-02 listing seam
//! (`storage::LocalDir::list_annotated`, `EntryFacts`, `DirectoryListing`).
//!
//! These prove the two fact sets are distinct: a followed directory alias whose child is a
//! file symlink must yield the endpoint's extension/kind for classification while the hash
//! facts stay the link's own `lstat`. Changing only the target's stat must therefore move
//! `endpoint.stat` and leave `stat` untouched — and vice versa.
//!
//! Existing `tests/local_storage.rs` semantics are untouched by this seam; this file adds
//! nothing to it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nd_scanner::storage::{
    DirectoryListing, EndpointKind, EntryEndpoint, EntryFacts, EntryKind, Follow, LocalDir,
    LogicalPath, StorageError,
};

fn fixture(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-seam-{}-{}-{}",
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

fn storage(root: &Path) -> LocalDir {
    LocalDir::from_path(root).expect("from_path")
}

fn logical(path: &str) -> LogicalPath {
    LogicalPath::parse(path).expect("parse")
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write file");
    path
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

/// Set a file's mtime explicitly, so a test can prove **which** stat a fact came from.
/// `std::fs` cannot set mtimes and no new test dependency is authorized, so this uses the
/// `touch` shell tool with an explicit UTC stamp (the `filetime` crate is not locked).
fn set_mtime(path: &Path, since_epoch: Duration) {
    #[cfg(unix)]
    {
        let time = UNIX_EPOCH + since_epoch;
        // `-h` so a symlink's own mtime is set rather than its target's.
        let status = std::process::Command::new("touch")
            .arg("-h")
            .arg("-t")
            .arg(go_touch_stamp(time))
            .arg(path)
            .env("TZ", "UTC")
            .status()
            .expect("run touch");
        assert!(status.success(), "touch {path:?} failed");
        let stat = if fs::symlink_metadata(path)
            .expect("lstat")
            .file_type()
            .is_symlink()
        {
            fs::symlink_metadata(path).expect("lstat after touch")
        } else {
            fs::metadata(path).expect("stat after touch")
        };
        let actual = stat.modified().expect("mtime after touch");
        assert_eq!(
            actual, time,
            "touch must set the requested second (whole-second precision)"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (path, since_epoch);
        panic!("mtime control is unavailable on this platform: declared coverage gap");
    }
}

/// `touch -t [[CCYY]MMDDhhmm[.ss]]` stamp, UTC.
#[cfg(unix)]
fn go_touch_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    let days = secs / 86_400;
    let rest = secs % 86_400;
    // Civil-date breakdown without a dependency: 400-year cycle arithmetic.
    let (year, month, day) = civil_from_days(days as i64);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!(
        "{:04}{:02}{:02}{:02}{:02}.{:02}",
        year, month, day, hour, minute, second
    )
}

#[cfg(unix)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(unix)]
#[test]
fn a_file_link_reports_its_own_lstat_size_beside_the_endpoint_size() {
    let root = fixture("link-facts");
    let target = write_file(&root, "real.mp3", b"0123456789"); // 10 bytes
    symlink(&target, &root.join("link.mp3")); // link text length differs

    let listing = storage(&root)
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("list");

    let link = listing
        .entries
        .iter()
        .find(|entry| entry.path.to_string() == "link.mp3")
        .expect("link entry");
    assert_eq!(link.kind, EntryKind::Symlink, "entry kind is the link");
    let link_bytes = std::fs::symlink_metadata(root.join("link.mp3"))
        .expect("lstat")
        .len();
    assert_eq!(
        link.stat.size, link_bytes,
        "hash facts are the link's own size"
    );
    assert_eq!(
        link.endpoint.as_ref().map(|endpoint| endpoint.stat.size),
        Some(10),
        "endpoint facts are the target's size"
    );
    assert_eq!(
        link.endpoint.as_ref().map(|endpoint| endpoint.kind),
        Some(EndpointKind::File)
    );

    // The regular file next to it carries no endpoint at all.
    let real = listing
        .entries
        .iter()
        .find(|entry| entry.path.to_string() == "real.mp3")
        .expect("real entry");
    assert_eq!(real.kind, EntryKind::File);
    assert!(real.endpoint.is_none(), "a plain file has no endpoint view");

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn changing_the_target_stat_moves_endpoint_facts_only() {
    let root = fixture("stat-distinct");
    let target = write_file(&root, "real.mp3", b"abc");
    symlink(&target, &root.join("link.mp3"));

    let store = storage(&root);
    let before = store
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("first list");
    let before_link = before
        .entries
        .iter()
        .find(|e| e.path.to_string() == "link.mp3")
        .expect("link");
    let link_stat = before_link.stat.clone();
    let endpoint_stat = before_link
        .endpoint
        .as_ref()
        .expect("endpoint")
        .stat
        .clone();

    // Rewrite the target: size and mtime both change, the link is untouched.
    fs::write(&target, b"abcdefghij").expect("rewrite target");
    set_mtime(&target, Duration::from_secs(1_500_000_000));

    let after = store
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("second list");
    let after_link = after
        .entries
        .iter()
        .find(|e| e.path.to_string() == "link.mp3")
        .expect("link");

    assert_eq!(
        after_link.stat, link_stat,
        "link lstat facts are unaffected by the target's change"
    );
    let endpoint = after_link.endpoint.as_ref().expect("endpoint");
    assert_eq!(endpoint.stat.size, 10, "endpoint size tracked the target");
    assert_ne!(
        endpoint.stat.modified, endpoint_stat.modified,
        "endpoint mtime tracked the target"
    );

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn changing_the_link_stat_moves_link_facts_only() {
    let root = fixture("link-touch");
    let target = write_file(&root, "real.mp3", b"abcdefghij");
    let link = root.join("link.mp3");
    symlink(&target, &link);

    let store = storage(&root);
    let before = store
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("list");
    let before_link = before
        .entries
        .iter()
        .find(|e| e.path.to_string() == "link.mp3")
        .expect("link");
    let endpoint_modified = before_link
        .endpoint
        .as_ref()
        .expect("endpoint")
        .stat
        .modified;

    set_mtime(&link, Duration::from_secs(1_200_000_000));

    let after = store
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("list again");
    let after_link = after
        .entries
        .iter()
        .find(|e| e.path.to_string() == "link.mp3")
        .expect("link");

    assert_eq!(
        after_link.stat.modified,
        Ok(UNIX_EPOCH + Duration::from_secs(1_200_000_000)),
        "the link's own mtime is what a fingerprint would hash"
    );
    assert_eq!(
        after_link
            .endpoint
            .as_ref()
            .expect("endpoint")
            .stat
            .modified,
        endpoint_modified,
        "the target's mtime is untouched by relinking the alias"
    );

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_followed_directory_alias_lists_children_with_link_facts_retained() {
    let root = fixture("dir-alias");
    let outside = fixture("dir-alias-target");
    let album = outside.join("album");
    write_file(&album, "song.mp3", b"xyz");
    symlink(&album, &root.join("alias"));
    // A file symlink inside the aliased directory: the endpoint lives outside the root.
    let outside_target = outside.join("real.mp3");
    write_file(&outside, "real.mp3", b"012345678901234");
    symlink(&outside_target, &album.join("alias-track.mp3"));

    let store = storage(&root);
    let listing = store
        .list_annotated(&logical("alias"), Follow::Yes)
        .expect("descend through a symlinked directory ancestor");

    assert_eq!(listing.entries.len(), 2, "both children are listed");
    let names: Vec<String> = listing.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(names, vec!["alias/alias-track.mp3", "alias/song.mp3"]);

    let link = listing
        .entries
        .iter()
        .find(|e| e.path.to_string() == "alias/alias-track.mp3")
        .expect("nested link");
    let link_size = std::fs::symlink_metadata(album.join("alias-track.mp3"))
        .expect("lstat")
        .len();
    assert_eq!(
        link.stat.size, link_size,
        "terminal entry keeps its link lstat"
    );
    assert_eq!(
        link.endpoint.as_ref().expect("endpoint").stat.size,
        15,
        "endpoint facts come from the outside-root target"
    );

    // The directory's own stat is the followed endpoint (Go's `fs.Stat(dirPath)`).
    let alias_dir_stat = std::fs::metadata(&album).expect("stat alias dir");
    assert_eq!(listing.dir_stat.size, alias_dir_stat.len());

    cleanup(&root);
    cleanup(&outside);
}

#[cfg(unix)]
#[test]
fn follow_disabled_still_refuses_a_symlinked_directory_and_reports_no_endpoint() {
    let root = fixture("seam-refuse");
    let target = fixture("seam-refuse-target");
    write_file(&target.join("album"), "song.mp3", b"x");
    symlink(&target, &root.join("alias"));
    write_file(&root, "plain.mp3", b"x");
    symlink(&root.join("plain.mp3"), &root.join("file-link.mp3"));

    let store = storage(&root);
    let err = store
        .list_annotated(&logical("alias"), Follow::No)
        .expect_err("a symlinked directory is refused");
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "got {err:?}"
    );

    let listing = store
        .list_annotated(&LogicalPath::root(), Follow::No)
        .expect("root listing");
    let link = listing
        .entries
        .iter()
        .find(|e| e.path.to_string() == "file-link.mp3")
        .expect("link entry");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert!(
        link.endpoint.is_none(),
        "follow-disabled exposes no endpoint facts at all"
    );

    cleanup(&root);
    cleanup(&target);
}

#[cfg(unix)]
#[test]
fn a_broken_link_keeps_its_entry_and_reports_no_endpoint() {
    let root = fixture("broken-link");
    write_file(&root, "keep.mp3", b"x");
    symlink(&root.join("gone.mp3"), &root.join("broken.mp3"));

    let listing = storage(&root)
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("listing survives a broken link");
    let broken = listing
        .entries
        .iter()
        .find(|e| e.path.to_string() == "broken.mp3")
        .expect("broken entry");
    assert_eq!(broken.kind, EntryKind::Symlink);
    assert!(broken.endpoint.is_none());
    assert!(
        broken.stat.modified.is_ok(),
        "the link's own mtime is available"
    );

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn listing_is_sorted_by_raw_name_bytes_and_carries_the_directory_stat() {
    let root = fixture("sorted");
    for name in ["Zebra.mp3", "apple.mp3", "Banana.mp3", "_under.mp3"] {
        write_file(&root, name, b"x");
    }
    let listing = storage(&root)
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("list");
    let names: Vec<String> = listing.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(
        names,
        vec!["Banana.mp3", "Zebra.mp3", "_under.mp3", "apple.mp3"],
        "byte order, like Go's fs.ReadDir sort"
    );
    assert!(listing.dir_stat.modified.is_ok());

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_missing_and_a_non_directory_listing_are_typed_errors() {
    let root = fixture("errors");
    write_file(&root, "song.mp3", b"x");
    let store = storage(&root);

    let err = store
        .list_annotated(&logical("nope"), Follow::Yes)
        .expect_err("missing directory");
    assert!(matches!(err, StorageError::NotFound { .. }), "got {err:?}");

    let err = store
        .list_annotated(&logical("song.mp3"), Follow::Yes)
        .expect_err("file is not a directory");
    assert!(
        matches!(err, StorageError::NotADirectory { .. }),
        "got {err:?}"
    );

    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn an_unreadable_directory_is_a_permission_error_not_an_empty_listing() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture("unreadable");
    let closed = root.join("closed");
    fs::create_dir(&closed).expect("create");
    write_file(&closed, "song.mp3", b"x");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let result = storage(&root).list_annotated(&logical("closed"), Follow::Yes);
    // Running as root can still read a 0o000 directory, so assert only the guarantee that
    // holds either way: no listing is reported for an unreadable directory.
    match result {
        Err(err) => assert!(
            matches!(err, StorageError::PermissionDenied { .. }),
            "got {err:?}"
        ),
        Ok(listing) => assert!(
            !listing.entries.is_empty(),
            "readable despite permissions: must not look empty"
        ),
    }

    fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).expect("restore");
    cleanup(&root);
}

#[cfg(not(unix))]
#[test]
fn the_seam_is_platform_independent_where_symlinks_are_unavailable() {
    let root = fixture("windows-gap");
    write_file(&root, "song.mp3", b"x");
    let listing = storage(&root)
        .list_annotated(&LogicalPath::root(), Follow::Yes)
        .expect("plain listing");
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].kind, EntryKind::File);
    assert!(listing.entries[0].endpoint.is_none());
    // Windows symlink/junction behaviour is a declared gap, not parity.
    cleanup(&root);
}

/// Compile-time guard: the seam's shapes stay constructible for callers.
#[allow(dead_code)]
fn _shape_markers(
    entry: EntryFacts,
    endpoint: EntryEndpoint,
    listing: DirectoryListing,
) -> (EntryFacts, EntryEndpoint, DirectoryListing) {
    (entry, endpoint, listing)
}
