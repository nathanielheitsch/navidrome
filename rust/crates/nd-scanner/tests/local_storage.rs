//! Real-filesystem tests for the SC-02 local storage subset (`src/storage.rs`).
//!
//! Every fixture is a fresh temp directory owned by the test that created it, removed only by
//! that test. Nothing outside `std::env::temp_dir()` is touched.
//!
//! Platform coverage: symlink/follow tests are `cfg(unix)`. The `cfg(not(unix))` test covers
//! only platform-independent guarantees — **Windows symlink and path behaviour is not
//! exercised here** (macOS was the only host run), and is reported as a gap.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nd_scanner::storage::{
    BirthTimeUnavailable, EndpointKind, EntryKind, Follow, LocalDir, LogicalPath, ResolvedTarget,
    StorageError, MAX_SYMLINK_HOPS,
};

/// Unique owned fixture root, created fresh.
fn fixture(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-storage-{}-{}-{}",
        std::process::id(),
        nanos,
        name
    ));
    fs::create_dir_all(&root).expect("create fixture root");
    root
}

/// Remove only the fixture this test created.
fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn storage(root: &Path) -> LocalDir {
    LocalDir::open(&format!("file://{}", root.display())).expect("open file:// root")
}

fn logical(path: &str) -> LogicalPath {
    LogicalPath::parse(path).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("create dir");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write file");
    path
}

fn names(entries: &[nd_scanner::storage::ListEntry]) -> Vec<String> {
    entries.iter().map(|e| e.path.to_string()).collect()
}

#[cfg(unix)]
fn os_name(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(target_os = "linux")]
fn raw_bytes(name: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

#[cfg(unix)]
fn symlink_str(target: &str, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

// ---------------------------------------------------------------- schema / logical paths

#[test]
fn unsupported_schemes_are_rejected_rather_than_treated_as_local() {
    for uri in [
        "s3://bucket/music",
        "sftp://host/song.mp3",
        "http://host/a.mp3",
        "gs://bucket/x",
        "smb://share/music",
    ] {
        let err = LocalDir::open(uri).expect_err("remote URI must not open");
        match err {
            StorageError::UnsupportedScheme { ref scheme } => {
                assert!(
                    uri.starts_with(scheme.as_str()),
                    "{uri} rejected as scheme {scheme}"
                );
                assert!(
                    err.to_string().contains("only 'file'"),
                    "message names the only supported scheme: {err}"
                );
            }
            other => panic!("expected UnsupportedScheme for {uri}, got {other:?}"),
        }
    }
}

#[test]
fn file_scheme_and_bare_path_are_accepted() {
    let root = fixture("scheme");
    let via_url = storage(&root);
    let bare = LocalDir::open(&root.display().to_string()).expect("bare path is file://");
    assert_eq!(via_url.root(), bare.root());
    assert!(via_url.exists(&LogicalPath::root()));

    // Go's `For` treats a schema-less string as file:// too (`core/storage/storage.go:60-66`), including
    // a Windows-style path — it is a local root here, never a remote scheme.
    let windowsish = LocalDir::open("C:\\music\\a.mp3").expect("bare path, no scheme");
    assert_eq!(windowsish.root(), Path::new("C:\\music\\a.mp3"));

    assert!(LocalDir::open("file://").is_err(), "empty file:// path");
    cleanup(&root);
}

#[test]
fn logical_paths_that_escape_or_are_malformed_are_rejected_before_io() {
    let root = fixture("paths");
    let store = storage(&root);
    for bad in [
        "..",
        "../x",
        "a/../b",
        "/absolute/x.mp3",
        "a//b",
        "a/",
        "a\\b",
    ] {
        let err = LogicalPath::parse(bad).expect_err("must reject {bad}");
        assert!(
            err.to_string().contains("invalid logical path"),
            "{bad} → {err}"
        );
    }
    assert!(
        LogicalPath::parse("").unwrap().is_root(),
        "empty means root"
    );
    assert!(LogicalPath::parse(".").unwrap().is_root(), "dot means root");
    assert_eq!(logical("a/b").components().len(), 2);
    assert_eq!(logical("a/b").parent(), logical("a"));
    assert_eq!(logical("a").file_name(), Some(OsStr::new("a")));

    // An OS path outside the root cannot be addressed either.
    assert!(store.logical(OsStr::new("/tmp/other/x.mp3")).is_err());
    assert!(store
        .logical(root.join("a").join("..").join("b").as_os_str())
        .is_err());
    assert!(store.logical(OsStr::new("")).is_err());
    assert_eq!(
        store.logical(root.as_os_str()).unwrap(),
        LogicalPath::root()
    );
    assert_eq!(
        store
            .logical(root.join("a").join("b.mp3").as_os_str())
            .unwrap(),
        logical("a/b.mp3")
    );

    // Rejection happens before any I/O: an escaped path cannot even be constructed, so no
    // operation can reach the filesystem with it.
    for bad in ["../sibling", "a/../../etc/passwd"] {
        assert!(LogicalPath::parse(bad).is_err(), "{bad} is not addressable");
    }
    cleanup(&root);
}

#[test]
fn join_and_parent_keep_component_boundaries() {
    let root = fixture("join");
    let store = storage(&root);
    let dir = logical("album");
    let child = dir.join(OsStr::new("01.flac")).expect("join");
    assert_eq!(child, logical("album/01.flac"));
    assert_eq!(child.parent(), dir);
    assert!(dir.join(OsStr::new("..")).is_err());
    assert!(dir.join(OsStr::new(".")).is_err());
    assert!(dir.join(OsStr::new("")).is_err());
    assert_eq!(
        store.os_path(&dir),
        root.join("album"),
        "OS path only for I/O"
    );
    assert_eq!(store.os_path(&LogicalPath::root()), root);
    cleanup(&root);
}

// ---------------------------------------------------------------- listing

#[test]
fn empty_directory_lists_nothing_and_listing_is_repeatable() {
    let root = fixture("empty");
    let store = storage(&root);
    assert_eq!(
        store.list(&LogicalPath::root(), Follow::No).unwrap(),
        Vec::new()
    );
    write_file(&root, "only.mp3", b"x");
    let once = store.list(&LogicalPath::root(), Follow::No).unwrap();
    let twice = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(once, twice, "same order and content on repeat");
    assert_eq!(once.len(), 1);
    assert_eq!(once[0].path, logical("only.mp3"));
    assert_eq!(once[0].kind, EntryKind::File);
    assert_eq!(once[0].stat.size, 1);
    cleanup(&root);
}

#[test]
fn entries_are_sorted_by_name_with_dirs_included() {
    let root = fixture("sort");
    let store = storage(&root);
    fs::create_dir_all(root.join("a")).expect("mkdir");
    write_file(&root.join("a"), "z.mp3", b"1");
    write_file(&root, "a!x.mp3", b"22");
    write_file(&root, "b.mp3", b"333");
    fs::create_dir_all(root.join("z")).expect("mkdir");

    // One directory level, sorted by raw name bytes — the same order Go's fs.ReadDir
    // produces. A full-tree postorder walk is SC-02 work this subset does not do.
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(
        names(&entries),
        vec!["a", "a!x.mp3", "b.mp3", "z"],
        "sorted by name, directories included, one level only"
    );
    assert_eq!(entries[0].kind, EntryKind::Dir);
    assert_eq!(entries[1].stat.size, 2, "exact size of the 2-byte file");
    assert!(
        !names(&entries).contains(&"a/z.mp3".to_string()),
        "children come from a separate list() call, not a recursive listing"
    );
    cleanup(&root);
}

#[test]
fn list_descends_into_subdirectories_with_full_logical_paths() {
    let root = fixture("subdir");
    let store = storage(&root);
    fs::create_dir_all(root.join("album").join("disc 1")).expect("mkdir");
    write_file(&root.join("album").join("disc 1"), "01.flac", b"abc");
    write_file(&root.join("album"), "cover.jpg", b"xy");

    let top = store.list(&logical("album"), Follow::No).unwrap();
    assert_eq!(names(&top), vec!["album/cover.jpg", "album/disc 1"]);
    assert_eq!(top[1].kind, EntryKind::Dir);
    let nested = store.list(&logical("album/disc 1"), Follow::No).unwrap();
    assert_eq!(names(&nested), vec!["album/disc 1/01.flac"]);
    cleanup(&root);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_survive_without_collision_or_lossiness_on_linux() {
    // ext4/tmpfs accept arbitrary byte sequences, so this proves the exact-bytes guarantee.
    // APFS rejects them (EILSEQ) — see the macOS test below — so this one runs on Linux only
    // and its non-execution on this host is reported as a coverage gap.
    let root = fixture("rawnames");
    let store = storage(&root);
    let a = os_name(&[b'x', 0xf0, b'.', b'm', b'p', b'3']);
    let b = os_name(&[b'x', 0xff, b'.', b'm', b'p', b'3']);
    for name in [&a, &b] {
        fs::write(root.join(name), b"1234").expect("write raw name");
    }

    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(entries.len(), 2, "no collisions");
    let listed: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| raw_bytes(e.path.file_name().expect("name")))
        .collect();
    assert!(listed.contains(&raw_bytes(&a)));
    assert!(listed.contains(&raw_bytes(&b)));
    for name in [&a, &b] {
        let path = store
            .logical(root.join(name).as_os_str())
            .expect("logical from raw name");
        assert_eq!(store.read(&path, Follow::No).unwrap(), b"1234");
    }
    cleanup(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn non_utf8_names_are_rejected_by_the_filesystem_on_macos() {
    // Documented platform limit, asserted rather than assumed: APFS refuses non-UTF8 names
    // (EILSEQ). Our layer performs no lossy conversion, so the rejection comes from the OS.
    let root = fixture("rawnames-macos");
    let raw = os_name(&[b'x', 0xff, b'.', b'm', b'p', b'3']);
    let err = fs::write(root.join(&raw), b"1234").expect_err("APFS rejects non-UTF8 names");
    assert_eq!(err.raw_os_error(), Some(92), "EILSEQ, got {err:?}");
    // Exact UTF-8 names with distinct byte content are still stored and listed exactly.
    fs::write(root.join("café.mp3"), b"1234").expect("utf-8 name");
    fs::write(root.join("song.mp3"), b"56").expect("second name");
    let store = storage(&root);
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(names(&entries), vec!["café.mp3", "song.mp3"]);
    assert_eq!(entries[0].stat.size, 4);
    cleanup(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_case_insensitivity_is_reported_not_hidden_by_our_layer() {
    // Platform limit, asserted rather than assumed: APFS folds case (and normalisation
    // variants), so names differing only that way cannot both exist. Our layer does not
    // invent a distinction; Linux is case-sensitive and covered by the listing tests.
    let root = fixture("casefold");
    fs::write(root.join("Song.MP3"), b"1234").expect("first");
    fs::write(root.join("song.mp3"), b"56").expect("second overwrites the same entry");
    let store = storage(&root);
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(entries.len(), 1, "APFS folded the two names into one entry");
    assert_eq!(entries[0].stat.size, 2, "last write won");
    // Both spellings address the same file, and the listed name is the OS's own bytes.
    assert_eq!(store.read(&logical("Song.MP3"), Follow::No).unwrap(), b"56");
    assert_eq!(store.read(&logical("song.mp3"), Follow::No).unwrap(), b"56");
    cleanup(&root);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn case_differing_names_are_distinct_on_case_sensitive_filesystems() {
    let root = fixture("case");
    let store = storage(&root);
    for name in ["Song.MP3", "song.mp3", "song.Mp3", "song.mp3.mp3"] {
        fs::write(root.join(name), name.as_bytes().len() as u8 * b'0').expect("write");
    }
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(
        names(&entries),
        vec![
            "Song.MP3",
            "song.MP3",
            "song.Mp3",
            "song.mp3",
            "song.mp3.mp3"
        ],
        "case differences preserved, byte-sorted"
    );
    cleanup(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_folds_unicode_normalisation_variants_and_we_invent_no_distinction() {
    // Platform limit, asserted rather than assumed: APFS normalises names, so the composed
    // and decomposed spellings of "café" are the same entry. Our layer keeps the exact bytes
    // it was given and does not fake a distinction the filesystem cannot store.
    let root = fixture("norm");
    fs::write(root.join("caf\u{e9}.mp3"), b"1234").expect("composed");
    fs::write(root.join("cafe\u{301}.mp3"), b"56").expect("decomposed overwrites it");
    let store = storage(&root);
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(entries.len(), 1, "APFS folded the two spellings");
    assert_eq!(entries[0].stat.size, 2, "last write won");
    assert_eq!(
        store.read(&logical("caf\u{e9}.mp3"), Follow::No).unwrap(),
        b"56"
    );
    cleanup(&root);
}

#[test]
fn names_differing_by_extension_or_prefix_stay_distinct_and_sorted() {
    let root = fixture("names");
    let store = storage(&root);
    for name in ["song.mp3", "song.mp3.mp3", "song.flac", "a.mp3"] {
        write_file(&root, name, b"1234");
    }
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(
        names(&entries),
        vec!["a.mp3", "song.flac", "song.mp3", "song.mp3.mp3"],
        "extension and prefix differences preserved, byte-sorted"
    );
    cleanup(&root);
}

#[test]
fn listing_a_file_or_a_symlink_to_a_file_is_not_a_directory() {
    let root = fixture("notdir");
    let store = storage(&root);
    write_file(&root, "song.mp3", b"123");
    let err = store.list(&logical("song.mp3"), Follow::No).unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::NotADirectory {
                kind: EntryKind::File,
                ..
            }
        ),
        "file → NotADirectory(File), got {err:?}"
    );

    #[cfg(unix)]
    {
        symlink_str("song.mp3", &root.join("alias.mp3"));
        // Follow disabled refuses the terminal link instead of trying to list through it.
        let err = store.list(&logical("alias.mp3"), Follow::No).unwrap_err();
        assert!(
            matches!(err, StorageError::EntryIsSymlink { .. }),
            "symlink-to-file under Follow::No → EntryIsSymlink, got {err}"
        );
        // With following enabled the OS attempts it and reports ENOTDIR, like Go's ReadDir
        // on a symlink-to-file (`open lib/abs.mp3: not a directory`).
        let err = store.list(&logical("alias.mp3"), Follow::Yes).unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::NotADirectory {
                    kind: EntryKind::Symlink,
                    ..
                }
            ),
            "symlink-to-file under Follow::Yes → NotADirectory(Symlink), got {err:?}"
        );
    }

    let err = store.list(&logical("nope"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::NotFound { .. }),
        "missing dir → NotFound, got {err:?}"
    );
    assert!(
        err.to_string().contains("nope"),
        "message names the path: {err}"
    );
    cleanup(&root);
}

// ---------------------------------------------------------------- stat snapshots

#[test]
fn snapshot_reports_exact_size_and_mtime_from_the_file_itself() {
    let root = fixture("stat");
    let store = storage(&root);
    let bytes = b"0123456789abcdef";
    let path = write_file(&root, "song.mp3", bytes);
    let snapshot = store.snapshot(&logical("song.mp3"), Follow::No).unwrap();

    assert_eq!(snapshot.logical, logical("song.mp3"));
    assert!(snapshot.is_regular_file());
    assert_eq!(snapshot.stat.size, bytes.len() as u64, "exact length");
    let raw = fs::metadata(&path).expect("raw stat");
    assert_eq!(
        snapshot.stat.modified.ok(),
        raw.modified().ok(),
        "mtime is the OS mtime"
    );
    assert_eq!(
        snapshot.stat.created,
        raw.created().map_err(|_| BirthTimeUnavailable),
        "birth time passes through unchanged"
    );
    // Not a scan timestamp: changing the file changes the snapshot.
    fs::write(&path, b"longer content").expect("rewrite");
    let changed = store.snapshot(&logical("song.mp3"), Follow::No).unwrap();
    assert_eq!(changed.stat.size, 14);
    assert_ne!(
        changed.stat.modified, snapshot.stat.modified,
        "mtime tracks the file"
    );
    cleanup(&root);
}

#[test]
fn birth_time_availability_is_reported_honestly() {
    let root = fixture("birth");
    let store = storage(&root);
    let path = write_file(&root, "song.mp3", b"abc");
    let raw = fs::metadata(&path).expect("raw stat");
    let snapshot = store.snapshot(&logical("song.mp3"), Follow::No).unwrap();

    match (raw.created(), snapshot.stat.created) {
        (Ok(raw_birth), Ok(birth)) => assert_eq!(birth, raw_birth, "birth time passes through"),
        (Err(_), Err(unavailable)) => {
            // Platform/filesystem reports no birth time: surfaced, never fabricated.
            assert!(unavailable.to_string().contains("birth time"));
        }
        (os, ours) => panic!(
            "birth time availability mismatch: OS {:?} vs ours {:?}",
            os.map(|_| "Ok"),
            ours.map(|_| "Ok")
        ),
    }

    // Go substitutes time.Now() when no birth time exists (`core/storage/local/local.go:120-128`); that
    // fallback is deliberately not ported, so the unavailable state is representable and
    // never silently replaced. (The no-fabrication rule itself is asserted in the private
    // `stat_tests` module, since `Metadata::created()` never fails on macOS/APFS.)
    let reported = store
        .snapshot(&logical("song.mp3"), Follow::No)
        .unwrap()
        .stat;
    assert_eq!(
        reported.created,
        raw.created().map_err(|_| BirthTimeUnavailable)
    );
    assert_eq!(
        reported
            .created
            .map(|_| "reported")
            .unwrap_or("unavailable"),
        raw.created().map(|_| "reported").unwrap_or("unavailable"),
        "availability matches the OS exactly, with no invented timestamp"
    );
    cleanup(&root);
}

#[test]
fn missing_paths_are_not_found_and_name_the_logical_path() {
    let root = fixture("missing");
    let store = storage(&root);
    let missing = logical("nope/deeper.mp3");

    for err in [
        store.snapshot(&missing, Follow::No).unwrap_err(),
        store.snapshot(&missing, Follow::Yes).unwrap_err(),
        store.read(&missing, Follow::Yes).unwrap_err(),
    ] {
        assert!(matches!(err, StorageError::NotFound { .. }), "got {err:?}");
        assert!(
            err.to_string().contains("nope/deeper.mp3"),
            "names the path: {err}"
        );
        assert!(err.source().is_some(), "OS error is preserved");
    }
    assert!(!store.exists(&missing));
    cleanup(&root);
}

// ---------------------------------------------------------------- reads

#[test]
fn read_returns_exact_bytes_including_binary_content() {
    let root = fixture("read");
    let store = storage(&root);
    let bytes: Vec<u8> = (0u8..=255).collect();
    write_file(&root, "id3.bin", &bytes);
    assert_eq!(store.read(&logical("id3.bin"), Follow::No).unwrap(), bytes);

    // A directory is not a file.
    let err = store.read(&LogicalPath::root(), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::Io { .. }),
        "dir read → Io, got {err:?}"
    );

    // An empty file reads as empty rather than erroring.
    write_file(&root, "empty.mp3", b"");
    assert_eq!(
        store.read(&logical("empty.mp3"), Follow::No).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        store
            .snapshot(&logical("empty.mp3"), Follow::No)
            .unwrap()
            .stat
            .size,
        0
    );
    cleanup(&root);
}

// ---------------------------------------------------------------- symlink classification

#[cfg(unix)]
#[test]
fn follow_disabled_classifies_the_link_itself_with_the_link_s_own_size() {
    let root = fixture("lstat");
    let store = storage(&root);
    let target = write_file(&root, "real.mp3", b"1234567890");
    symlink_str("real.mp3", &root.join("link.mp3"));
    let link_length = "real.mp3".len() as u64; // a link stores its target path

    let snapshot = store.snapshot(&logical("link.mp3"), Follow::No).unwrap();
    assert!(snapshot.is_symlink(), "classified as a symlink, not a file");
    assert_eq!(
        snapshot.logical,
        logical("link.mp3"),
        "logical name preserved"
    );
    assert_eq!(
        snapshot.stat.size, link_length,
        "lstat size is the link's own"
    );
    assert_ne!(snapshot.stat.size, 10, "not the target's size");

    let followed = store.snapshot(&logical("link.mp3"), Follow::Yes).unwrap();
    assert!(followed.is_regular_file());
    assert_eq!(followed.stat.size, 10, "endpoint size when following");
    assert_eq!(followed.logical, logical("link.mp3"));

    // Reading must not silently traverse the link while following is disabled.
    let err = store.read(&logical("link.mp3"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "follow=false refuses to read through a link, got {err:?}"
    );
    assert!(
        err.to_string().contains("link.mp3"),
        "names the link: {err}"
    );
    assert_eq!(
        store.read(&logical("link.mp3"), Follow::Yes).unwrap(),
        b"1234567890",
        "following reads the endpoint"
    );
    // A plain file reads under both policies.
    assert_eq!(
        store.read(&logical("real.mp3"), Follow::No).unwrap(),
        b"1234567890"
    );

    // list() follows the caller's policy per entry.
    let no_follow = store.list(&LogicalPath::root(), Follow::No).unwrap();
    let link = no_follow
        .iter()
        .find(|e| e.path == logical("link.mp3"))
        .expect("link listed");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.stat.size, link_length);
    let yes_follow = store.list(&LogicalPath::root(), Follow::Yes).unwrap();
    let followed_entry = yes_follow
        .iter()
        .find(|e| e.path == logical("link.mp3"))
        .expect("link listed");
    assert_eq!(followed_entry.kind, EntryKind::File);
    assert_eq!(followed_entry.stat.size, 10);

    // read_link is the raw target (fs.ReadLink)…
    assert_eq!(
        store.read_link(&logical("link.mp3")).unwrap(),
        PathBuf::from("real.mp3")
    );
    // …and a non-link is an OS error rather than a silent "not a link".
    assert!(store.read_link(&logical("real.mp3")).is_err());

    // resolve(Follow::No) reports the link without traversing it.
    match store.resolve(&logical("link.mp3"), Follow::No).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::Symlink);
            assert_eq!(endpoint.hops, 0, "no chain walked");
            assert_eq!(endpoint.logical, logical("link.mp3"));
            assert_eq!(
                endpoint.target,
                root.join("link.mp3"),
                "the link's own path"
            );
        }
        ResolvedTarget::LinkLoop { .. } => panic!("nothing was chained"),
    }
    // The real file behind the link is untouched and still 10 bytes.
    assert_eq!(fs::metadata(&target).unwrap().len(), 10);
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_link_chain_resolves_to_its_final_endpoint() {
    let root = fixture("chain");
    let store = storage(&root);
    write_file(&root, "real.mp3", b"payload");
    symlink_str("real.mp3", &root.join("l0.mp3"));
    symlink_str("l0.mp3", &root.join("l1.mp3"));
    symlink_str("l1.mp3", &root.join("l2.mp3"));

    match store.resolve(&logical("l2.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(endpoint.hops, 3, "three links walked");
            assert_eq!(
                endpoint.logical,
                logical("l2.mp3"),
                "logical name preserved"
            );
            assert_eq!(endpoint.target, root.join("real.mp3"));
            assert_eq!(
                store.read(&logical("l2.mp3"), Follow::Yes).unwrap(),
                b"payload"
            );
        }
        ResolvedTarget::LinkLoop { hops, .. } => panic!("expected endpoint, looped at {hops}"),
    }
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn relative_link_targets_resolve_against_the_link_directory() {
    let root = fixture("relative");
    let store = storage(&root);
    fs::create_dir_all(root.join("sub")).expect("mkdir");
    let target = write_file(&root, "real.mp3", b"payload");
    // Relative target stored under sub/, i.e. sub/../real.mp3 (filepath/symlink.go rule).
    symlink_str("../real.mp3", &root.join("sub").join("rel.mp3"));

    match store.resolve(&logical("sub/rel.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.target, target);
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(endpoint.logical, logical("sub/rel.mp3"));
        }
        ResolvedTarget::LinkLoop { hops, .. } => panic!("expected endpoint, looped at {hops}"),
    }
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn absolute_targets_outside_the_root_resolve_and_keep_the_logical_name() {
    let root = fixture("outside");
    let store = storage(&root);
    let outside = fixture("outside-target");
    let target = write_file(&outside, "real.mp3", b"outside payload");
    symlink(&target, &root.join("alias.mp3"));

    match store.resolve(&logical("alias.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            // No containment claim: the endpoint is reported and the caller decides
            // (unresolved SC-01/SC-02 authorization decision).
            // The target is fully resolved, so a symlinked temp-dir prefix (`/var` →
            // `private/var` on macOS) is rewritten here exactly as Go's EvalSymlinks
            // rewrites it; the logical path is what stays unchanged.
            assert_eq!(
                endpoint.target,
                target.canonicalize().expect("canonicalize target")
            );
            assert!(
                !endpoint.target.starts_with(&root),
                "target really is outside"
            );
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(
                endpoint.logical,
                logical("alias.mp3"),
                "logical path unchanged"
            );
            // Reachable through the logical path too, like Go's os.DirFS.
            assert_eq!(
                store.read(&logical("alias.mp3"), Follow::Yes).unwrap(),
                b"outside payload"
            );
        }
        ResolvedTarget::LinkLoop { hops, .. } => panic!("expected endpoint, looped at {hops}"),
    }
    cleanup(&root);
    cleanup(&outside);
}

#[cfg(unix)]
#[test]
fn broken_links_are_classified_without_faking_an_endpoint() {
    let root = fixture("broken");
    let store = storage(&root);
    symlink_str("does-not-exist.mp3", &root.join("gone.mp3"));

    // lstat sees the link (Go's fs.Lstat succeeds).
    let snapshot = store.snapshot(&logical("gone.mp3"), Follow::No).unwrap();
    assert!(snapshot.is_symlink());
    assert_eq!(
        store.read_link(&logical("gone.mp3")).unwrap(),
        PathBuf::from("does-not-exist.mp3")
    );

    // Following finds no endpoint (Go's fs.Stat → ErrNotExist).
    let err = store
        .snapshot(&logical("gone.mp3"), Follow::Yes)
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound { .. }), "got {err:?}");

    match store.resolve(&logical("gone.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::Missing, "no endpoint exists");
            assert_eq!(endpoint.hops, 1);
            assert_eq!(endpoint.logical, logical("gone.mp3"));
        }
        ResolvedTarget::LinkLoop { .. } => panic!("a broken link is not a loop"),
    }

    // read must not invent content.
    assert!(store.read(&logical("gone.mp3"), Follow::Yes).is_err());

    // list(Follow::Yes) keeps a broken entry visible as a link, neither dropping it nor
    // claiming it is a file.
    let entries = store.list(&LogicalPath::root(), Follow::Yes).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EntryKind::Symlink);
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_link_loop_is_bounded_and_reported() {
    let root = fixture("loop");
    let store = storage(&root);
    symlink_str("b.mp3", &root.join("a.mp3"));
    symlink_str("a.mp3", &root.join("b.mp3"));

    match store.resolve(&logical("a.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::LinkLoop {
            logical: looped,
            hops,
        } => {
            assert_eq!(looped, logical("a.mp3"));
            assert!(
                hops > MAX_SYMLINK_HOPS,
                "walk stopped at the {MAX_SYMLINK_HOPS}-link bound, got {hops}"
            );
        }
        ResolvedTarget::Endpoint(endpoint) => panic!("loop reported as {endpoint:?}"),
    }
    // Nothing was traversed, so nothing can be read.
    assert!(store.read(&logical("a.mp3"), Follow::Yes).is_err());
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_long_chain_is_stopped_by_the_same_bound_go_uses() {
    let root = fixture("longchain");
    let store = storage(&root);
    write_file(&root, "real.mp3", b"payload");
    // One more link than the bound, so the deepest entry cannot resolve.
    let links = MAX_SYMLINK_HOPS + 5;
    symlink_str("real.mp3", &root.join("l0.mp3"));
    for i in 1..links {
        symlink_str(
            &format!("l{}.mp3", i - 1),
            &root.join(format!("l{}.mp3", i)),
        );
    }

    let deepest = format!("l{}.mp3", links - 1);
    match store.resolve(&logical(&deepest), Follow::Yes).unwrap() {
        ResolvedTarget::LinkLoop { hops, .. } => assert!(hops > MAX_SYMLINK_HOPS, "hops {hops}"),
        ResolvedTarget::Endpoint(endpoint) => panic!("bound not applied: {endpoint:?}"),
    }

    // A chain that fits inside the bound still resolves, with the hop count reported.
    match store.resolve(&logical("l254.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(endpoint.hops, 255);
            assert_eq!(endpoint.target, root.join("real.mp3"));
        }
        ResolvedTarget::LinkLoop { hops, .. } => panic!("fits the bound, looped at {hops}"),
    }
    cleanup(&root);
}

// ---------------------------------------------------------------- ancestor policy

#[cfg(unix)]
#[test]
fn follow_disabled_never_traverses_a_symlinked_ancestor() {
    let root = fixture("ancestor");
    let store = storage(&root);
    let outside = fixture("ancestor-target");
    write_file(&outside, "song.mp3", b"outside bytes");
    fs::create_dir_all(outside.join("inner")).expect("mkdir");
    write_file(&outside.join("inner"), "deeper.mp3", b"deeper bytes");
    symlink(&outside, &root.join("linkdir"));

    let deep = logical("linkdir/song.mp3");
    let deeper = logical("linkdir/inner/deeper.mp3");

    // snapshot, read and list all refuse to traverse the link, at any depth.
    let rejected = vec![
        store.snapshot(&deep, Follow::No).unwrap_err(),
        store.read(&deep, Follow::No).unwrap_err(),
        store.snapshot(&deeper, Follow::No).unwrap_err(),
        store.read(&deeper, Follow::No).unwrap_err(),
        store
            .list(&logical("linkdir/inner"), Follow::No)
            .unwrap_err(),
    ];
    for result in rejected {
        match result {
            StorageError::AncestorIsSymlink { path, target } => {
                assert_eq!(path, logical("linkdir"), "the link itself is named");
                assert_eq!(target, outside);
            }
            other => panic!("expected AncestorIsSymlink, got {other:?}"),
        }
    }

    // The terminal component is refused too, so the alias target is never enumerated
    // with following disabled (Go's os.DirFS would list it).
    let err = store.list(&logical("linkdir"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "terminal dir symlink under Follow::No → EntryIsSymlink, got {err:?}"
    );

    // The same paths are reachable when following is explicitly enabled (Go behaviour).
    assert_eq!(store.read(&deep, Follow::Yes).unwrap(), b"outside bytes");
    assert_eq!(store.read(&deeper, Follow::Yes).unwrap(), b"deeper bytes");
    assert!(store.list(&logical("linkdir/inner"), Follow::Yes).is_ok());
    cleanup(&root);
    cleanup(&outside);
}

#[cfg(unix)]
#[test]
fn follow_disabled_reports_a_non_directory_ancestor() {
    let root = fixture("ancestor-file");
    let store = storage(&root);
    write_file(&root, "song.mp3", b"x");
    let err = store
        .snapshot(&logical("song.mp3/deeper.mp3"), Follow::No)
        .unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::AncestorNotDirectory {
                kind: EntryKind::File,
                ..
            }
        ),
        "got {err:?}"
    );
    // Following surfaces the OS ENOTDIR instead, as Go's walkSymlinks does.
    let err = store
        .snapshot(&logical("song.mp3/deeper.mp3"), Follow::Yes)
        .unwrap_err();
    assert!(matches!(err, StorageError::Io { .. }), "got {err:?}");
    cleanup(&root);
}

// ---------------------------------------------------------------- endpoint classification

#[cfg(unix)]
#[test]
fn classification_uses_the_real_endpoint_never_the_link_name() {
    let root = fixture("endpoint");
    let store = storage(&root);

    // A link named like audio whose endpoint is not audio.
    write_file(&root, "notes.txt", b"not audio at all");
    symlink_str("notes.txt", &root.join("evil.mp3"));
    // A link whose name looks non-audio whose endpoint is audio.
    write_file(&root, "song.flac", b"flac bytes");
    symlink_str("song.flac", &root.join("cover.jpg"));

    match store.resolve(&logical("evil.mp3"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(endpoint.target, root.join("notes.txt"), "actual endpoint");
            assert_eq!(
                endpoint.logical,
                logical("evil.mp3"),
                "logical name preserved"
            );
        }
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }
    match store.resolve(&logical("cover.jpg"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::File);
            assert_eq!(endpoint.target, root.join("song.flac"));
            assert_eq!(endpoint.logical, logical("cover.jpg"));
        }
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }

    // Metadata comes from the endpoint under Follow::Yes, while the logical name is kept.
    let snapshot = store.snapshot(&logical("evil.mp3"), Follow::Yes).unwrap();
    assert_eq!(snapshot.stat.size, 16, "endpoint size, not link size");
    assert_eq!(snapshot.logical, logical("evil.mp3"));
    assert_eq!(
        snapshot.stat.size,
        fs::metadata(root.join("notes.txt")).unwrap().len()
    );

    // A directory endpoint is reported as a directory, not a file.
    let dir_target = fixture("dir-target");
    write_file(&dir_target, "inner.mp3", b"x");
    symlink(&dir_target, &root.join("dirlink"));
    match store.resolve(&logical("dirlink"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => assert_eq!(endpoint.kind, EndpointKind::Dir),
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }
    assert_eq!(
        store.list(&logical("dirlink"), Follow::Yes).unwrap().len(),
        1
    );

    // An audio-*named* link whose endpoint is a directory must report Dir, and one whose
    // endpoint is missing must report Missing. These catch a name-based shortcut: with
    // `.mp3` in the link name, kind-from-name would claim File for both.
    symlink(&dir_target, &root.join("evil-dir.mp3"));
    match store
        .resolve(&logical("evil-dir.mp3"), Follow::Yes)
        .unwrap()
    {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(
                endpoint.kind,
                EndpointKind::Dir,
                "kind comes from the endpoint, not the .mp3 link name"
            );
            assert_eq!(endpoint.logical, logical("evil-dir.mp3"));
        }
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }
    // Reading through a directory endpoint fails, and is never reported as success.
    assert!(store.read(&logical("evil-dir.mp3"), Follow::Yes).is_err());

    symlink_str("nowhere.bin", &root.join("evil-broken.mp3"));
    match store
        .resolve(&logical("evil-broken.mp3"), Follow::Yes)
        .unwrap()
    {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(
                endpoint.kind,
                EndpointKind::Missing,
                "a .mp3 link with no endpoint is Missing, not File"
            );
        }
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }
    cleanup(&root);
    cleanup(&dir_target);
}

#[cfg(unix)]
#[test]
fn nothing_is_written_by_read_only_operations() {
    let root = fixture("readonly");
    let store = storage(&root);
    write_file(&root, "song.mp3", b"abc");
    symlink_str("song.mp3", &root.join("alias.mp3"));

    for path in [logical("song.mp3"), logical("alias.mp3")] {
        let _ = store.read(&path, Follow::Yes).unwrap();
        let _ = store.resolve(&path, Follow::Yes).unwrap();
        let _ = store.snapshot(&path, Follow::Yes).unwrap();
    }
    let entries = store.list(&LogicalPath::root(), Follow::Yes).unwrap();
    assert_eq!(
        names(&entries),
        vec!["alias.mp3", "song.mp3"],
        "tree unchanged"
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn two_aliases_of_one_file_keep_their_logical_distinction() {
    let root = fixture("aliases");
    let store = storage(&root);
    let target = write_file(&root, "real.mp3", b"payload");
    symlink(&target, &root.join("alias-a.mp3"));
    fs::create_dir_all(root.join("nested")).expect("mkdir");
    symlink(&target, &root.join("nested").join("alias-b.mp3"));

    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(names(&entries), vec!["alias-a.mp3", "nested", "real.mp3"]);

    let a = store
        .snapshot(&logical("alias-a.mp3"), Follow::Yes)
        .unwrap();
    let b = store
        .snapshot(&logical("nested/alias-b.mp3"), Follow::Yes)
        .unwrap();
    assert_ne!(a.logical, b.logical, "logical paths stay distinct");
    assert_eq!(a.stat.size, b.stat.size, "same underlying bytes");
    assert_eq!(a.logical, logical("alias-a.mp3"));
    assert_eq!(b.logical, logical("nested/alias-b.mp3"));

    let resolved_a = store.resolve(&logical("alias-a.mp3"), Follow::Yes).unwrap();
    let resolved_b = store
        .resolve(&logical("nested/alias-b.mp3"), Follow::Yes)
        .unwrap();
    match (resolved_a, resolved_b) {
        (ResolvedTarget::Endpoint(ea), ResolvedTarget::Endpoint(eb)) => {
            assert_eq!(ea.target, eb.target, "both land on the same endpoint");
            // Absolute link targets are fully resolved, so a symlinked temp prefix is
            // rewritten (`/var` → `private/var`) exactly as Go's EvalSymlinks does.
            assert_eq!(
                ea.target,
                target.canonicalize().expect("canonicalize target")
            );
            assert_eq!(ea.logical, logical("alias-a.mp3"));
            assert_eq!(eb.logical, logical("nested/alias-b.mp3"));
            // `hops` counts every link walked, including symlinked components of the
            // absolute target path (`/var` → `private/var` on macOS), so it is a lower
            // bound here rather than exactly the one alias link.
            assert!(ea.hops >= 1, "the alias link itself is walked: {}", ea.hops);
            assert_eq!(
                ea.hops, eb.hops,
                "both aliases walk the same number of links"
            );
        }
        other => panic!("expected two endpoints, got {other:?}"),
    }
    cleanup(&root);
}

// ---------------------------------------------------------------- terminal link refusal

#[cfg(unix)]
#[test]
fn a_directory_alias_below_the_root_is_refused_when_following_is_disabled() {
    let root = fixture("diralias");
    let store = storage(&root);
    let target = root.join("real");
    fs::create_dir_all(&target).expect("mkdir");
    write_file(&target, "song.mp3", b"payload");
    write_file(&target, "art.jpg", b"img");
    symlink(&target, &root.join("alias"));

    // Follow disabled must not enumerate the alias target at all.
    let err = store.list(&logical("alias"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "alias dir under Follow::No → EntryIsSymlink, got {err:?}"
    );
    // The alias itself is the named path, not its target.
    if let StorageError::EntryIsSymlink { ref path, .. } = err {
        assert_eq!(*path, logical("alias"));
    }
    assert!(
        err.to_string().contains("alias"),
        "message names the alias: {err}"
    );
    // snapshot still reports the link (fs.Lstat), and read_link gives the raw target.
    assert!(store
        .snapshot(&logical("alias"), Follow::No)
        .unwrap()
        .is_symlink());
    // read_link returns the raw stored target text (fs.ReadLink), here an absolute path.
    assert_eq!(store.read_link(&logical("alias")).unwrap(), target);

    // Following explicitly lists the endpoint's contents under the alias's logical paths.
    let entries = store.list(&logical("alias"), Follow::Yes).unwrap();
    assert_eq!(names(&entries), vec!["alias/art.jpg", "alias/song.mp3"]);
    assert_eq!(entries[1].kind, EntryKind::File);
    assert_eq!(
        store.read(&logical("alias/song.mp3"), Follow::Yes).unwrap(),
        b"payload"
    );

    // A *broken* directory alias is refused too, not reported as an empty listing.
    symlink_str("nowhere", &root.join("broken-dir"));
    let err = store.list(&logical("broken-dir"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "broken dir alias under Follow::No → EntryIsSymlink, got {err:?}"
    );
    let err = store.list(&logical("broken-dir"), Follow::Yes).unwrap_err();
    assert!(
        matches!(err, StorageError::NotFound { .. }),
        "broken dir alias under Follow::Yes → NotFound, got {err:?}"
    );

    // Nested alias reached from a real parent: refused at the link, listed when following.
    fs::create_dir_all(root.join("parent")).expect("mkdir");
    symlink(&target, &root.join("parent").join("via"));
    let err = store.list(&logical("parent/via"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "nested alias under Follow::No → EntryIsSymlink, got {err:?}"
    );
    assert_eq!(
        names(&store.list(&logical("parent/via"), Follow::Yes).unwrap()),
        vec!["parent/via/art.jpg", "parent/via/song.mp3"]
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn a_directory_alias_whose_target_is_outside_the_root_is_refused_when_following_is_disabled() {
    let root = fixture("diralias-outside");
    let store = storage(&root);
    let outside = fixture("diralias-target");
    write_file(&outside, "elsewhere.mp3", b"outside");
    symlink(&outside, &root.join("escape"));

    // Enumerating an out-of-root target through an alias must not happen with disabled
    // following, at any depth. (Best-effort path-name policy, not containment.)
    for path in ["escape", "escape/elsewhere.mp3"] {
        let err = store.list(&logical(path), Follow::No).unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::EntryIsSymlink { .. } | StorageError::AncestorIsSymlink { .. }
            ),
            "{path} must be refused, got {err:?}"
        );
    }
    assert!(store
        .read(&logical("escape/elsewhere.mp3"), Follow::No)
        .is_err());
    // Explicitly following reaches it, and reports the out-of-root endpoint (no
    // authorization decision is made here).
    let entries = store.list(&logical("escape"), Follow::Yes).unwrap();
    assert_eq!(names(&entries), vec!["escape/elsewhere.mp3"]);
    match store.resolve(&logical("escape"), Follow::Yes).unwrap() {
        ResolvedTarget::Endpoint(endpoint) => {
            assert_eq!(endpoint.kind, EndpointKind::Dir);
            assert_eq!(
                endpoint.target,
                outside.canonicalize().expect("canonicalize")
            );
            assert_eq!(
                endpoint.logical,
                logical("escape"),
                "logical name preserved"
            );
        }
        ResolvedTarget::LinkLoop { .. } => panic!("no loop here"),
    }
    cleanup(&root);
    cleanup(&outside);
}

#[cfg(unix)]
#[test]
fn a_configured_root_that_is_a_symlink_is_trusted_configuration() {
    // Deliberate policy (matching Go, which stats/os.DirFSes the configured path): the
    // root is trusted configuration and is followed even under Follow::No. Components
    // *below* the root are still guarded. Documented in src/storage.rs; asserted here.
    let real = fixture("root-symlink-target");
    write_file(&real, "song.mp3", b"payload");
    fs::create_dir_all(real.join("inner")).expect("mkdir");
    write_file(&real.join("inner"), "deeper.mp3", b"deeper");

    let alias_parent = fixture("root-symlink-parent");
    let alias = alias_parent.join("library");
    symlink(&real, &alias);
    let store = LocalDir::open(&alias.display().to_string()).expect("open alias root");

    // Followed under Follow::No: the root itself is not treated as an alias.
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(names(&entries), vec!["inner", "song.mp3"]);
    assert_eq!(
        store
            .read(&logical("inner/deeper.mp3"), Follow::No)
            .unwrap(),
        b"deeper"
    );
    assert_eq!(
        store.root(),
        alias.as_path(),
        "root kept exactly as configured"
    );

    // A symlink below that root is still refused with following disabled.
    symlink(&real, &real.join("self"));
    let err = store.list(&logical("self"), Follow::No).unwrap_err();
    assert!(
        matches!(err, StorageError::EntryIsSymlink { .. }),
        "below-root link still refused, got {err:?}"
    );
    // Listing the alias endpoint under the alias path also surfaces the alias itself
    // (a nested alias), which is exactly why disabled following refuses it.
    assert_eq!(
        names(&store.list(&logical("self"), Follow::Yes).unwrap()),
        vec!["self/inner", "self/self", "self/song.mp3"]
    );
    // Under Follow::Yes entries are classified by their endpoint, so the nested alias
    // reports Dir; under Follow::No the same entry reports Symlink (fs.Lstat semantics).
    let following = store.list(&logical("self"), Follow::Yes).unwrap();
    assert_eq!(following[1].kind, EntryKind::Dir, "endpoint classification");
    let not_following = store.list(&LogicalPath::root(), Follow::No).unwrap();
    let self_entry = not_following
        .iter()
        .find(|entry| entry.path == logical("self"))
        .expect("self entry");
    assert_eq!(self_entry.kind, EntryKind::Symlink, "lstat classification");
    cleanup(&real);
    cleanup(&alias_parent);
}

// ---------------------------------------------------------------- URI to real files

#[test]
fn a_percent_encoded_file_url_names_a_real_directory_with_reserved_bytes() {
    // Fixture names containing the bytes Go's LocalPathToURL would escape.
    let parent = fixture("uri");
    let dir = parent.join("50% off #1");
    fs::create_dir_all(&dir).expect("mkdir");
    write_file(&dir, "song.mp3", b"payload");
    let encoded = format!("file://{}", percent_encode(&dir));

    let store = LocalDir::open(&encoded).expect("open escaped url");
    assert_eq!(store.root(), dir, "decoded once to the real path");
    assert_eq!(
        store.read(&logical("song.mp3"), Follow::No).unwrap(),
        b"payload"
    );
    assert_eq!(
        names(&store.list(&LogicalPath::root(), Follow::No).unwrap()),
        vec!["song.mp3"]
    );

    // The bare-path constructor takes the same bytes with no decoding at all.
    let verbatim = LocalDir::from_path(dir.as_os_str()).expect("from_path");
    assert_eq!(verbatim.root(), dir);
    assert_eq!(
        verbatim.read(&logical("song.mp3"), Follow::No).unwrap(),
        b"payload"
    );

    // A path whose literal text looks escaped must NOT be decoded: the file named
    // "a%20b" stays itself, proving the two constructors differ on purpose.
    let literal = parent.join("a%20b");
    fs::create_dir_all(&literal).expect("mkdir");
    write_file(&literal, "keep.mp3", b"literal");
    let via_bare = LocalDir::open(&literal.display().to_string()).expect("bare path");
    assert_eq!(via_bare.root(), literal, "bare path is never decoded");
    assert_eq!(
        via_bare.read(&logical("keep.mp3"), Follow::No).unwrap(),
        b"literal"
    );

    // A URL decoding to that same name reaches it too (single decode of %2520).
    let via_url =
        LocalDir::open(&format!("file://{}", percent_encode(&literal))).expect("escaped url");
    assert_eq!(
        via_url.root(),
        literal,
        "%2520 decodes once to the literal text a%20b, not to a space"
    );
    assert_eq!(
        via_url.read(&logical("keep.mp3"), Follow::No).unwrap(),
        b"literal"
    );
    cleanup(&parent);
}

#[test]
fn an_uppercase_file_scheme_url_names_a_real_temp_directory() {
    // Regression control: with case-sensitive scheme dispatch this fails before any I/O,
    // because `FILE://…` is rejected outright. Go accepts it — url.Parse lowercases the
    // scheme (verified with go1.27.4: "FILE:///tmp/x" → scheme "file", path "/tmp/x").
    let parent = fixture("scheme-case");
    let dir = parent.join("Music");
    fs::create_dir_all(&dir).expect("mkdir");
    write_file(&dir, "song.mp3", b"payload");

    for scheme in ["file", "FILE", "File", "FiLe"] {
        let store = LocalDir::open(&format!("{scheme}://{}", percent_encode(&dir)))
            .unwrap_or_else(|err| panic!("open {scheme} url: {err}"));
        assert_eq!(store.root(), dir, "{scheme} decoded to the real path");
        assert_eq!(
            store.read(&logical("song.mp3"), Follow::No).unwrap(),
            b"payload",
            "{scheme} reads through the real path"
        );
        assert_eq!(
            names(&store.list(&LogicalPath::root(), Follow::No).unwrap()),
            vec!["song.mp3"],
            "{scheme} lists the real path"
        );
        // Case folding covers only the scheme: the decoded path keeps exact bytes, so the
        // directory is *not* matched case-insensitively by name.
        assert_eq!(store.root().file_name().unwrap(), "Music");
    }
    cleanup(&parent);
}

#[test]
fn scheme_and_authority_case_folding_does_not_widen_anything_else() {
    // Unsupported schemes stay typed errors in every spelling, and folding is ASCII-only.
    for uri in ["S3://bucket/x", "HTTPS://h/x", "SMB://s/m", "sFtp://h/x"] {
        let err = LocalDir::open(uri).unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedScheme { .. }),
            "{uri} → {err:?}"
        );
    }
    let err = LocalDir::open("\u{fb01}:///tmp/x.mp3").unwrap_err();
    assert!(
        matches!(err, StorageError::UnsupportedScheme { .. }),
        "the fi ligature is not the file scheme, got {err:?}"
    );

    // Authority folding is case only: ports, userinfo and other hosts remain rejected.
    for uri in [
        "file://LOCALHOST:80/x.mp3",
        "file://LocalHost:80/x.mp3",
        "file://u@LOCALHOST/x.mp3",
        "file://LOCALHOST.example.com/x.mp3",
        "file://EXAMPLE.com/x.mp3",
    ] {
        let err = LocalDir::open(uri).unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedAuthority { .. }),
            "{uri} must stay rejected, got {err:?}"
        );
    }
    // Bare OS paths are untouched by any of this: raw bytes, including case, are literal.
    let mixed = "/tmp/FILE/Music";
    assert_eq!(LocalDir::open(mixed).unwrap().root(), Path::new(mixed));
}

/// Percent-encode everything except unreserved bytes: the conservative escaping Go's
/// `url.PathEscape` produces (verified against net/url go1.27.4: "50% off #1" ->
/// 50%25%20off%20%231, "a%20b" -> a%2520b, u.mp3 -> %C3%BC.mp3).
fn percent_encode(path: &Path) -> String {
    let mut out = String::new();
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(not(unix))]
#[test]
fn non_unix_platforms_cover_only_platform_independent_guarantees() {
    // Windows symlink/path behaviour is NOT exercised: coverage gap, reported not assumed.
    assert!(LocalDir::open("s3://bucket/x").is_err());
    let root = fixture("win-basic");
    let store = storage(&root);
    write_file(&root, "song.mp3", b"abc");
    let entries = store.list(&LogicalPath::root(), Follow::No).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EntryKind::File);
    assert_eq!(
        store.read(&logical("song.mp3"), Follow::No).unwrap(),
        b"abc"
    );
    assert!(LogicalPath::parse("..").is_err());
    cleanup(&root);
}
