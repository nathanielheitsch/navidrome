//! Scanner contract types for the SC-02 remainder: what a walk is asked to do,
//! what it emits, how it fails, and how it is cancelled.
//!
//! Ports/adapts these Go sources, with the mirrored file:line cited where a
//! behaviour is a translation rather than an adaptation:
//! - `scanner/walk_dir_tree.go:23-74` (`walkDirTree`: target list, missing-target
//!   skip, follow-disabled symlinked target skip, per-target ignore stack).
//! - `scanner/walk_dir_tree.go:315-324` (`ignoredDirs`, case-insensitive blocklist).
//! - `scanner/walk_dir_tree.go:332-352` (`isIgnoredEntry`, `isDirIgnored`, `isDotEntry`).
//! - `utils/time.go:11-19` (`TimeNewest` → [`newest_time`]).
//! - `consts/consts.go:92` (`ScanIgnoreFile`) → [`SCAN_IGNORE_FILE`].
//!
//! ## What this module does *not* claim
//!
//! No database, no tag reading, no persistence, no controller/phases, no watcher,
//! no progress IPC. Nothing here reports scan success against prior DB state.
//!
//! ## Cancellation (deliberate design, not a port)
//!
//! `utils/context.Cancellation` in Go is a plain `context.Context`; the Rust
//! counterpart `nd_utils::context::Cancellation` is only an `Arc<AtomicBool>`,
//! which cannot wake a producer that is parked on a full queue. [`CancelToken`]
//! is therefore a scanner-local wrapper with a *wakeup*: blocking workers park on
//! a condvar, async consumers select on a `tokio::sync::watch` (both already in
//! `rust/Cargo.lock`). Cancellation is **cooperative**: it never interrupts an
//! in-flight OS syscall — the worker is always joined afterwards.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use tokio::sync::watch;

use crate::storage::{LogicalPath, StorageError};

/// `consts/consts.go:92` — `ScanIgnoreFile = ".ndignore"`.
pub const SCAN_IGNORE_FILE: &str = ".ndignore";

/// `scanner/walk_dir_tree.go:315-324` — `ignoredDirs`. Go compares each entry with
/// `strings.EqualFold`; [`is_dir_ignored`] reproduces that fold for these names rather
/// than claiming general Unicode case folding. [`is_ignored_entry`] consults this
/// blocklist first, in Go's own order.
pub const IGNORED_DIRS: &[&str] = &[
    "$RECYCLE.BIN",
    "#snapshot",
    "@Recycle",
    "@eaDir",
    "@Recently-Snapshot",
    ".git",
    ".streams",
    "lost+found",
];

/// `scanner/walk_dir_tree.go:341-343` — `isDirIgnored`: membership in [`IGNORED_DIRS`]
/// under Go's simple case fold. Only the fold orbits that intersect ASCII are
/// implemented — long-s folds to S and the Kelvin sign to K — which is exactly
/// `EqualFold` for every name above; this is not generic Unicode folding.
#[must_use]
pub fn is_dir_ignored(name: &str) -> bool {
    IGNORED_DIRS.iter().any(|blocked| {
        blocked
            .chars()
            .map(blocklist_fold)
            .eq(name.chars().map(blocklist_fold))
    })
}

fn blocklist_fold(c: char) -> char {
    match c {
        'ſ' => 'S',
        'K' => 'K',
        c => c.to_ascii_uppercase(),
    }
}

#[cfg(test)]
mod fold_tests {
    use super::blocklist_fold;

    #[test]
    fn fold_orbits_intersecting_ascii_include_kelvin_and_long_s() {
        for (left, right) in [('K', 'k'), ('ſ', 's'), ('K', 'k'), ('S', 's')] {
            assert_eq!(blocklist_fold(left), blocklist_fold(right));
        }
        assert_ne!(blocklist_fold('ß'), blocklist_fold('s'));
    }
}

/// `scanner/walk_dir_tree.go:350-352` — `isDotEntry`: exactly one leading dot.
/// `"."`, `".."`, `"..foo"` and `"...Album"` are deliberately **not** hidden.
#[must_use]
pub fn is_dot_entry(name: &str) -> bool {
    name != "." && name.starts_with('.') && !name.starts_with("..")
}

/// `scanner/walk_dir_tree.go:332-337` — `isIgnoredEntry`.
///
/// `ignore_dot_folders` mirrors `conf.Server.Scanner.IgnoreDotFolders` (default
/// `true`): dot *folders* are ignored while it is set, so an album folder named
/// `.Hack Sign` is scanned only when the caller opts out.
///
/// Order matches Go exactly (`S9` item b, corrected): Go also tests the blocklist first
/// (`if isDir && isDirIgnored(name) { return true }`) and only then the dot rule, so a
/// blocklisted dot-prefixed name such as `.git` or `.streams` is decided by the blocklist
/// in both implementations. Go returns a plain `bool` and reports no reason, and this
/// returns a plain `bool` too — there is no divergence to record here.
#[must_use]
pub fn is_ignored_entry(name: &str, is_dir: bool, ignore_dot_folders: bool) -> bool {
    if is_dir && is_dir_ignored(name) {
        return true;
    }
    is_dot_entry(name) && (!is_dir || ignore_dot_folders)
}

/// `utils/time.go:11-19` — `TimeNewest`: the newest of the given times, or the
/// zero time when none is newer than it.
#[must_use]
pub fn newest_time(times: &[SystemTime]) -> Option<SystemTime> {
    times.iter().max().copied()
}

/// One configured music library root: an OS directory plus its (trusted) ID.
///
/// Go reads the library list from the database (`s.ds.Library().GetAll`,
/// `scanner/scanner.go:76`); here the caller injects the list explicitly
/// (workspace freeze: explicit construction instead of a global config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRoot {
    /// Stable library identifier, matching Go's 1-based `library.ID`.
    pub id: i64,
    /// Display name, used in diagnostics only.
    pub name: String,
    /// Configured root, trusted configuration (a symlinked root is followed).
    pub root: OsString,
}

/// A single directory to walk inside a library, given as a validated logical path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkTarget {
    /// `LibraryRoot::id` this target belongs to.
    pub library_id: i64,
    /// Library-relative logical path; [`LogicalPath::root`] means "whole library".
    pub path: LogicalPath,
}

/// What the walk should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkRequest {
    /// Libraries to walk. An empty list is an **error**, not an empty success.
    pub libraries: Vec<LibraryRoot>,
    /// Targets within those libraries. Empty means every configured library root
    /// (Go: `scanner/walk_dir_tree.go:29-33` defaults to `"."`).
    pub targets: Vec<WalkTarget>,
    /// `conf.Server.Scanner.FollowSymlinks`.
    pub follow_symlinks: bool,
    /// `conf.Server.Scanner.IgnoreDotFolders`.
    pub ignore_dot_folders: bool,
}

impl WalkRequest {
    /// Validate the request and return the effective target list.
    ///
    /// Rules: no libraries is a typed error (never an empty success); targets
    /// naming an unknown library are rejected; duplicate targets are collapsed
    /// deterministically (accepted normalization deviation, not literal-list Go parity);
    /// distinct overlapping targets traverse independently. Targets are sorted by
    /// `(library_id, logical path)`.
    pub fn validated_targets(&self) -> Result<Vec<WalkTarget>, ScanError> {
        if self.libraries.is_empty() {
            return Err(ScanError::NoLibraries);
        }
        let mut targets = Vec::new();
        for target in &self.targets {
            // Unknown library ids are rejected rather than dropped, so a typo can never
            // silently scan nothing. `LogicalPath` already rejects absolute paths, `..`,
            // empty components and backslashes at construction time.
            if !self.libraries.iter().any(|l| l.id == target.library_id) {
                return Err(ScanError::UnknownLibrary {
                    library_id: target.library_id,
                    path: target.path.clone(),
                });
            }
            targets.push(target.clone());
        }
        if targets.is_empty() {
            for library in &self.libraries {
                targets.push(WalkTarget {
                    library_id: library.id,
                    path: LogicalPath::root(),
                });
            }
        }
        targets.sort_by(|a, b| {
            a.library_id
                .cmp(&b.library_id)
                .then_with(|| a.path.cmp(&b.path))
        });
        targets.dedup_by(|a, b| a.library_id == b.library_id && a.path == b.path);
        Ok(targets)
    }
}

/// Wakeable, monotonic cancellation shared by producer and consumer.
///
/// `Clone` hands out further observers of the *same* state. Once cancelled it can
/// never be un-cancelled, so a late send can never resurrect a stopped walk.
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug)]
struct CancelInner {
    /// Set once, never cleared.
    cancelled: AtomicBool,
    /// Wakeup for blocking workers (parked while the folder queue is full) and for
    /// "a folder was consumed, retry the queue" notifications.
    wake: Mutex<()>,
    wake_cv: Condvar,
    /// Wakeup for async consumers (`tokio::select!`).
    async_tx: watch::Sender<bool>,
}

impl fmt::Display for CancelToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CancelToken({})",
            if self.is_cancelled() {
                "cancelled"
            } else {
                "active"
            }
        )
    }
}

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> CancelToken {
        let (tx, _rx) = watch::channel(false);
        CancelToken {
            inner: Arc::new(CancelInner {
                cancelled: AtomicBool::new(false),
                wake: Mutex::new(()),
                wake_cv: Condvar::new(),
                async_tx: tx,
            }),
        }
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Request cancellation. Idempotent, and wakes every parked waiter.
    pub fn cancel(&self) {
        // Take the lock before notifying so a waiter that just re-checked the flag
        // cannot miss the wakeup (lost-wakeup race).
        let guard = self.lock_wake();
        self.inner.cancelled.store(true, Ordering::SeqCst);
        drop(guard);
        self.inner.wake_cv.notify_all();
        self.inner.async_tx.send_replace(true);
    }

    /// Unblock blocking producers waiting for queue space. Called by the consumer
    /// after it takes a folder; cancellation is re-checked by the waiter, so this
    /// is only a "retry now" hint plus a safety net for lost notifications.
    pub fn notify_queue_space(&self) {
        let _guard = self.lock_wake();
        self.inner.wake_cv.notify_all();
    }

    /// Async view: resolves as soon as the token is cancelled.
    ///
    /// A token cancelled before this future is polled still resolves immediately:
    /// the watch value already holds `true`.
    pub async fn cancelled(&self) {
        let mut rx = self.inner.async_tx.subscribe();
        if *rx.borrow_and_update() {
            return;
        }
        // `Err` means the sender was dropped; treat that as "no more updates" and
        // fall back to the flag, which is still authoritative.
        let _ = rx.changed().await;
    }

    /// Park until cancellation **or** a wakeup hint, returning `true` when cancelled.
    ///
    /// Used by the blocking producer while the folder queue is full. A hint is either
    /// `cancel` or [`CancelToken::notify_queue_space`]; the producer re-checks both the
    /// flag and the send after every return, so a lost hint costs at most
    /// [`QUEUE_SPACE_RETRY_INTERVAL`] instead of hanging the walk.
    pub fn wait_for_wakeup(&self) -> bool {
        let mut guard = self.lock_wake();
        while !self.is_cancelled() {
            let (next, _wait) = self
                .inner
                .wake_cv
                .wait_timeout(guard, QUEUE_SPACE_RETRY_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next;
            // A notification (queue space) ends the park; cancellation ends it too.
            if !self.is_cancelled() {
                return false;
            }
        }
        true
    }

    fn lock_wake(&self) -> MutexGuard<'_> {
        self.inner
            .wake
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Interval the producer re-checks queue space when no hint arrives.
const QUEUE_SPACE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Private alias: only the wakeup helpers need the guard type named. The mutex guards the
/// wakeup state, so `()` is its payload.
type MutexGuard<'a> = std::sync::MutexGuard<'a, ()>;

/// Categories of scanner error. Concrete storage failures stay typed sources
/// rather than strings, so later SC-06 policy can distinguish missing from denied.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// The request named no library. Never treated as an empty success.
    #[error("no libraries configured for this walk")]
    NoLibraries,
    /// A target names a library the request does not configure.
    #[error("target '{path}' names unknown library {library_id}")]
    UnknownLibrary {
        /// Rejected library id.
        library_id: i64,
        /// The target's logical path.
        path: LogicalPath,
    },
    /// Opening the library root failed (`LocalDir::from_path`).
    #[error("library {library_id} root cannot be used: {source}")]
    InvalidTarget {
        /// Rejected library id.
        library_id: i64,
        /// Root as given.
        root: String,
        /// Storage-level cause.
        source: StorageError,
    },
    /// The root itself could not be listed, so nothing is known about the library.
    #[error("cannot list library root '{path}': {source}")]
    RootUnreadable {
        /// Root path as given.
        path: String,
        /// Storage-level cause.
        source: StorageError,
    },
    /// A logical path could not be joined (defensive; `LogicalPath` validates).
    #[error("invalid logical path: {source}")]
    InvalidLogicalPath {
        /// Storage-level cause.
        source: StorageError,
    },
    /// A directory-entry name is not valid UTF-8, so it cannot be classified or
    /// used as a logical path. Reported, never lossily rendered.
    #[error("'{name}' is not valid text on this platform")]
    NonUtf8Name {
        /// Lossy rendering, diagnostics only.
        name: String,
    },
    /// The fingerprint of a folder could not be built from its observed facts.
    #[error("folder hash failed: {error}")]
    HashTime {
        /// The hash-level cause, kept typed.
        error: crate::folder::FolderSnapshotError,
    },
    /// The walk was cancelled. Distinct from "finished with warnings".
    #[error("walk cancelled")]
    Cancelled,
    /// The worker task could not be joined (it panicked).
    #[error("walk worker panicked while {phase}")]
    WorkerPanic {
        /// What the worker was doing when it died.
        phase: &'static str,
    },
}

/// Non-fatal observations that do not stop the walk but do change what coverage
/// means. Warnings are ordered by emission, and coverage never silently degrades
/// because of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanWarningKind {
    /// `.ndignore` exists but could not be opened: no patterns from it (Go logs a
    /// warning and returns no patterns, `scanner/ignore_checker.go:117-121`).
    IgnoreFileOpenFailed,
    /// `.ndignore` could not be read to completion: the patterns read *before* the
    /// failure are kept (Go returns the accumulated prefix, `scanner/ignore_checker.go:133-136`).
    IgnoreFileReadFailed,
    /// `.ndignore` contains a line the matcher cannot compile: Go silently omits
    /// it; here it is reported (documented deviation, same effective behavior).
    IgnorePatternInvalid,
    /// `.ndignore` contains a construct Rust's regex engine spells differently from
    /// Go's: the pattern is not translated at all rather than reinterpreted.
    IgnorePatternUnsupported,
    /// A `.ndignore` line cannot be decoded as text.
    IgnoreFileNotUtf8,
    /// `.ndignore` contains a line longer than [`MAX_IGNORE_LINE_BYTES`].
    IgnoreLineTooLong,
    /// A child directory could not be opened, so it is not walked (Go
    /// `isDirReadable`, `scanner/walk_dir_tree.go:301-312`).
    DirectoryUnreadable,
    /// A directory listing failed partway, so that folder is reported as
    /// **incomplete**, never as an empty successful snapshot.
    DirectoryEnumerationFailed,
    /// A file entry could not be stat'd, so it contributes nothing to the snapshot.
    EntryStatFailed,
    /// A symlink entry is skipped: following is disabled, or its endpoint is
    /// broken/looping/not a file.
    SymlinkSkipped,
    /// A directory symlink would create an ancestor cycle, so it is not descended
    /// into. Deliberate safety addition: Go has no directory-ancestor guard.
    DirectoryCycleSkipped,
    /// A directory link or a target reached through one is skipped because
    /// following is disabled (`scanner/walk_dir_tree.go:47-51`).
    SymlinkedTargetSkipped,
    /// A name is not valid UTF-8 and is excluded from the snapshot.
    NameNotUtf8,
    /// A resolved endpoint lies outside the library root and is classified by the
    /// target extension anyway (Go accepts legitimate outside-root audio,
    /// `walk_dir_tree_test.go:520`).
    OutsideRootEndpoint,
    /// A target was skipped because it does not exist
    /// (`scanner/walk_dir_tree.go:39-45`).
    TargetMissing,
    /// The active-ancestor walk exceeded [`crate::walk::MAX_WALK_DEPTH`].
    DepthLimitReached,
    /// A timestamp the fingerprint needs is not reported by this platform.
    TimestampUnavailable,
}

impl ScanWarningKind {
    /// Stable machine-readable name, so a consumer can match on it without
    /// depending on the message text. `warning_kind_strings_are_unique_and_pinned` in
    /// `tests/walk_policy.rs` pins this whole table.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
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
}

/// One warning: its kind plus a lossy path/context for humans.
///
/// The `path` renders through `LogicalPath`'s `Display`, which is lossy (U+FFFD) for
/// non-UTF-8 names; the exact raw bytes survive in `path.components` and the `Debug`
/// form, and `message` carries the exact `\xFF`-style spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanWarning {
    /// What happened.
    pub kind: ScanWarningKind,
    /// Library the warning belongs to, when known.
    pub library_id: Option<i64>,
    /// Logical path the warning is about, when known.
    pub path: Option<LogicalPath>,
    /// Lossy message text, diagnostics only.
    pub message: String,
}

impl fmt::Display for ScanWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self
            .path
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| ".".to_string());
        match self.library_id {
            Some(id) => write!(
                f,
                "lib {}: {} {}: {}",
                id,
                self.kind.as_str(),
                path,
                self.message
            ),
            None => write!(f, "{} {}: {}", self.kind.as_str(), path, self.message),
        }
    }
}

/// Identity of a library + logical path pair, used to collapse duplicate targets.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibraryPath {
    /// Library id.
    pub library_id: i64,
    /// Logical path relative to that library's root.
    pub path: LogicalPath,
}

impl LibraryPath {
    /// Build the identity pair.
    #[must_use]
    pub fn new(library_id: i64, path: LogicalPath) -> LibraryPath {
        LibraryPath { library_id, path }
    }
}

impl fmt::Display for LibraryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lib{}:{}", self.library_id, self.path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A name borrowed as text, or the typed reason it cannot be.
///
/// Go's `filepath.WalkDir`-style code paths pass `string` names; here the raw
/// `OsStr` is kept and the UTF-8 boundary is an explicit outcome, so a non-UTF8
/// name can never collide with a real name through `to_string_lossy`.
pub enum NameText<'a> {
    /// Valid UTF-8, usable as a logical/classification name.
    Valid(&'a str),
    /// Not valid UTF-8: caller must decide the typed outcome.
    Invalid,
}

/// Classify one entry name, returning its text form when decodable.
#[must_use]
pub fn name_text(name: &OsStr) -> NameText<'_> {
    match name.to_str() {
        Some(text) => NameText::Valid(text),
        None => NameText::Invalid,
    }
}
