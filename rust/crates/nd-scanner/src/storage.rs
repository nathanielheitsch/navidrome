//! `core/storage` — the **local-filesystem subset** only: real local `stat`/`read`/`list`
//! plus explicit symlink classification. A deliberately small slice of SC-02
//! (`docs/rust-port/subplans/scanner.md`): **not** `MusicFS`/`ReadTags`
//! (`core/storage/interface.go:15-18`), not registry dispatch (`core/storage/storage.go:53-79`), not the
//! walker/ignore/hash/cancellation surface, not the watcher, not SC-02 completion.
//! Standard library only: no dependency decision is implied here.
//!
//! ## This is not a security boundary
//!
//! Every check is a path-name check followed by a separate `std::fs` call (check-then-use);
//! there is no `openat`/`O_NOFOLLOW`/descriptor-relative I/O, so a concurrent renaming
//! process can defeat [`Follow::No`]. The policy is best-effort defence against ordinary
//! aliasing. There is **no** trusted-target authorization (unresolved SC-01 decision) and
//! no root containment: [`LocalDir::resolve`] reports endpoints outside the root and the
//! caller decides what to do with them.
//!
//! **Configured-root policy:** the root is trusted configuration. A root that is itself a
//! symlink **is followed**, by every operation including [`Follow::No`] — Go stats and
//! `os.DirFS`es the configured path without rejecting it (`core/storage/local/local.go:53-59`). Only
//! components **below** the root are guarded; the root path is never canonicalised or
//! re-derived from a request.
//!
//! ## Parity notes (Go → here)
//!
//! * `core/storage/storage.go:13` `LocalSchemaID = "file"` → [`LOCAL_SCHEMA_ID`]; `For` rejecting an
//!   unregistered schema (`core/storage/storage.go:76-78`) → [`StorageError::UnsupportedScheme`]. Only
//!   `file`: no remote code path exists, so a remote URI can never appear to work.
//! * `core/storage/storage.go:32-47` `LocalPathToURL` escapes components with `url.PathEscape` and `For`
//!   hands `url.Parse`'s decoded `u.Path` to the constructor — escape → parse is the
//!   identity. Reproduced exactly: [`LocalDir::open`] percent-decodes a `file` URL **once**
//!   ([`decode_path`]); a bare path and [`LocalDir::from_path`] take bytes **verbatim**, so
//!   a literal `%`, `#` or `?` in a name survives. Verified against net/url (go1.27.4):
//!   `%25`→`%`, `%2520`→`%20`, `%23`→`#`, `%3F`→`?`, `+`→`+` (no `+`-space decoding in
//!   paths), `%C3%BC`→`ü`, `%2F`→`/`, `%zz`→`invalid URL escape`; `u.Fragment`/`u.RawQuery`
//!   are never part of `u.Path`.
//! * `core/storage/local/local.go:61-88` `localFS` implements `fs.ReadLinkFS` → [`LocalDir::snapshot`] and
//!   [`LocalDir::read_link`] are always no-follow; [`LocalDir::list`] reports each entry's
//!   own lstat kind under [`Follow::No`], endpoint kind under [`Follow::Yes`].
//! * `ResolveSymlink` (`core/storage/interface.go:24-26`, `core/storage/local/local.go:72-77`) → [`LocalDir::resolve`]
//!   [`Follow::Yes`]. Go's `filepath.EvalSymlinks` stops after 255 walked links
//!   (`path/filepath/symlink.go`); [`MAX_SYMLINK_HOPS`] mirrors it and is tested to it
//!   (255 resolve, 256 fail). Go returns one plain error; here a loop is
//!   [`ResolvedTarget::LinkLoop`] and a chain ending on a missing component is
//!   [`EndpointKind::Missing`], because classification needs the distinction. The logical
//!   name is always preserved; the target may lie outside the root.
//! * `core/storage/local/local.go:120-128` `BirthTime()` falls back to `time.Now()`. Not ported:
//!   [`FileStat::created`] reports availability honestly; [`FileStat::modified`] is never
//!   filled with an epoch either.
//!
//! ## Deliberate differences (documented, not silent)
//!
//! * [`Follow::No`] refuses the **terminal** component too, not only ancestors:
//!   [`LocalDir::list`] and [`LocalDir::read`] reject a symlink entry
//!   ([`StorageError::EntryIsSymlink`]) rather than enumerate or read through it —
//!   including broken links and links to files. [`LocalDir::snapshot`] and
//!   [`LocalDir::read_link`] instead *report* the link (lstat), and
//!   [`LocalDir::resolve`] [`Follow::No`] classifies it. Go's `os.DirFS` follows every
//!   component; SC-02 item 4 requires the refusal, so this half is strictly safer. The
//!   configured root is the exempt case (above).
//! * Invalid logical paths are rejected for every operation, not only `ResolveSymlink`
//!   (Go checks `fs.ValidPath` there). Its `file`/`dir`/`symlink` name checks are not
//!   ported: they stop `os.DirFS` escaping, which a root-relative [`LocalDir`] cannot do.
//! * [`LocalDir::open`] matches the scheme and the `localhost` authority
//!   ASCII-case-insensitively (RFC 3986 / Go's `url.Parse` lowercase schemes; Go leaves the
//!   host as written). ASCII only — no Unicode case folding, so `ﬁle` is not `file`.
//!   It rejects a non-empty authority other than `localhost`, a query, and a fragment
//!   instead of silently dropping them as `For` does — dropping them could silently point at
//!   a different file. Raw OS paths keep their exact bytes.
//! * [`LocalDir::list`] covers **one directory level** (like `fs.ReadDir`), sorted by raw
//!   name bytes — Go's order. A whole-tree postorder walk is SC-02 work, not this subset.
//! * Names stay `OsString`/`OsStr` end to end; lossy rendering appears only in diagnostics.
//!   Platform limits are asserted in tests, not assumed: APFS rejects non-UTF8 names
//!   (`EILSEQ`) and folds case and normalisation variants; Linux keeps byte distinctions.
//! * Windows specifics (`\\?\`, volumes, ADS, junctions) are neither implemented nor
//!   exercised; on non-Unix [`LocalDir::open`] rejects drive-prefix URIs rather than
//!   pretending they work. Coverage gap, not parity.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `core/storage/storage.go:13` — `LocalSchemaID = "file"`. The only scheme this subset
/// can serve; anything else is rejected rather than emulated.
pub const LOCAL_SCHEMA_ID: &str = "file";

/// Parity bound with Go's `filepath.EvalSymlinks`, which stops after 255 walked links
/// (`path/filepath/symlink.go`: `linksWalked > 255`).
pub const MAX_SYMLINK_HOPS: usize = 255;

/// Hard stop on total resolution steps (components consumed **and** enqueued), so a link
/// target can never expand into an unbounded worklist. Exceeding it is an error.
const MAX_RESOLUTION_STEPS: usize = 100_000;

/// Whether an operation may follow symlinks. Go has no such switch: `os.DirFS` follows
/// every component, and `localFS` exposes the no-follow half as `fs.ReadLinkFS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Never follow: classify and stat the entry itself (`lstat`), refuse a symlink
    /// terminal component and a symlinked ancestor.
    No,
    /// Follow: walk the whole chain at the OS level, classify from the final endpoint.
    Yes,
}

/// Entry type as seen **without** following a symlink (Go `fs.Lstat` entry types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Regular file (`FileInfo.Mode().IsRegular()`).
    File,
    /// Directory (`fs.ModeDir`).
    Dir,
    /// Symbolic link (`fs.ModeSymlink`).
    Symlink,
    /// Anything else (FIFO, socket, device node, …).
    Other,
}

/// "This platform/filesystem reported no creation time." Go's `BirthTime` papers over the
/// same situation with `time.Now()` (`core/storage/local/local.go:120-128`); this keeps it visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BirthTimeUnavailable;

impl fmt::Display for BirthTimeUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("birth time not reported by this platform/filesystem")
    }
}

impl std::error::Error for BirthTimeUnavailable {}

/// "This platform reported no timestamp at all." `Metadata::modified()` can fail where
/// Go's `fs.FileInfo.ModTime()` cannot; the absence is surfaced, not replaced by an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatUnavailable;

impl fmt::Display for StatUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("timestamp not reported by this platform/filesystem")
    }
}

impl std::error::Error for StatUnavailable {}

/// Owned stat snapshot: the `fs.FileInfo` fields a scanner consumes (`size`, `mtime`) plus
/// the birth time Go reads via `times.Get`/`statx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// `fs.FileInfo.Size()`. Go stores `int64`; here the raw `u64` from the OS.
    pub size: u64,
    /// `fs.FileInfo.ModTime()` — the file's own mtime, never a scan timestamp.
    pub modified: Result<SystemTime, StatUnavailable>,
    /// Birth time, with an explicit unavailable state rather than a fabricated value.
    pub created: Result<SystemTime, BirthTimeUnavailable>,
}

/// What a resolved symlink chain ends at. `Symlink` here means "a link left in the chain
/// because the hop bound was reached"; a fully resolved chain ends at `File`, `Dir`,
/// `Other` or `Missing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// Regular file at the end of the chain.
    File,
    /// Directory at the end of the chain.
    Dir,
    /// Unresolved link remaining after [`MAX_SYMLINK_HOPS`].
    Symlink,
    /// Non-file, non-directory endpoint (FIFO, socket, …).
    Other,
    /// Broken link: the chain ends at a path that does not exist.
    Missing,
}

/// A library-relative, slash-separated logical path: what a scanner would store. The OS
/// path derived from it is for I/O only, never for IDs or PIDs. It stays valid and
/// preserved even when it names a symlink whose target is outside the root
/// (`core/storage/interface.go:21-26`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalPath {
    components: Vec<OsString>,
}

impl LogicalPath {
    /// The root (`""` or `"."` in Go's `fs.ValidPath` terms).
    pub fn root() -> LogicalPath {
        LogicalPath {
            components: Vec::new(),
        }
    }

    /// Parse and validate a slash-separated logical path relative to the root.
    ///
    /// Rejects absolute paths, `..`, `.`, empty components, trailing separators and any
    /// backslash (Windows separators are not logical-path syntax; on Unix a literal `\` in
    /// a name is addressed through [`LocalDir::logical`]).
    pub fn parse(path: &str) -> Result<LogicalPath, StorageError> {
        if path.is_empty() || path == "." {
            return Ok(LogicalPath::root());
        }
        let mut components = Vec::new();
        for raw in path.split('/') {
            if raw == ".." || raw == "." || raw.is_empty() || raw.contains('\\') {
                return Err(StorageError::InvalidLogicalPath {
                    path: path.to_string(),
                });
            }
            components.push(OsString::from(raw));
        }
        Ok(LogicalPath { components })
    }

    /// Number of components (0 for the root).
    pub fn depth(&self) -> usize {
        self.components.len()
    }

    /// Components, in order.
    pub fn components(&self) -> &[OsString] {
        &self.components
    }

    /// True for the root.
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// Join a single child name onto this path.
    pub fn join(&self, name: &OsStr) -> Result<LogicalPath, StorageError> {
        if name.is_empty() || name == OsStr::new("..") || name == OsStr::new(".") {
            return Err(StorageError::InvalidLogicalPath {
                path: name.to_string_lossy().into_owned(),
            });
        }
        let mut components = self.components.clone();
        components.push(name.to_os_string());
        Ok(LogicalPath { components })
    }

    /// This path without its last component (root for a single-component path).
    pub fn parent(&self) -> LogicalPath {
        let mut components = self.components.clone();
        components.pop();
        LogicalPath { components }
    }

    /// Last component, or `None` for the root.
    pub fn file_name(&self) -> Option<&OsStr> {
        self.components.last().map(OsString::as_os_str)
    }
}

/// Slash-separated display form of a logical path: diagnostics only — it is lossy
/// (U+FFFD for any non-UTF-8 bytes, so `raw-\xFF` displays as `raw-<U+FFFD>`) while the
/// exact bytes live in [`LogicalPath::components`] and the derived `Debug` form
/// (`S9`).
impl fmt::Display for LogicalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.components.is_empty() {
            return f.write_str(".");
        }
        // Lossy is fine here: this string is diagnostics only, the components stay exact.
        let rendered = self
            .components
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        f.write_str(&rendered)
    }
}

/// One directory entry: logical path, lstat-based kind, lstat-based stat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// Path relative to the root (`parent/<name>`), not just the name.
    pub path: LogicalPath,
    /// Kind without following the entry's own symlink.
    pub kind: EntryKind,
    /// Stat without following the entry's own symlink: for a symlink this is the link's own
    /// length, exactly like Go's `fs.Lstat`.
    pub stat: FileStat,
}
/// Entry facts for one directory level, with **both** stat views kept distinct.
///
/// This is the additive SC-02 listing seam. Go reads a folder entry's size and mtime from
/// `fs.DirEntry.Info()` — the entry's own `lstat` — while tag enrichment later stats the
/// **endpoint** with `fs.Stat` (`scanner/walk_dir_tree.go:157-177`, `scanner/folder_entry.go:133-148`,
/// `core/storage/local/local.go:90-109`). [`LocalDir::list`] under [`Follow::Yes`] replaces an entry's kind
/// and stat with the endpoint's (`storage.rs:550-561`), which is right for classifying by
/// target extension and wrong for a fingerprint that must reproduce Go's `Info()` bytes.
/// [`LocalDir::list_annotated`] reports both instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFacts {
    /// Path relative to the root (`parent/<name>`).
    pub path: LogicalPath,
    /// Kind without following the entry's own symlink (`fs.DirEntry.Type()`).
    pub kind: EntryKind,
    /// The entry's own `lstat` facts (`fs.DirEntry.Info()`). For a symlink this is the
    /// link's own length and mtime — exactly the values Go hashes.
    pub stat: FileStat,
    /// Endpoint facts, present only when `kind` is [`EntryKind::Symlink`] **and** the
    /// endpoint could be stat'd. Reserved for mapping/tag enrichment, never for hashing.
    pub endpoint: Option<EntryEndpoint>,
}

/// Resolved endpoint of a symlink entry (`fs.Stat` of the entry's path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryEndpoint {
    /// Classification of the actual endpoint, never of the link name.
    pub kind: EndpointKind,
    /// Endpoint size and mtime.
    pub stat: FileStat,
}

/// One directory level: the directory's own facts plus its entries.
///
/// `dir_stat` mirrors `fs.Stat(dirPath)` in `loadDir` (`scanner/walk_dir_tree.go:111-119`) under
/// [`Follow::Yes`]; under [`Follow::No`] the directory is `lstat`'d instead, because that
/// policy reports the entry rather than its endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    /// The listed directory's own facts.
    pub dir_stat: FileStat,
    /// Entries sorted by raw name bytes (Go's `fs.ReadDir` order).
    pub entries: Vec<EntryFacts>,
}

/// Endpoint of a resolved chain, with the logical name preserved beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    /// The logical path that was resolved (unchanged even when the target is elsewhere).
    pub logical: LogicalPath,
    /// Resolved OS path (absolute whenever the root was).
    pub target: PathBuf,
    /// Classification of the **actual final endpoint**, never of the link name.
    pub kind: EndpointKind,
    /// Links walked, including symlinked components of the root prefix. A lower bound for
    /// absolute targets, which may pass through `/var` → `private/var` and friends.
    pub hops: usize,
}

/// Outcome of [`LocalDir::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// The chain terminated at a real endpoint, or at a missing one.
    Endpoint(ResolvedEndpoint),
    /// The chain looped: `hops` links walked with no endpoint reached.
    LinkLoop {
        /// Logical path that looped.
        logical: LogicalPath,
        /// Links walked before giving up (above [`MAX_SYMLINK_HOPS`]).
        hops: usize,
    },
}

/// Owned metadata snapshot of one logical path (`fs.FileInfo` equivalent plus the lstat kind
/// a scanner needs for classification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Logical path as supplied — never rewritten to the resolved target.
    pub logical: LogicalPath,
    /// Kind under the caller's follow policy.
    pub kind: EntryKind,
    /// Stat under the caller's follow policy.
    pub stat: FileStat,
}

impl Snapshot {
    /// Whether the snapshot names a regular file (Go `FileInfo.Mode().IsRegular()`).
    pub fn is_regular_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    /// Whether the snapshot names a symlink (Go `FileInfo.Mode()&fs.ModeSymlink != 0`).
    pub fn is_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink
    }
}

/// A local storage root: one **configured** (trusted) directory addressed by logical paths.
/// No containment, no authorization, no race-proof no-follow — see the module docs.
/// Errors carry the logical path, mirroring Go's `*fs.PathError{Op, Path, Err}`
/// (`core/storage/local/local.go:56,74`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDir {
    root: PathBuf,
}

impl LocalDir {
    /// Open a storage URI.
    ///
    /// Parity with `storage.For` (`core/storage/storage.go:53-79`), which checks for `://` **before**
    /// `url.Parse`. Classification here:
    /// * text containing `://` → a URL; a scheme with no implementation is an error.
    /// * text starting with a multi-character scheme followed by `:/` (e.g. `file:/`,
    ///   `http:/`) → also a URL, matching `url.Parse` (`file:/tmp/x.mp3` → path
    ///   `/tmp/x.mp3`, no authority). Go's `For` would treat this shape as a literal local
    ///   path; treating it as a URL is safer because it never turns a remote-looking URI
    ///   into an existing-looking directory.
    /// * anything else → a bare OS path taken **verbatim**. That includes Windows drive
    ///   paths (`C:\music`, `C:/music`), which `url.Parse` would misread as scheme `c`
    ///   (verified against net/url) but which Go's `For` keeps as a local path.
    ///
    /// For a `file` URL the path is percent-decoded exactly once, matching `url.Parse`'s
    /// `u.Path` that Go hands to `newLocalStorage`. Scheme and the `localhost` authority are
    /// matched **ASCII-case-insensitively** (RFC 3986: schemes are case-insensitive; Go's
    /// `url.Parse` lowercases them — `FILE:///tmp/x` → scheme `file`, and it leaves the host
    /// as written). Only ASCII case is folded: no Unicode lowercasing, so a lookalike name
    /// such as `ﬁle` is not accepted as `file`. Nothing else is case-folded — the decoded
    /// path keeps its exact bytes. A non-empty authority other than `localhost`, a query, and
    /// a fragment are rejected rather than silently dropped, and malformed escapes are errors
    /// — never a literal path.
    pub fn open(uri: &str) -> Result<LocalDir, StorageError> {
        let (scheme, rest) = match uri.split_once("://") {
            Some((scheme, rest)) => (scheme, rest),
            None => match scheme_prefix(uri) {
                Some((scheme, rest)) => (scheme, rest),
                None => return LocalDir::from_path(uri),
            },
        };
        if !is_local_scheme(scheme) {
            return Err(StorageError::UnsupportedScheme {
                scheme: scheme.to_string(),
            });
        }

        // Authority: everything before the first '/'. `file:/tmp/x.mp3` has none, matching
        // Go's url.Parse. A non-empty authority other than the conventional `localhost` is
        // a misbuilt URI here, not something to reinterpret as a path.
        let (authority, rest) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if !authority.is_empty() && !is_localhost_authority(authority) {
            return Err(StorageError::UnsupportedAuthority {
                uri: uri.to_string(),
                authority: authority.to_string(),
            });
        }

        // Fragment, then query: neither is part of u.Path in Go, so neither is a path here.
        let (rest, fragment) = match rest.find('#') {
            Some(index) => (&rest[..index], Some(&rest[index + 1..])),
            None => (rest, None),
        };
        let (path, query) = match rest.find('?') {
            Some(index) => (&rest[..index], Some(&rest[index + 1..])),
            None => (rest, None),
        };
        if fragment.is_some() {
            return Err(StorageError::UnsupportedUrlComponent {
                uri: uri.to_string(),
                component: "fragment",
            });
        }
        if query.is_some() {
            return Err(StorageError::UnsupportedUrlComponent {
                uri: uri.to_string(),
                component: "query",
            });
        }
        if path.is_empty() {
            return Err(StorageError::InvalidStorageUri {
                uri: uri.to_string(),
                reason: "empty path",
            });
        }

        let decoded = decode_path(path, uri)?;
        if is_windows_volume(&decoded) && !cfg!(unix) {
            return Err(StorageError::InvalidStorageUri {
                uri: uri.to_string(),
                reason: "Windows volume paths are not supported on this untested platform",
            });
        }
        Ok(LocalDir { root: decoded })
    }

    /// Build a local root from a bare OS path: bytes taken **verbatim**, no percent
    /// decoding and no UTF-8 conversion, so a directory named `50% off` or `a#b` opens as
    /// itself. This is the constructor for anything derived from the filesystem.
    pub fn from_path(path: impl AsRef<OsStr>) -> Result<LocalDir, StorageError> {
        let raw = path.as_ref();
        if raw.is_empty() {
            return Err(StorageError::InvalidStorageUri {
                uri: String::new(),
                reason: "empty path",
            });
        }
        Ok(LocalDir {
            root: PathBuf::from(raw),
        })
    }

    /// The configured root exactly as given (not canonicalised; a symlink root is trusted
    /// configuration and is followed).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Exact logical path from an OS name or path, without any UTF-8 conversion.
    ///
    /// Only a path **inside** the root can be addressed: absolute paths and `..` are
    /// rejected. This replaces Go's `fs.ValidPath` check (`core/storage/local/local.go:73`).
    pub fn logical(&self, path: &OsStr) -> Result<LogicalPath, StorageError> {
        let raw = path.to_string_lossy().into_owned();
        if path.is_empty() {
            return Err(StorageError::InvalidLogicalPath { path: raw });
        }
        let as_path = Path::new(path);
        let relative = if path == OsStr::new(".") {
            Path::new("")
        } else {
            as_path.strip_prefix(&self.root).unwrap_or(as_path)
        };
        let mut components = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::CurDir => continue,
                std::path::Component::Normal(name) => components.push(name.to_os_string()),
                // RootDir (absolute/drive prefix) and ParentDir (`..`) both escape.
                _ => return Err(StorageError::InvalidLogicalPath { path: raw }),
            }
        }
        Ok(LogicalPath { components })
    }

    /// List one directory level, sorted by raw name bytes (the order Go's `fs.ReadDir`
    /// gives). Entries are classified with `lstat` under [`Follow::No`] — a symlink reports
    /// `EntryKind::Symlink` with its own length — and by their endpoint under [`Follow::Yes`].
    ///
    /// Under [`Follow::No`] a symlink **requested** directory is refused
    /// ([`StorageError::EntryIsSymlink`]) rather than traversed, and a symlinked ancestor is
    /// refused. The configured root itself is exempt (trusted configuration, followed like
    /// Go). The requested directory component of a *child* path is still resolved by the OS,
    /// so this is best-effort path-name checking, not a race-proof guarantee.
    pub fn list(&self, dir: &LogicalPath, follow: Follow) -> Result<Vec<ListEntry>, StorageError> {
        let os_dir = self.os_path(dir);
        self.check_ancestors(dir, follow)?;
        if follow == Follow::No && !dir.is_root() {
            // The terminal component is checked before read_dir, which would otherwise
            // follow a directory symlink and enumerate the alias target.
            match std::fs::symlink_metadata(&os_dir) {
                Ok(metadata) => {
                    let kind = kind_of_metadata(&metadata);
                    if kind == EntryKind::Symlink {
                        return Err(StorageError::EntryIsSymlink {
                            path: dir.clone(),
                            target: read_link_or_empty(&os_dir),
                        });
                    }
                    if kind != EntryKind::Dir {
                        return Err(StorageError::NotADirectory {
                            path: dir.clone(),
                            kind,
                            source: enotdir_error(),
                        });
                    }
                }
                Err(source) => return Err(self.not_found(dir, source)),
            }
        }

        let read = match std::fs::read_dir(&os_dir) {
            Ok(read) => read,
            Err(source) => {
                // Go surfaces the raw os.ReadDir error (ENOTDIR for a file, ENOENT for a
                // missing path or a broken link); classify the common cases.
                if source.kind() == io::ErrorKind::NotFound {
                    return Err(self.not_found(dir, source));
                }
                return match std::fs::symlink_metadata(&os_dir) {
                    Ok(metadata) if kind_of_metadata(&metadata) != EntryKind::Dir => {
                        Err(StorageError::NotADirectory {
                            path: dir.clone(),
                            kind: kind_of_metadata(&metadata),
                            source,
                        })
                    }
                    // A real directory whose listing still failed (permissions, EIO), or a
                    // path that cannot be stat'd at all.
                    _ => Err(self.map_io(dir, source)),
                };
            }
        };

        let mut entries = Vec::new();
        for entry in read {
            let entry = entry.map_err(|source| StorageError::Io {
                path: dir.clone(),
                source,
            })?;
            // lstat the entry's own OS path instead of trusting `DirEntry::file_type()`:
            // on Windows that follows the link, so it cannot distinguish a link from target.
            let os_path = entry.path();
            let path = dir.join(entry.file_name().as_os_str())?;
            let (kind, stat) = match follow {
                Follow::No => self.stat_pair(&os_path, Follow::No, &path)?,
                Follow::Yes => match self.stat_pair(&os_path, Follow::Yes, &path) {
                    Ok(pair) => pair,
                    // A link whose endpoint vanished between readdir and stat keeps the
                    // link itself (Go would surface the ReadDir/Stat error instead).
                    Err(StorageError::NotFound { .. }) => {
                        self.stat_pair(&os_path, Follow::No, &path)?
                    }
                    Err(other) => return Err(other),
                },
            };
            entries.push(ListEntry { path, kind, stat });
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
    /// List one directory level while keeping the entry's own `lstat` facts **and** its
    /// endpoint facts distinct — the seam SC-02's walker needs (see [`EntryFacts`]).
    ///
    /// Additive: [`LocalDir::list`] is unchanged. Differences here:
    /// * every entry keeps its own kind and `lstat` stat, and a symlink additionally
    ///   carries the endpoint's kind and stat under [`Follow::Yes`];
    /// * under [`Follow::Yes`] a symlinked ancestor or a symlinked requested directory is
    ///   followed by the OS (like Go's `os.DirFS`) while entries keep their link facts;
    /// * under [`Follow::No`] the refusals still apply — a symlinked ancestor or a
    ///   symlinked requested directory is rejected, never traversed, and no endpoint is
    ///   reported at all;
    /// * the directory's own facts come back beside the entries.
    ///
    /// A link whose endpoint vanished keeps its link facts and reports no endpoint, which
    /// matches Go: `DirEntry.Info()` succeeds on a broken link.
    pub fn list_annotated(
        &self,
        dir: &LogicalPath,
        follow: Follow,
    ) -> Result<DirectoryListing, StorageError> {
        let os_dir = self.os_path(dir);
        self.check_ancestors(dir, follow)?;

        let dir_stat = if follow == Follow::No && !dir.is_root() {
            // Same refusal as `list`: enumerating through a symlinked directory would
            // traverse an alias the policy forbids.
            match std::fs::symlink_metadata(&os_dir) {
                Ok(metadata) => {
                    let kind = kind_of_metadata(&metadata);
                    if kind == EntryKind::Symlink {
                        return Err(StorageError::EntryIsSymlink {
                            path: dir.clone(),
                            target: read_link_or_empty(&os_dir),
                        });
                    }
                    if kind != EntryKind::Dir {
                        return Err(StorageError::NotADirectory {
                            path: dir.clone(),
                            kind,
                            source: enotdir_error(),
                        });
                    }
                    stat_of_metadata(&metadata)
                }
                Err(source) => return Err(self.not_found(dir, source)),
            }
        } else {
            match std::fs::metadata(&os_dir) {
                Ok(metadata) => stat_of_metadata(&metadata),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return Err(self.not_found(dir, source));
                }
                Err(source) => {
                    return match std::fs::symlink_metadata(&os_dir) {
                        Ok(metadata) if kind_of_metadata(&metadata) != EntryKind::Dir => {
                            Err(StorageError::NotADirectory {
                                path: dir.clone(),
                                kind: kind_of_metadata(&metadata),
                                source,
                            })
                        }
                        // A real directory whose stat still failed, or a path that
                        // cannot be stat'd at all.
                        _ => Err(self.map_io(dir, source)),
                    };
                }
            }
        };

        let read = match std::fs::read_dir(&os_dir) {
            Ok(read) => read,
            Err(source) => {
                if source.kind() == io::ErrorKind::NotFound {
                    return Err(self.not_found(dir, source));
                }
                return match std::fs::symlink_metadata(&os_dir) {
                    Ok(metadata) if kind_of_metadata(&metadata) != EntryKind::Dir => {
                        Err(StorageError::NotADirectory {
                            path: dir.clone(),
                            kind: kind_of_metadata(&metadata),
                            source,
                        })
                    }
                    _ => Err(self.map_io(dir, source)),
                };
            }
        };

        let mut entries = Vec::new();
        for entry in read {
            let entry = entry.map_err(|source| StorageError::Io {
                path: dir.clone(),
                source,
            })?;
            let os_path = entry.path();
            let path = dir.join(entry.file_name().as_os_str())?;
            // lstat first: this is Go's `DirEntry.Info()`, the facts the fingerprint uses.
            let metadata =
                std::fs::symlink_metadata(&os_path).map_err(|source| self.map_io(&path, source))?;
            let kind = kind_of_metadata(&metadata);
            let endpoint = if kind != EntryKind::Symlink || follow == Follow::No {
                None
            } else {
                match std::fs::metadata(&os_path) {
                    Ok(endpoint_stat) => Some(EntryEndpoint {
                        kind: endpoint_kind_of_metadata(&endpoint_stat),
                        stat: stat_of_metadata(&endpoint_stat),
                    }),
                    Err(_) => None,
                }
            };
            entries.push(EntryFacts {
                path,
                kind,
                stat: stat_of_metadata(&metadata),
                endpoint,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(DirectoryListing { dir_stat, entries })
    }

    /// Stat one entry. Always no-follow under [`Follow::No`]: a symlink yields
    /// `EntryKind::Symlink` and works even when its target is missing (Go `fs.Lstat`).
    /// [`Follow::Yes`] classifies the endpoint; a broken link is `NotFound` (Go `fs.Stat`).
    /// This operation *reports* a link instead of refusing it — see [`LocalDir::list`] and
    /// [`LocalDir::read`] for the operations that refuse to traverse one.
    pub fn snapshot(&self, path: &LogicalPath, follow: Follow) -> Result<Snapshot, StorageError> {
        let os_path = self.os_path(path);
        self.check_ancestors(path, follow)?;
        let (kind, stat) = self.stat_pair(&os_path, follow, path)?;
        Ok(Snapshot {
            logical: path.clone(),
            kind,
            stat,
        })
    }

    /// Read a file's bytes.
    ///
    /// [`Follow::Yes`] follows the entry's own link and any chain, like Go's `fs.ReadFile`.
    /// [`Follow::No`] refuses to read through a link at all: a symlink entry is
    /// [`StorageError::EntryIsSymlink`], and a symlinked ancestor is refused. Go has no
    /// such mode (`os.DirFS` always follows).
    pub fn read(&self, path: &LogicalPath, follow: Follow) -> Result<Vec<u8>, StorageError> {
        let os_path = self.os_path(path);
        self.check_ancestors(path, follow)?;
        if follow == Follow::No {
            let metadata =
                std::fs::symlink_metadata(&os_path).map_err(|source| self.map_io(path, source))?;
            if kind_of_metadata(&metadata) == EntryKind::Symlink {
                return Err(StorageError::EntryIsSymlink {
                    path: path.clone(),
                    target: read_link_or_empty(&os_path),
                });
            }
        }
        std::fs::read(&os_path).map_err(|source| self.map_io(path, source))
    }

    /// Raw link target, without interpreting it (`fs.ReadLink`, `core/storage/local/local.go:82-84`). A
    /// non-link is an OS error (Linux `EINVAL`, macOS `ENOTDIR`), not "not a symlink".
    pub fn read_link(&self, path: &LogicalPath) -> Result<PathBuf, StorageError> {
        let os_path = self.os_path(path);
        std::fs::read_link(&os_path).map_err(|source| self.map_io(path, source))
    }

    /// Resolve the whole symlink chain at the OS level, like `localFS.ResolveSymlink`
    /// (`core/storage/local/local.go:72-77`) via `filepath.EvalSymlinks`.
    ///
    /// [`Follow::Yes`] mirrors Go: relative targets resolve against the link's directory,
    /// `.` components are dropped, a `..` produced **by a link target** is applied to the
    /// resolved path (a caller-supplied logical path can never contain `..`), a
    /// non-directory component mid-path is an error, and the walk stops after
    /// [`MAX_SYMLINK_HOPS`] links. The logical name is always preserved. No authorization:
    /// the endpoint may lie outside the root and the caller decides.
    ///
    /// [`Follow::No`] traverses nothing: the entry itself is classified and a symlink is
    /// reported as `EndpointKind::Symlink` with `target` set to the link's own path
    /// ([`LocalDir::read_link`] gives the raw target text).
    pub fn resolve(
        &self,
        path: &LogicalPath,
        follow: Follow,
    ) -> Result<ResolvedTarget, StorageError> {
        if follow == Follow::No {
            let snapshot = self.snapshot(path, Follow::No)?;
            let kind = match snapshot.kind {
                EntryKind::File => EndpointKind::File,
                EntryKind::Dir => EndpointKind::Dir,
                EntryKind::Symlink => EndpointKind::Symlink,
                EntryKind::Other => EndpointKind::Other,
            };
            return Ok(ResolvedTarget::Endpoint(ResolvedEndpoint {
                logical: path.clone(),
                target: self.os_path(path),
                kind,
                hops: 0,
            }));
        }
        self.resolve_chain(path)
    }

    /// Whether the entry exists without following it (`fs.Lstat`-style).
    pub fn exists(&self, path: &LogicalPath) -> bool {
        self.snapshot(path, Follow::No).is_ok()
    }

    /// OS path for a logical path (`filepath.Join(root, filepath.FromSlash(name))`,
    /// `core/storage/local/local.go:76`). Public so a decoder can be handed the real file; IDs and PIDs must
    /// keep using the **logical** path.
    pub fn os_path(&self, path: &LogicalPath) -> PathBuf {
        let mut out = self.root.clone();
        for component in &path.components {
            out.push(component);
        }
        out
    }

    fn not_found(&self, path: &LogicalPath, source: io::Error) -> StorageError {
        // Go's fs.Stat reports the *requested* path in its PathError, so the caller sees
        // the whole logical path even when an ancestor is the missing component.
        StorageError::NotFound {
            path: path.clone(),
            source,
        }
    }

    fn map_io(&self, path: &LogicalPath, source: io::Error) -> StorageError {
        match source.kind() {
            io::ErrorKind::NotFound => self.not_found(path, source),
            io::ErrorKind::PermissionDenied => StorageError::PermissionDenied {
                path: path.clone(),
                source,
            },
            // A NUL byte in a name is an invalid path rather than an I/O failure. Go's
            // os.* returns EINVAL for the same input.
            io::ErrorKind::InvalidInput => StorageError::InvalidLogicalPath {
                path: path.to_string(),
            },
            _ => StorageError::Io {
                path: path.clone(),
                source,
            },
        }
    }

    fn stat_pair(
        &self,
        os_path: &Path,
        follow: Follow,
        logical: &LogicalPath,
    ) -> Result<(EntryKind, FileStat), StorageError> {
        let metadata = match match follow {
            Follow::No => std::fs::symlink_metadata(os_path),
            Follow::Yes => std::fs::metadata(os_path),
        } {
            Ok(metadata) => metadata,
            Err(source) => return Err(self.map_io(logical, source)),
        };
        Ok((
            kind_of_metadata(&metadata),
            file_stat(
                metadata.len(),
                metadata.modified().map_err(|_| StatUnavailable),
                metadata.created().map_err(|_| BirthTimeUnavailable),
            ),
        ))
    }

    /// Refuse to walk through a symlinked ancestor when following is disabled. Guards
    /// components **of the logical path** only: the configured root is trusted and a
    /// symlink above it is out of scope (see module docs).
    fn check_ancestors(&self, path: &LogicalPath, follow: Follow) -> Result<(), StorageError> {
        // With following enabled the chain walk / stat call decides what each component is.
        if follow == Follow::Yes || path.is_root() {
            return Ok(());
        }
        let mut current = LogicalPath::root();
        for ancestor in path.parent().components.clone() {
            current = current.join(&ancestor)?;
            let os_path = self.os_path(&current);
            match std::fs::symlink_metadata(&os_path) {
                Ok(metadata) => {
                    let kind = kind_of_metadata(&metadata);
                    if kind == EntryKind::Symlink {
                        return Err(StorageError::AncestorIsSymlink {
                            path: current,
                            target: read_link_or_empty(&os_path),
                        });
                    }
                    if kind != EntryKind::Dir {
                        // filepath/symlink.go returns syscall.ENOTDIR for a non-directory
                        // component mid-path.
                        return Err(StorageError::AncestorNotDirectory {
                            path: current,
                            kind,
                        });
                    }
                }
                Err(source) => return Err(self.not_found(path, source)),
            }
        }
        Ok(())
    }

    /// Mirrors Go's `filepath.walkSymlinks`: pending components in a worklist plus the
    /// resolved prefix, with a link's target components inserted at the **front** (Go
    /// rebuilds the path as `link + remaining`), so relative targets resolve against the
    /// link's directory and absolute ones restart the prefix.
    fn resolve_chain(&self, path: &LogicalPath) -> Result<ResolvedTarget, StorageError> {
        let mut dest: Vec<OsString> = self
            .root
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        let mut queue: std::collections::VecDeque<OsString> =
            std::collections::VecDeque::from(path.components.clone());
        let mut hops: usize = 0;
        let mut steps: usize = 0;

        while let Some(component) = queue.pop_front() {
            steps += 1;
            if steps > MAX_RESOLUTION_STEPS {
                return Err(self.step_limit(path));
            }
            if component == OsStr::new(".") {
                continue;
            }
            if component == OsStr::new("..") {
                // Only reachable through a link target. Applied to the resolved prefix.
                if !pop_component(&mut dest) {
                    return Err(StorageError::Io {
                        path: path.clone(),
                        source: io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "resolve: path escaped its root",
                        ),
                    });
                }
                continue;
            }

            // Separator marker, so joining never merges two components.
            if !dest.is_empty() {
                dest.push(OsString::new());
            }
            dest.push(component.clone());

            let candidate = join_dest(&dest);
            match std::fs::symlink_metadata(&candidate) {
                Ok(metadata) => {
                    let kind = kind_of_metadata(&metadata);
                    if kind != EntryKind::Symlink {
                        if kind != EntryKind::Dir && !queue.is_empty() {
                            return Err(StorageError::Io {
                                path: path.clone(),
                                source: enotdir_error(),
                            });
                        }
                        continue;
                    }
                }
                Err(source) => {
                    // A missing component terminates the walk: report the endpoint as
                    // missing (remaining components appended lexically) rather than
                    // pretending a target exists.
                    if source.kind() == io::ErrorKind::NotFound {
                        let mut missing = dest.clone();
                        for remaining in queue.iter() {
                            missing.push(OsString::new());
                            missing.push(remaining.clone());
                        }
                        return Ok(ResolvedTarget::Endpoint(ResolvedEndpoint {
                            logical: path.clone(),
                            target: join_dest(&missing),
                            kind: EndpointKind::Missing,
                            hops,
                        }));
                    }
                    return Err(self.map_io(path, source));
                }
            }

            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Ok(ResolvedTarget::LinkLoop {
                    logical: path.clone(),
                    hops,
                });
            }

            let target =
                std::fs::read_link(&candidate).map_err(|source| self.map_io(path, source))?;
            // Drop the link's own component: a relative target resolves against the link's
            // directory and an absolute one restarts the prefix, either way the link
            // component is no longer part of the resolved path.
            pop_component(&mut dest);
            let mut inserted = 0;
            for component in target.components() {
                let pending = match component {
                    std::path::Component::CurDir => OsString::from("."),
                    std::path::Component::Normal(name) => name.to_os_string(),
                    std::path::Component::ParentDir => OsString::from(".."),
                    std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                        dest = vec![component.as_os_str().to_os_string()];
                        continue;
                    }
                };
                queue.insert(inserted, pending);
                inserted += 1;
                steps += 1;
                if steps > MAX_RESOLUTION_STEPS {
                    return Err(self.step_limit(path));
                }
            }
        }

        let final_path = join_dest(&dest);
        let kind = match std::fs::symlink_metadata(&final_path) {
            Ok(metadata) => match kind_of_metadata(&metadata) {
                EntryKind::File => EndpointKind::File,
                EntryKind::Dir => EndpointKind::Dir,
                EntryKind::Symlink => EndpointKind::Symlink,
                EntryKind::Other => EndpointKind::Other,
            },
            Err(source) if source.kind() == io::ErrorKind::NotFound => EndpointKind::Missing,
            Err(source) => return Err(self.map_io(path, source)),
        };
        Ok(ResolvedTarget::Endpoint(ResolvedEndpoint {
            logical: path.clone(),
            target: final_path,
            kind,
            hops,
        }))
    }

    fn step_limit(&self, path: &LogicalPath) -> StorageError {
        StorageError::Io {
            path: path.clone(),
            source: io::Error::other(format!(
                "resolve: exceeded {} resolution steps",
                MAX_RESOLUTION_STEPS
            )),
        }
    }
}

/// Scheme match per RFC 3986: ASCII case-insensitive, which is also what Go's `url.Parse`
/// does (it lowercases the scheme). ASCII only on purpose — no Unicode case folding, so a
/// lookalike such as `ﬁle` (U+FB01 ligature) is not accepted as `file`.
fn is_local_scheme(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case(LOCAL_SCHEMA_ID)
}

/// The narrow `localhost` allowlist, likewise ASCII-case-insensitive (host names are
/// case-insensitive in practice; Go's `url.Parse` leaves the host as written). This
/// broadens *case* only: userinfo, ports and any other host stay rejected.
fn is_localhost_authority(authority: &str) -> bool {
    authority.eq_ignore_ascii_case("localhost")
}

/// `scheme:` followed by `/`, for a scheme of at least two characters — so URL shapes
/// without an authority (`file:/x`, `http:/x`) are recognised as URLs while a Windows drive
/// prefix (`C:/music`, which `url.Parse` reads as scheme `c`) stays a filesystem path, as
/// Go's `storage.For` does by checking for `://` before parsing.
fn scheme_prefix(uri: &str) -> Option<(&str, &str)> {
    let (scheme, after_colon) = uri.split_once(':')?;
    // The path keeps its leading slash, so `file:/tmp/x` has an empty authority.
    if scheme.len() < 2 || !after_colon.starts_with('/') {
        return None;
    }
    let is_scheme = scheme
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'));
    is_scheme.then_some((scheme, after_colon))
}

/// Percent-decode a URL path **once**, matching what `url.Parse` puts in `u.Path`.
///
/// `%XX` becomes the byte, so multi-byte UTF-8 sequences reassemble; `%2F` becomes a real
/// `/` separator (Go does the same), and `+` stays `+` (no `+`-space decoding in paths).
/// Invalid escapes and embedded NUL bytes are typed errors, never a literal path. Decoded
/// bytes are used verbatim on Unix; on other platforms valid UTF-8 is required.
fn decode_path(encoded: &str, uri: &str) -> Result<PathBuf, StorageError> {
    let invalid = |reason: &'static str| StorageError::InvalidStorageUri {
        uri: uri.to_string(),
        reason,
    };
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(invalid("truncated percent escape"));
            }
            let hi = hex_value(bytes[index + 1]).ok_or(invalid("invalid URL escape"))?;
            let lo = hex_value(bytes[index + 2]).ok_or(invalid("invalid URL escape"))?;
            out.push(hi * 16 + lo);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    if out.contains(&0) {
        return Err(invalid("path contains a NUL byte"));
    }
    path_from_bytes(&out).ok_or(invalid("decoded path is not valid text on this platform"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(bytes).ok().map(PathBuf::from)
}

/// `filepath.VolumeName`-shaped prefix (`C:\`, `C:/` after Go's leading-slash form).
fn is_windows_volume(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let trimmed = text.strip_prefix('/').unwrap_or(&text);
    trimmed.len() >= 2
        && trimmed.as_bytes()[1] == b':'
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
}

/// `FileStat` from real metadata, with each timestamp's availability preserved.
fn stat_of_metadata(metadata: &std::fs::Metadata) -> FileStat {
    file_stat(
        metadata.len(),
        metadata.modified().map_err(|_| StatUnavailable),
        metadata.created().map_err(|_| BirthTimeUnavailable),
    )
}

/// Endpoint classification of a followed stat (`fs.Stat` → `IsDir`/`IsRegular`). A
/// missing target produces no metadata, so this never yields [`EndpointKind::Missing`].
fn endpoint_kind_of_metadata(metadata: &std::fs::Metadata) -> EndpointKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        EndpointKind::Dir
    } else if file_type.is_file() {
        EndpointKind::File
    } else if file_type.is_symlink() {
        EndpointKind::Symlink
    } else {
        EndpointKind::Other
    }
}

fn kind_of_metadata(metadata: &std::fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Assemble a snapshot from explicit availability. **No fallback timestamp**: Go's
/// `localFileInfo.BirthTime` substitutes `time.Now()` here (`core/storage/local/local.go:120-128`), which is
/// deliberately absent. Kept private; `stat_tests` covers the unavailable states directly,
/// because `Metadata::created()` never fails on macOS/APFS.
fn file_stat(
    size: u64,
    modified: Result<SystemTime, StatUnavailable>,
    created: Result<SystemTime, BirthTimeUnavailable>,
) -> FileStat {
    FileStat {
        size,
        modified,
        created,
    }
}

fn read_link_or_empty(path: &Path) -> PathBuf {
    std::fs::read_link(path).unwrap_or_else(|_| PathBuf::new())
}

/// `syscall.ENOTDIR` for the mid-path non-directory case Go reports
/// (`filepath/symlink.go`). No libc dependency: the numeric errno on Unix, the matching
/// `ErrorKind` elsewhere.
#[cfg(unix)]
fn enotdir_error() -> io::Error {
    io::Error::from_raw_os_error(20)
}

#[cfg(not(unix))]
fn enotdir_error() -> io::Error {
    io::Error::from(io::ErrorKind::NotADirectory)
}

fn join_dest(dest: &[OsString]) -> PathBuf {
    let mut out = PathBuf::new();
    for component in dest {
        out.push(component);
    }
    out
}

/// Drop the last real component (and its separator marker) from a resolved prefix.
/// `false` means the path tried to escape past its own root.
fn pop_component(dest: &mut Vec<OsString>) -> bool {
    if dest.pop().is_none() {
        return false;
    }
    if dest.len() > 1 && dest.last().is_some_and(|last| last.is_empty()) {
        dest.pop();
    }
    true
}

/// Errors for the local subset, carrying the logical path like Go's `*fs.PathError`.
#[derive(Debug)]
pub enum StorageError {
    /// Scheme with no implementation (`core/storage/storage.go:76-78`). Never silently treated as local.
    UnsupportedScheme {
        /// The scheme that was rejected.
        scheme: String,
    },
    /// `file://` URI with a non-empty authority other than `localhost`. Never reinterpreted
    /// as a path.
    UnsupportedAuthority {
        /// The URI as given.
        uri: String,
        /// The authority that was rejected.
        authority: String,
    },
    /// `file://` URI carrying a query or fragment, neither of which is part of Go's
    /// `u.Path`. Rejected rather than silently dropped.
    UnsupportedUrlComponent {
        /// The URI as given.
        uri: String,
        /// Which part made it unusable as a path.
        component: &'static str,
    },
    /// `file://` URI that cannot name a path (malformed escape, NUL, empty path, unsupported
    /// platform form).
    InvalidStorageUri {
        /// The URI as given.
        uri: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// Logical path that escapes or is malformed (`fs.ErrInvalid`, `core/storage/local/local.go:74`).
    InvalidLogicalPath {
        /// The rejected path as written.
        path: String,
    },
    /// Follow disabled, but an ancestor component is a symlink (nothing was traversed).
    AncestorIsSymlink {
        /// The symlinked ancestor.
        path: LogicalPath,
        /// Its raw link target.
        target: PathBuf,
    },
    /// An ancestor component exists but is not a directory (Go `syscall.ENOTDIR`).
    AncestorNotDirectory {
        /// The offending ancestor.
        path: LogicalPath,
        /// What it actually is.
        kind: EntryKind,
    },
    /// Follow disabled and the entry itself is a symlink, so listing or reading it would
    /// traverse. Refused rather than followed.
    EntryIsSymlink {
        /// The link that was not traversed.
        path: LogicalPath,
        /// Its raw link target.
        target: PathBuf,
    },
    /// Path does not exist (`fs.ErrNotExist`).
    NotFound {
        /// The missing path.
        path: LogicalPath,
        /// OS error.
        source: io::Error,
    },
    /// Permission denied (`fs.ErrPermission`).
    PermissionDenied {
        /// The unreadable path.
        path: LogicalPath,
        /// OS error.
        source: io::Error,
    },
    /// Requested path is not a directory (`ENOTDIR`/`ENOENT` from `os.ReadDir`).
    NotADirectory {
        /// The path that was listed.
        path: LogicalPath,
        /// What it actually is.
        kind: EntryKind,
        /// OS error.
        source: io::Error,
    },
    /// Any other I/O failure (`*fs.PathError` in Go).
    Io {
        /// Best available logical path.
        path: LogicalPath,
        /// OS error.
        source: io::Error,
    },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::UnsupportedScheme { scheme } => write!(
                f,
                "schema '{}' is not supported (only '{}')",
                scheme, LOCAL_SCHEMA_ID
            ),
            StorageError::UnsupportedAuthority { uri, authority } => {
                write!(f, "{uri}: authority '{authority}' is not supported")
            }
            StorageError::UnsupportedUrlComponent { uri, component } => {
                write!(f, "{uri}: a {component} is not part of a file path")
            }
            StorageError::InvalidStorageUri { uri, reason } => {
                write!(f, "{uri}: invalid file uri ({reason})")
            }
            StorageError::InvalidLogicalPath { path } => {
                write!(f, "invalid logical path '{}'", path)
            }
            StorageError::AncestorIsSymlink { path, target } => write!(
                f,
                "ancestor '{}' is a symlink (-> {}); following is disabled",
                path,
                target.display()
            ),
            StorageError::AncestorNotDirectory { path, kind } => {
                write!(f, "ancestor '{}' is not a directory ({:?})", path, kind)
            }
            StorageError::EntryIsSymlink { path, target } => write!(
                f,
                "'{}' is a symlink (-> {}); following is disabled",
                path,
                target.display()
            ),
            StorageError::NotFound { path, source } => write!(f, "{}: {}", path, source),
            StorageError::PermissionDenied { path, source } => write!(f, "{}: {}", path, source),
            StorageError::NotADirectory { path, kind, source } => {
                write!(f, "{}: not a directory ({:?}): {}", path, kind, source)
            }
            StorageError::Io { path, source } => write!(f, "{}: {}", path, source),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::NotFound { source, .. }
            | StorageError::PermissionDenied { source, .. }
            | StorageError::NotADirectory { source, .. }
            | StorageError::Io { source, .. } => Some(source),
            StorageError::UnsupportedScheme { .. }
            | StorageError::UnsupportedAuthority { .. }
            | StorageError::UnsupportedUrlComponent { .. }
            | StorageError::InvalidStorageUri { .. }
            | StorageError::InvalidLogicalPath { .. }
            | StorageError::AncestorIsSymlink { .. }
            | StorageError::AncestorNotDirectory { .. }
            | StorageError::EntryIsSymlink { .. } => None,
        }
    }
}

#[cfg(test)]
mod stat_tests {
    use super::*;

    #[test]
    fn unavailable_timestamps_stay_unavailable() {
        // The states Go's BirthTime would paper over with time.Now() (`core/storage/local/local.go:120-128`).
        let stat = file_stat(7, Err(StatUnavailable), Err(BirthTimeUnavailable));
        assert_eq!(stat.size, 7);
        assert_eq!(stat.modified, Err(StatUnavailable));
        assert_eq!(stat.created, Err(BirthTimeUnavailable));
        assert!(stat
            .created
            .unwrap_err()
            .to_string()
            .contains("birth time not reported"));
        assert!(stat
            .modified
            .unwrap_err()
            .to_string()
            .contains("timestamp not reported"));
    }

    #[test]
    fn present_timestamps_pass_through_untouched() {
        let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let stat = file_stat(3, Ok(stamp), Ok(stamp));
        assert_eq!(stat.modified, Ok(stamp));
        assert_eq!(stat.created, Ok(stamp));
        // One field missing does not disturb the other.
        let partial = file_stat(3, Ok(stamp), Err(BirthTimeUnavailable));
        assert_eq!(partial.modified, Ok(stamp));
        assert!(partial.created.is_err());
    }
}

#[cfg(test)]
mod uri_tests {
    use super::*;

    fn path_of(uri: &str) -> PathBuf {
        LocalDir::open(uri)
            .unwrap_or_else(|err| panic!("open {uri}: {err}"))
            .root
            .clone()
    }

    #[test]
    fn file_urls_are_percent_decoded_exactly_once() {
        // Values verified against Go net/url (go1.27.4) url.Parse u.Path.
        assert_eq!(
            path_of("file:///tmp/a%20b/x.mp3"),
            PathBuf::from("/tmp/a b/x.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/a%25b/x.mp3"),
            PathBuf::from("/tmp/a%b/x.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/a%2520b/x.mp3"),
            PathBuf::from("/tmp/a%20b/x.mp3"),
            "double-encoded stays literal after one decode"
        );
        assert_eq!(
            path_of("file:///tmp/a%23b/x.mp3"),
            PathBuf::from("/tmp/a#b/x.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/a%3Fb/x.mp3"),
            PathBuf::from("/tmp/a?b/x.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/a%2Bb/x.mp3"),
            PathBuf::from("/tmp/a+b/x.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/a+b/x.mp3"),
            PathBuf::from("/tmp/a+b/x.mp3"),
            "+ is not a space"
        );
        assert_eq!(
            path_of("file:///tmp/%C3%BC.mp3"),
            PathBuf::from("/tmp/ü.mp3")
        );
        assert_eq!(
            path_of("file:///tmp/u%CC%88.mp3"),
            PathBuf::from("/tmp/u\u{308}.mp3"),
            "combining mark reassembles from bytes"
        );
        assert_eq!(
            path_of("file:/tmp/x.mp3"),
            PathBuf::from("/tmp/x.mp3"),
            "no authority"
        );
        assert_eq!(
            path_of("file://localhost/tmp/x.mp3"),
            PathBuf::from("/tmp/x.mp3"),
            "localhost authority is conventional"
        );
        // %2F decodes to a real separator, as Go's u.Path does. Examined, not "safe":
        // the root is trusted configuration, so no containment is claimed either way.
        assert_eq!(
            path_of("file:///tmp/a%2Fb.mp3"),
            PathBuf::from("/tmp/a/b.mp3")
        );
        // %2E%2E decodes to literal ".." text inside a component name.
        assert_eq!(
            path_of("file:///tmp/a%2E%2E/b.mp3"),
            PathBuf::from("/tmp/a../b.mp3")
        );
    }

    #[test]
    fn file_urls_percent_decode_bare_paths_are_verbatim() {
        // Go: For() checks for "://" before url.Parse, so a schema-less string is a local
        // path — never decoded.
        for raw in [
            "/tmp/50%20off",
            "/tmp/a%b",
            "/tmp/a#b",
            "/tmp/a?b",
            "/tmp/ü",
            "C:\\music\\a.mp3",
            "C:/music",
        ] {
            let dir = LocalDir::open(raw).unwrap_or_else(|err| panic!("open {raw}: {err}"));
            assert_eq!(dir.root(), Path::new(raw), "{raw} preserved verbatim");
        }
        assert_eq!(
            LocalDir::from_path("/tmp/a%20b").unwrap().root(),
            Path::new("/tmp/a%20b")
        );
        assert!(LocalDir::from_path("").is_err(), "empty path");

        // A scheme prefix without an authority is still a URL, so a remote-looking URI can
        // never open as a local directory (Go's For would take it as a literal path).
        let err = LocalDir::open("http:/host/x.mp3").unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedScheme { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn malformed_uris_are_typed_errors_not_paths() {
        for uri in [
            "file:///tmp/a%zz.mp3",
            "file:///tmp/%E0%A4%A.mp3",
            "file:///tmp/a%2.mp3",
            "file:///tmp/a%00b.mp3",
            "file://",
            "file:///tmp/%.mp3",
        ] {
            let err = LocalDir::open(uri).expect_err("must reject {uri}");
            assert!(
                matches!(err, StorageError::InvalidStorageUri { .. }),
                "{uri} → {err:?}"
            );
        }
    }

    #[test]
    fn authority_query_and_fragment_are_not_treated_as_paths() {
        let err = LocalDir::open("file://example.com/tmp/x.mp3").unwrap_err();
        assert!(
            matches!(&err, StorageError::UnsupportedAuthority { authority, .. } if authority == "example.com"),
            "got {err:?}"
        );
        for uri in ["file:///tmp/a.mp3?x=1", "file:///tmp/a.mp3#frag"] {
            let err = LocalDir::open(uri).unwrap_err();
            assert!(
                matches!(err, StorageError::UnsupportedUrlComponent { .. }),
                "{uri} → {err:?}"
            );
        }
    }

    #[test]
    fn other_schemes_are_rejected_not_emulated() {
        for uri in [
            "s3://bucket/x",
            "sftp://h/x",
            "http://h/x",
            "gs://b/x",
            "smb://s/m",
        ] {
            let err = LocalDir::open(uri).unwrap_err();
            assert!(
                matches!(err, StorageError::UnsupportedScheme { .. }),
                "{uri} → {err:?}"
            );
            assert!(
                err.to_string().contains("only 'file'"),
                "names the scheme: {err}"
            );
        }
    }

    #[test]
    fn file_scheme_matches_ascii_case_insensitively() {
        // Go: url.Parse lowercases the scheme (RFC 3986). Verified with go1.27.4 —
        // "FILE:///tmp/x" and "FiLe:/tmp/x" both yield scheme "file" and the same path.
        for scheme in ["file", "FILE", "File", "FiLe"] {
            let absolute = format!("{scheme}:///tmp/a%20b/x.mp3");
            assert_eq!(
                path_of(&absolute),
                PathBuf::from("/tmp/a b/x.mp3"),
                "{absolute}"
            );
            // Same scheme in the authority-less `file:/path` form.
            let short = format!("{scheme}:/tmp/a%20b/x.mp3");
            assert_eq!(path_of(&short), PathBuf::from("/tmp/a b/x.mp3"), "{short}");
            // Unsupported schemes keep being rejected in every spelling.
            for other in ["S3", "HTTPS", "smb"] {
                let err = LocalDir::open(&format!("{other}://x/y")).unwrap_err();
                assert!(
                    matches!(err, StorageError::UnsupportedScheme { .. }),
                    "{other}://x/y → {err:?}"
                );
            }
        }

        // ASCII-only folding: a Unicode lookalike is not accepted as `file`.
        let err = LocalDir::open("\u{fb01}:///tmp/x.mp3").unwrap_err();
        assert!(
            matches!(err, StorageError::UnsupportedScheme { .. }),
            "ligature 'fi' + le is not the file scheme, got {err:?}"
        );
    }

    #[test]
    fn localhost_authority_is_case_insensitive_but_nothing_else_is_allowed() {
        for authority in ["", "localhost", "LOCALHOST", "LocalHost"] {
            let uri = format!("file://{authority}/tmp/x.mp3");
            assert_eq!(path_of(&uri), PathBuf::from("/tmp/x.mp3"), "{uri}");
        }
        // Broadening is limited to case: userinfo, ports and other hosts stay rejected.
        for uri in [
            "file://LOCALHOST:80/tmp/x.mp3",
            "file://localhost:80/tmp/x.mp3",
            "file://u@LOCALHOST/tmp/x.mp3",
            "file://u@localhost/tmp/x.mp3",
            "file://LOCALHOST.example.com/tmp/x.mp3",
            "file://LOCALHOST.local/tmp/x.mp3",
            "file://example.com/tmp/x.mp3",
        ] {
            let err = LocalDir::open(uri).unwrap_err();
            assert!(
                matches!(err, StorageError::UnsupportedAuthority { .. }),
                "{uri} must be rejected, got {err:?}"
            );
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn windows_volume_uris_are_rejected_on_an_untested_platform() {
        let err = LocalDir::open("file:///C:/music/x.mp3")
            .expect_err("untested volume handling must not pretend to work");
        assert!(
            matches!(err, StorageError::InvalidStorageUri { .. }),
            "got {err:?}"
        );
    }
}
