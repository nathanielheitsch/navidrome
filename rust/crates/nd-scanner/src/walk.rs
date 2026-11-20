//! Bounded, cancellable, deterministic postorder walk over local library trees.
//!
//! Sources, mirrored:
//! - `scanner/walk_dir_tree.go:23-74` (`walkDirTree`: per-target skip rules, fresh
//!   ignore checker per target, `close(results)` on completion).
//! - `scanner/walk_dir_tree.go:76-106` (`walkFolder`: push/pop, child-before-parent
//!   emission, `select` on the result channel and `ctx.Done()`).
//! - `scanner/walk_dir_tree.go:108-181` (`loadDir`: dir stat, ignore checks, name policy,
//!   classification, timestamp accumulation, `numSubFolders`).
//! - `scanner/walk_dir_tree.go:187-208` (`fullReadDir` — see the coverage deviation).
//! - `scanner/walk_dir_tree.go:301-312` (`isDirReadable`), `scanner/walk_dir_tree.go:315-324` (`ignoredDirs`).
//! - `scanner/walk_dir_tree.go:237-245` (`isSymlinkedPath`).
//! - `scanner/walk_dir_tree.go:253-298` (`resolveEntryName`: classify by the resolved
//!   endpoint's base name, keep the link's own name as identity).
//!
//! ## Honest bounds
//!
//! One folder is queued at a time (`mpsc` capacity 1, like Go's unbuffered result
//! channel being drained one folder at a time). Memory is O(active DFS frames + their
//! captured listings + the single queued folder) — **not** a byte or file-count bound.
//! The tree is never materialized: a listing is captured, folded into a snapshot, and
//! released. Depth is capped at [`MAX_WALK_DEPTH`] (Go recurses until the goroutine stack
//! grows; a cap is a defined warning rather than a crash).
//!
//! ## Cancellation and lifecycle contract
//!
//! [`CancelToken`](crate::contract::CancelToken) wakes the worker while it waits for queue
//! space, and the async consumer selects on the same token. A plain atomic flag could never
//! wake a full send, so the wakeup is part of the contract. Cancellation is
//! **cooperative**: an in-flight OS syscall is never interrupted — the worker finishes the
//! current operation, observes the flag, and is then joined.
//!
//! Receiver EOF is **never** success. [`WalkHandle::terminal`] delivers the authoritative
//! [`WalkSummary`] exactly once, and every consuming path (`terminal`, `cancel_and_join`,
//! `drain_and_join`) joins the worker, so no worker thread leaks. Dropping the receiver
//! without joining leaves the worker running until it notices the closed channel; that is
//! reported as `WalkOutcome::Cancelled`, not as success.
//!
//! ## Declared deviations from Go (reported as warnings, never silent)
//!
//! * Go's `fullReadDir` keeps the entries read before a failure and `loadDir` returns an
//!   error which `walkFolder` logs and swallows. Here a **failed enumeration marks the
//!   folder incomplete and emits no snapshot**: conservative incomplete coverage, never a
//!   fake empty folder. `LocalDir::list_annotated` fails like `fs.ReadDir` on the first
//!   error, so Go's repeated-error "bail" loop is not reproduced — the conservative
//!   outcome stands in for it and is documented.
//! * Go has **no directory-ancestor cycle guard**. This walker suppresses cycles using the
//!   *active* resolved directory identity (canonicalized OS paths on the current DFS path
//!   only). Distinct symlink aliases outside an active cycle are still indexed separately,
//!   as Go does; there is no global visited set.
//! * Non-UTF-8 entry names are excluded with a raw-path warning and their owning folder
//!   marked `EntriesIncomplete`. Go stores byte names; lossy names would collide.
//! * A directory that cannot be opened is treated as a plain entry (Go falls through to
//!   the file branch and does not increment `numSubFolders`), with
//!   [`ScanWarningKind::DirectoryUnreadable`].
//! * `.ndignore` files are read as UTF-8 text; see the ignore module's deviation notes.
//! * Name ignore policy mirrors Go exactly: `is_ignored_entry` tests the `ignoredDirs`
//!   blocklist first and the dot rule second, in the same order as Go's `isIgnoredEntry`
//!   (`scanner/walk_dir_tree.go:332-337`), and both return a plain bool with no reported
//!   reason. No deviation; an earlier note here wrongly claimed the opposite order and a
//!   Go flag that does not exist (`S9` item b).
//! * Go's `PushAllParents` pushes the target folder's own patterns a second time when
//!   `walkFolder` enters it. That duplication is reproduced: matching is idempotent under
//!   it, and reproducing it keeps the stack depth identical.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::sync::{mpsc, oneshot};

use crate::contract::{
    CancelToken, ScanError, ScanWarning, ScanWarningKind, WalkRequest, WalkTarget,
};
use crate::folder::{
    newest, EntryCategory, FolderCoverage, FolderEntry, FolderSnapshot, FolderSnapshotError,
};
use crate::ignore::{read_ignore_file, IgnoreStack};
use crate::storage::{
    EndpointKind, EntryFacts, EntryKind, FileStat, Follow, LocalDir, LogicalPath, ResolvedTarget,
    StorageError,
};

/// Maximum active DFS depth. Go recurses without a cap; this makes an absurdly deep tree a
/// typed warning instead of a stack overflow.
pub const MAX_WALK_DEPTH: usize = 255;

/// The depth gate as a pure function of the active DFS length, so the boundary is
/// testable at an arbitrary limit without changing [`MAX_WALK_DEPTH`] itself (`S9` item d).
/// `active` already holds the seed root identity plus one identity per entered
/// directory, so entering a directory while `active_len` identities are live makes the
/// walk `active_len + 1` deep; `active_len + 1 > limit` is exactly `active_len >= limit`.
#[must_use]
fn depth_limit_exceeded(active_len: usize, limit: usize) -> bool {
    active_len >= limit
}

/// One folder emitted by the walk, with the fingerprint computed from the captured
/// snapshot facts (no re-stat during hashing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEvent {
    /// The immutable snapshot.
    pub snapshot: FolderSnapshot,
    /// MD5 over [`FolderSnapshot::hash_preimage`], lowercase hex — the value Go stores.
    pub hash: String,
}

/// Terminal outcome of a walk.
///
/// `ScanError` carries an `io::Error`, so this compares through
/// [`WalkOutcome::is_complete`] / [`WalkOutcome::describe`] rather than `PartialEq`, and
/// it is not `Clone` because the failure carries an OS error.
#[derive(Debug)]
pub enum WalkOutcome {
    /// Every configured target was walked to completion.
    Complete,
    /// Cancellation was requested, or the receiver went away; the summary is partial.
    Cancelled,
    /// A fatal error stopped the walk.
    Failed(ScanError),
}

/// Aggregate of a walk. Reported only through [`WalkHandle::terminal`] (or the join
/// helpers), never inferred from receiver EOF.
#[derive(Debug)]
pub struct WalkSummary {
    /// How the walk ended.
    pub outcome: WalkOutcome,
    /// Folders emitted with a complete or entry-incomplete snapshot.
    pub folders_emitted: usize,
    /// Folders whose enumeration was incomplete, so no snapshot was emitted.
    pub folders_incomplete: usize,
    /// Targets walked.
    pub targets_walked: usize,
    /// Targets skipped (missing, or symlinked with following disabled).
    pub targets_skipped: usize,
    /// Every warning emitted, in order.
    pub warnings: Vec<ScanWarning>,
}

impl WalkOutcome {
    /// The only successful outcome.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, WalkOutcome::Complete)
    }

    /// Whether cancellation (or a dropped receiver) ended the walk.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, WalkOutcome::Cancelled)
    }

    /// Short text form for assertions and diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            WalkOutcome::Complete => "complete".to_string(),
            WalkOutcome::Cancelled => "cancelled".to_string(),
            WalkOutcome::Failed(error) => format!("failed: {error}"),
        }
    }
}

impl WalkSummary {
    /// Warnings of one kind, in emission order.
    #[must_use]
    pub fn warnings_of(&self, kind: ScanWarningKind) -> Vec<&ScanWarning> {
        self.warnings
            .iter()
            .filter(|warning| warning.kind == kind)
            .collect()
    }
}

/// Folder event stream plus the authoritative terminal outcome.
#[derive(Debug)]
pub struct WalkHandle {
    receiver: mpsc::Receiver<FolderEvent>,
    terminal: Option<oneshot::Receiver<WalkSummary>>,
    cancel: CancelToken,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl WalkHandle {
    /// Start a walk. The worker runs on its own blocking thread: filesystem syscalls are
    /// never executed on an async worker thread.
    #[must_use]
    pub fn start(request: WalkRequest, cancel: CancelToken) -> WalkHandle {
        let (tx, rx) = mpsc::channel::<FolderEvent>(1);
        let (done_tx, done_rx) = oneshot::channel::<WalkSummary>();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("nd-scanner-walk".to_string())
            .spawn(move || {
                let summary = run_walk(&request, &tx, &worker_cancel);
                // A dropped receiver simply discards the summary; the thread still
                // returns, so the join never hangs.
                let _ = done_tx.send(summary);
            })
            .expect("spawn nd-scanner walk worker");
        WalkHandle {
            receiver: rx,
            terminal: Some(done_rx),
            cancel,
            worker: Some(worker),
        }
    }

    /// Next folder event, or `None` after cancellation or producer closure.
    ///
    /// `None` is never success: use [`WalkHandle::terminal`] for the outcome.
    pub async fn next(&mut self) -> Option<FolderEvent> {
        tokio::select! {
            event = self.receiver.recv() => {
                if event.is_some() {
                    // Wake a producer parked on a full queue.
                    self.cancel.notify_queue_space();
                }
                event
            }
            () = self.cancel.cancelled() => None,
        }
    }

    /// Request cancellation, drop the receiver (so a producer parked on a full queue also
    /// sees the closed channel), and join. The summary's
    /// [`WalkSummary::outcome`](WalkOutcome::Cancelled) is the authoritative result.
    pub async fn cancel_and_join(mut self) -> WalkSummary {
        self.cancel.cancel();
        self.drop_receiver();
        self.finish().await
    }

    /// Drain every remaining event, then join. Tests use this to assert termination
    /// without hanging.
    pub async fn drain_and_join(mut self) -> WalkSummary {
        while self.receiver.recv().await.is_some() {
            self.cancel.notify_queue_space();
        }
        self.finish().await
    }

    /// Drain and discard events through EOF, then join and return the producer outcome.
    /// `Complete` describes producer completion, not observation by another consumer.
    /// Use `next()` to retain events before joining.
    pub async fn terminal(self) -> WalkSummary {
        self.drain_and_join().await
    }

    /// Cancel without joining; the worker stops on its own once the channel is closed.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Explicitly drop the receiver, which makes the producer see a closed channel.
    pub fn close_receiver(&mut self) {
        self.drop_receiver();
    }

    fn drop_receiver(&mut self) {
        // Replacing with an unused receiver drops the original one, closing the channel
        // from the consumer side.
        let (_, unused) = mpsc::channel::<FolderEvent>(1);
        let _ = std::mem::replace(&mut self.receiver, unused);
    }

    async fn finish(&mut self) -> WalkSummary {
        let terminal = match self.terminal.take() {
            Some(terminal) => terminal,
            None => {
                self.join_worker();
                return WalkSummary {
                    outcome: WalkOutcome::Failed(ScanError::WorkerPanic {
                        phase: "emitting terminal outcome",
                    }),
                    folders_emitted: 0,
                    folders_incomplete: 0,
                    targets_walked: 0,
                    targets_skipped: 0,
                    warnings: Vec::new(),
                };
            }
        };
        let summary = match terminal.await {
            Ok(summary) => summary,
            // The sender is dropped only when the worker panicked.
            Err(_) => {
                self.join_worker();
                return WalkSummary {
                    outcome: WalkOutcome::Failed(ScanError::WorkerPanic {
                        phase: "emitting terminal outcome",
                    }),
                    folders_emitted: 0,
                    folders_incomplete: 0,
                    targets_walked: 0,
                    targets_skipped: 0,
                    warnings: Vec::new(),
                };
            }
        };
        self.join_worker();
        summary
    }

    fn join_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            // The worker always returns: cancellation and a closed channel are both
            // terminal states. A panic here is a bug in this module.
            let _ = worker.join();
        }
    }
}

/// `ScanError` cannot derive `Clone` because `StorageError` carries an `io::Error`, so a
/// failure is rebuilt field by field where a caller needs an owned copy. OS error codes
/// survive; messages are reconstructed.
pub fn clone_scan_error(error: &ScanError) -> ScanError {
    match error {
        ScanError::NoLibraries => ScanError::NoLibraries,
        ScanError::UnknownLibrary { library_id, path } => ScanError::UnknownLibrary {
            library_id: *library_id,
            path: path.clone(),
        },
        ScanError::InvalidTarget {
            library_id,
            root,
            source,
        } => ScanError::InvalidTarget {
            library_id: *library_id,
            root: root.clone(),
            source: clone_storage_error(source),
        },
        ScanError::RootUnreadable { path, source } => ScanError::RootUnreadable {
            path: path.clone(),
            source: clone_storage_error(source),
        },
        ScanError::InvalidLogicalPath { source } => ScanError::InvalidLogicalPath {
            source: clone_storage_error(source),
        },
        ScanError::NonUtf8Name { name } => ScanError::NonUtf8Name { name: name.clone() },
        ScanError::HashTime { error } => ScanError::HashTime {
            error: clone_folder_error(error),
        },
        ScanError::Cancelled => ScanError::Cancelled,
        ScanError::WorkerPanic { phase } => ScanError::WorkerPanic { phase },
    }
}

fn clone_storage_error(error: &StorageError) -> StorageError {
    match error {
        StorageError::UnsupportedScheme { scheme } => StorageError::UnsupportedScheme {
            scheme: scheme.clone(),
        },
        StorageError::UnsupportedAuthority { uri, authority } => {
            StorageError::UnsupportedAuthority {
                uri: uri.clone(),
                authority: authority.clone(),
            }
        }
        StorageError::UnsupportedUrlComponent { uri, component } => {
            StorageError::UnsupportedUrlComponent {
                uri: uri.clone(),
                component,
            }
        }
        StorageError::InvalidStorageUri { uri, reason } => StorageError::InvalidStorageUri {
            uri: uri.clone(),
            reason,
        },
        StorageError::InvalidLogicalPath { path } => {
            StorageError::InvalidLogicalPath { path: path.clone() }
        }
        StorageError::AncestorIsSymlink { path, target } => StorageError::AncestorIsSymlink {
            path: path.clone(),
            target: target.clone(),
        },
        StorageError::AncestorNotDirectory { path, kind } => StorageError::AncestorNotDirectory {
            path: path.clone(),
            kind: *kind,
        },
        StorageError::EntryIsSymlink { path, target } => StorageError::EntryIsSymlink {
            path: path.clone(),
            target: target.clone(),
        },
        StorageError::NotFound { path, source } => StorageError::NotFound {
            path: path.clone(),
            source: clone_io_error(source),
        },
        StorageError::PermissionDenied { path, source } => StorageError::PermissionDenied {
            path: path.clone(),
            source: clone_io_error(source),
        },
        StorageError::NotADirectory { path, kind, source } => StorageError::NotADirectory {
            path: path.clone(),
            kind: *kind,
            source: clone_io_error(source),
        },
        StorageError::Io { path, source } => StorageError::Io {
            path: path.clone(),
            source: clone_io_error(source),
        },
    }
}

fn clone_io_error(source: &std::io::Error) -> std::io::Error {
    match source.raw_os_error() {
        Some(code) => std::io::Error::from_raw_os_error(code),
        None => std::io::Error::new(source.kind(), source.to_string()),
    }
}

fn clone_folder_error(error: &FolderSnapshotError) -> FolderSnapshotError {
    use FolderSnapshotError as E;
    match error {
        E::TimestampUnavailable { path } => E::TimestampUnavailable { path: path.clone() },
        E::TimestampOutOfRange { path } => E::TimestampOutOfRange { path: path.clone() },
        E::SizeOutOfRange { path, size } => E::SizeOutOfRange {
            path: path.clone(),
            size: *size,
        },
    }
}

/// The blocking worker body.
fn run_walk(
    request: &WalkRequest,
    tx: &mpsc::Sender<FolderEvent>,
    cancel: &CancelToken,
) -> WalkSummary {
    let mut summary = WalkSummary {
        outcome: WalkOutcome::Complete,
        folders_emitted: 0,
        folders_incomplete: 0,
        targets_walked: 0,
        targets_skipped: 0,
        warnings: Vec::new(),
    };

    let targets = match request.validated_targets() {
        Ok(targets) => targets,
        Err(error) => {
            summary.outcome = WalkOutcome::Failed(error);
            return summary;
        }
    };
    // Validate every logical component before opening roots or traversing anything.
    // Configured OS roots are trusted and intentionally outside this UTF-8 boundary.
    for target in &targets {
        for component in target.path.components() {
            if let Err(error) = preflight_component(
                target,
                component,
                crate::contract::name_text(component),
                &mut summary,
            ) {
                summary.outcome = WalkOutcome::Failed(error);
                return summary;
            }
        }
    }

    // Open each configured root once; a root that cannot even be named is fatal.
    let mut stores: Vec<(i64, LocalDir)> = Vec::with_capacity(request.libraries.len());
    for library in &request.libraries {
        match LocalDir::from_path(&library.root) {
            Ok(store) => stores.push((library.id, store)),
            Err(source) => {
                summary.outcome = WalkOutcome::Failed(ScanError::InvalidTarget {
                    library_id: library.id,
                    root: library.root.to_string_lossy().into_owned(),
                    source,
                });
                return summary;
            }
        }
    }

    for target in targets {
        if cancel.is_cancelled() {
            summary.outcome = WalkOutcome::Cancelled;
            return summary;
        }
        let store = match stores.iter().find(|(id, _)| *id == target.library_id) {
            Some((_, store)) => store,
            None => {
                summary.outcome = WalkOutcome::Failed(ScanError::UnknownLibrary {
                    library_id: target.library_id,
                    path: target.path.clone(),
                });
                return summary;
            }
        };

        // `scanner/walk_dir_tree.go:39-45`: a missing target is skipped, not fatal, so a watcher
        // event for a deleted folder stays in the job's last-updates (SC-06 territory).
        if store.snapshot(&target.path, Follow::Yes).is_err() {
            summary.targets_skipped += 1;
            summary.warnings.push(ScanWarning {
                kind: ScanWarningKind::TargetMissing,
                library_id: Some(target.library_id),
                path: Some(target.path.clone()),
                message: "target folder does not exist; skipped".to_string(),
            });
            continue;
        }

        // `scanner/walk_dir_tree.go:47-51`: with following disabled, a target that *is* a link or
        // sits under one is skipped. The configured root itself stays trusted/exempt.
        if !request.follow_symlinks && is_symlinked_path(store, &target.path) {
            summary.targets_skipped += 1;
            summary.warnings.push(ScanWarning {
                kind: ScanWarningKind::SymlinkedTargetSkipped,
                library_id: Some(target.library_id),
                path: Some(target.path.clone()),
                message: "target is a symlink or is reached through one; following is disabled"
                    .to_string(),
            });
            continue;
        }

        match walk_target(request, store, &target, tx, cancel, &mut summary) {
            TargetOutcome::Done => summary.targets_walked += 1,
            TargetOutcome::Cancelled => {
                summary.outcome = WalkOutcome::Cancelled;
                return summary;
            }
            TargetOutcome::Failed(error) => {
                summary.outcome = WalkOutcome::Failed(error);
                return summary;
            }
        }
    }

    if cancel.is_cancelled() {
        summary.outcome = WalkOutcome::Cancelled;
    }
    summary
}

enum TargetOutcome {
    Done,
    Cancelled,
    Failed(ScanError),
}

/// Build the `.ndignore` loader for one store, so the ignore stack stays filesystem-agnostic.
fn ignore_loader(store: &LocalDir) -> impl Fn(&[&OsStr]) -> crate::ignore::IgnoreFileOutcome {
    let root = store.root().to_path_buf();
    move |ancestors| {
        let mut path = root.clone();
        for component in ancestors {
            path.push(component);
        }
        path.push(crate::ignore::ignore_file_name());
        read_ignore_file(&path)
    }
}

fn walk_target(
    request: &WalkRequest,
    store: &LocalDir,
    target: &WalkTarget,
    tx: &mpsc::Sender<FolderEvent>,
    cancel: &CancelToken,
    summary: &mut WalkSummary,
) -> TargetOutcome {
    // `scanner/walk_dir_tree.go:53-59`: a fresh checker per target, patterns pushed root→target.
    let mut checker = IgnoreStack::new();
    preload_logical_parents(store, target, &mut checker, summary);

    let follow = if request.follow_symlinks {
        Follow::Yes
    } else {
        Follow::No
    };
    // Frames `PushAllParents` pushed: the root plus one per target component. Go's
    // `walkFolder` then pushes the target folder again, which `enter_directory` mirrors,
    // so the target frame's own push is balanced by its own pop and only these preloaded
    // frames need popping at the end.
    let preloaded = target.path.depth() + 1;

    // Active resolved-directory identities, current DFS path only (cycle suppression).
    let mut active: Vec<PathBuf> = vec![canonical_identity(store.root())];
    let mut state = WalkState {
        checker: &mut checker,
        active: &mut active,
        summary,
    };

    // Iterative DFS replaces Go's recursion: push on entry, pop on exit.
    let mut frames: Vec<Frame> = Vec::new();
    match enter_directory(
        store,
        &target.path,
        follow,
        true,
        target.library_id,
        &mut state,
    ) {
        EnterOutcome::Entered(frame) => frames.push(frame),
        EnterOutcome::Skipped => {
            pop_frames(state.checker, preloaded);
            return TargetOutcome::Done;
        }
        EnterOutcome::Failed(error) => {
            pop_frames(state.checker, preloaded);
            return TargetOutcome::Failed(error);
        }
    }

    loop {
        if cancel.is_cancelled() {
            pop_frames(state.checker, preloaded);
            return TargetOutcome::Cancelled;
        }
        // Borrow the top frame without holding the borrow across the child push.
        let ready = {
            let frame = frames
                .last_mut()
                .expect("the target frame is present until it is popped");
            if frame.index < frame.entries.len() {
                Some(frame.entries[frame.index].clone())
            } else {
                None
            }
        };

        if let Some(entry) = ready {
            let frame = frames.last_mut().expect("frame present");
            frame.index += 1;
            let name = crate::contract::name_text(entry.path.file_name().expect("entry name"));
            let Some(entry_text) =
                representable_name(frame, &entry, name, target.library_id, state.summary)
            else {
                continue;
            };

            // `.ndignore` matches the library-relative logical path
            // (`scanner/walk_dir_tree.go:136-140`); patterns are never rebased per folder.
            if state.checker.should_ignore(&logical_to_string(&entry.path)) {
                continue;
            }

            let is_dir = is_dir_or_symlink_to_dir(&entry, request.follow_symlinks);
            if is_dir
                && crate::contract::is_ignored_entry(&entry_text, true, request.ignore_dot_folders)
            {
                continue;
            }

            if is_dir {
                match enter_directory(
                    store,
                    &entry.path,
                    follow,
                    false,
                    target.library_id,
                    &mut state,
                ) {
                    EnterOutcome::Entered(child) => {
                        frames.last_mut().expect("parent frame").num_sub_folders += 1;
                        frames.push(child);
                    }
                    EnterOutcome::Skipped => {
                        // Go falls through to the file branch for an unreadable directory
                        // and does not count it as a sub-folder.
                        accumulate_entry(
                            frames.last_mut().expect("frame present"),
                            &entry,
                            &entry_text,
                            store,
                            request.follow_symlinks,
                            target.library_id,
                            state.summary,
                        );
                    }
                    EnterOutcome::Failed(error) => {
                        pop_frames(state.checker, preloaded);
                        return TargetOutcome::Failed(error);
                    }
                }
                continue;
            }

            if crate::contract::is_ignored_entry(&entry_text, false, request.ignore_dot_folders) {
                continue;
            }
            accumulate_entry(
                frames.last_mut().expect("frame present"),
                &entry,
                &entry_text,
                store,
                request.follow_symlinks,
                target.library_id,
                state.summary,
            );
            continue;
        }

        // Postorder: every child has been emitted, so emit this folder then pop it.
        let finished = frames.pop().expect("frame present");
        state.checker.pop();
        state.active.pop();
        match build_event(&finished, target.library_id) {
            EventOutcome::Emitted(event) => {
                match send_event(tx, event, cancel, state.summary, || {}) {
                    SendOutcome::Sent => {}
                    SendOutcome::Cancelled | SendOutcome::Closed => {
                        pop_frames(state.checker, preloaded);
                        return TargetOutcome::Cancelled;
                    }
                }
            }
            EventOutcome::Incomplete(warning) => {
                state.summary.folders_incomplete += 1;
                state.summary.warnings.push(warning);
            }
            EventOutcome::Failed(error) => {
                pop_frames(state.checker, preloaded);
                return TargetOutcome::Failed(error);
            }
        }

        if frames.is_empty() {
            break;
        }
    }

    pop_frames(state.checker, preloaded);
    TargetOutcome::Done
}

/// Borrowed component names as `&[&OsStr]` for the ignore loader.
fn os_refs(components: &[OsString]) -> Vec<&OsStr> {
    components.iter().map(OsString::as_os_str).collect()
}

/// Load root and each logical component prefix, including the target itself.
fn preload_logical_parents(
    store: &LocalDir,
    target: &WalkTarget,
    checker: &mut IgnoreStack,
    summary: &mut WalkSummary,
) {
    let mut prefix = LogicalPath::root();
    push_ignore(store, &prefix, target.library_id, checker, summary);
    for component in target.path.components() {
        prefix = prefix.join(component).expect("validated logical component");
        push_ignore(store, &prefix, target.library_id, checker, summary);
    }
}

fn push_ignore(
    store: &LocalDir,
    dir: &LogicalPath,
    library_id: i64,
    checker: &mut IgnoreStack,
    summary: &mut WalkSummary,
) {
    let outcome = ignore_loader(store)(&os_refs(dir.components()));
    summary
        .warnings
        .extend(outcome.warnings(library_id, dir.clone()));
    checker.push(outcome.patterns());
    for mut warning in checker.take_warnings() {
        warning.library_id = Some(library_id);
        warning.path = Some(dir.clone());
        summary.warnings.push(warning);
    }
}

fn representable_name(
    frame: &mut Frame,
    entry: &EntryFacts,
    name: crate::contract::NameText<'_>,
    library_id: i64,
    summary: &mut WalkSummary,
) -> Option<String> {
    match name {
        crate::contract::NameText::Valid(text) => Some(text.to_string()),
        crate::contract::NameText::Invalid => {
            frame.coverage = FolderCoverage::EntriesIncomplete;
            let warning = ScanWarning {
                kind: ScanWarningKind::NameNotUtf8,
                library_id: Some(library_id),
                path: Some(entry.path.clone()),
                message: format!(
                    "unrepresentable raw path {:?}; excluded from snapshot",
                    entry.path
                ),
            };
            frame.warnings.push(warning.clone());
            summary.warnings.push(warning);
            None
        }
    }
}

fn pop_frames(checker: &mut IgnoreStack, count: usize) {
    for _ in 0..count {
        checker.pop();
    }
}

/// A DFS frame: the captured listing plus the folder state accumulated from it.
struct Frame {
    /// Logical directory this frame enumerates.
    dir: LogicalPath,
    /// Entries sorted by raw name bytes (Go's `fs.ReadDir` order).
    entries: Vec<EntryFacts>,
    /// Next entry to process.
    index: usize,
    /// The directory's own observed stat.
    dir_stat: FileStat,
    /// Running `folderEntry.modTime`: the directory mtime raised to the newest file mtime.
    mod_time: SystemTime,
    /// Running `folderEntry.imagesUpdatedAt`.
    images_updated_at: Option<SystemTime>,
    /// Classified entries.
    entries_classified: Vec<FolderEntry>,
    /// `folderEntry.numSubFolders`.
    num_sub_folders: usize,
    /// Coverage of this folder's enumeration.
    coverage: FolderCoverage,
    /// Warnings attributable to this folder.
    warnings: Vec<ScanWarning>,
}

enum EnterOutcome {
    Entered(Frame),
    /// Exists but must not be descended into (unreadable, cycle, depth limit).
    Skipped,
    Failed(ScanError),
}

/// The mutable state a walk carries across folders, bundled so the entry helper stays
/// small enough to read (and within clippy's argument bound).
struct WalkState<'a> {
    /// `.ndignore` stack, pushed/popped per folder like Go's `IgnoreChecker`.
    checker: &'a mut IgnoreStack,
    /// Active resolved directory identities (current DFS path only).
    active: &'a mut Vec<PathBuf>,
    /// Aggregate counters and warnings.
    summary: &'a mut WalkSummary,
}

/// Enter one directory: push its `.ndignore` patterns, check cycle/depth, capture the
/// listing. `is_walk_root` exempts the target itself from the cycle check (Go starts a
/// fresh recursion there).
fn enter_directory(
    store: &LocalDir,
    dir: &LogicalPath,
    follow: Follow,
    is_walk_root: bool,
    library_id: i64,
    state: &mut WalkState<'_>,
) -> EnterOutcome {
    // `scanner/walk_dir_tree.go:77-79`: patterns for this folder, pushed before it is read. Go loads
    // them from `<folder>/.ndignore` exactly as `Push` does.
    push_ignore(store, dir, library_id, state.checker, state.summary);

    if depth_limit_exceeded(state.active.len(), MAX_WALK_DEPTH) {
        state.checker.pop();
        state.summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::DepthLimitReached,
            library_id: Some(library_id),
            path: Some(dir.clone()),
            message: format!("active depth would exceed {MAX_WALK_DEPTH}; not descended"),
        });
        return EnterOutcome::Skipped;
    }

    let os_path = store.os_path(dir);
    let identity = canonical_identity(&os_path);
    if !is_walk_root && state.active.contains(&identity) {
        state.checker.pop();
        state.summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::DirectoryCycleSkipped,
            library_id: Some(library_id),
            path: Some(dir.clone()),
            message:
                "directory resolves to an active ancestor; not descended (Go would recurse forever)"
                    .to_string(),
        });
        return EnterOutcome::Skipped;
    }

    match store.list_annotated(dir, follow) {
        Ok(listing) => {
            state.active.push(identity);
            let dir_stat = listing.dir_stat.clone();
            // `folderEntry.modTime` starts from `fs.Stat(dirPath).ModTime()`. A platform
            // that reports no mtime leaves it unset; `build_event` then refuses to emit a
            // snapshot rather than guessing a timestamp.
            let mod_time = dir_stat.modified.unwrap_or(SystemTime::UNIX_EPOCH);
            EnterOutcome::Entered(Frame {
                dir: dir.clone(),
                entries: listing.entries,
                index: 0,
                dir_stat,
                mod_time,
                images_updated_at: None,
                entries_classified: Vec::new(),
                num_sub_folders: 0,
                coverage: FolderCoverage::Complete,
                warnings: Vec::new(),
            })
        }
        Err(error) => {
            state.checker.pop();
            // A failed listing is never an empty successful folder.
            state.summary.folders_incomplete += 1;
            match &error {
                StorageError::PermissionDenied { .. } => {
                    state.summary.warnings.push(ScanWarning {
                        kind: ScanWarningKind::DirectoryUnreadable,
                        library_id: Some(library_id),
                        path: Some(dir.clone()),
                        message: format!("directory cannot be opened: {error}"),
                    });
                    EnterOutcome::Skipped
                }
                StorageError::EntryIsSymlink { .. } | StorageError::AncestorIsSymlink { .. } => {
                    state.summary.warnings.push(ScanWarning {
                        kind: ScanWarningKind::SymlinkedTargetSkipped,
                        library_id: Some(library_id),
                        path: Some(dir.clone()),
                        message: format!("symlinked directory refused: {error}"),
                    });
                    EnterOutcome::Skipped
                }
                StorageError::NotFound { .. } | StorageError::NotADirectory { .. } => {
                    state.summary.warnings.push(ScanWarning {
                        kind: ScanWarningKind::DirectoryEnumerationFailed,
                        library_id: Some(library_id),
                        path: Some(dir.clone()),
                        message: format!("directory cannot be listed: {error}"),
                    });
                    EnterOutcome::Skipped
                }
                other => EnterOutcome::Failed(ScanError::RootUnreadable {
                    path: dir.to_string(),
                    source: clone_storage_error(other),
                }),
            }
        }
    }
}

/// `scanner/walk_dir_tree.go:216-233` (`isDirOrSymlinkToDir`): a real directory, or a symlink whose
/// **endpoint** is a directory and following is enabled.
fn is_dir_or_symlink_to_dir(entry: &EntryFacts, follow_symlinks: bool) -> bool {
    if entry.kind == EntryKind::Dir {
        return true;
    }
    entry.kind == EntryKind::Symlink
        && follow_symlinks
        && matches!(
            entry.endpoint.as_ref().map(|endpoint| endpoint.kind),
            Some(EndpointKind::Dir)
        )
}

/// Fold one non-directory entry into its frame, mirroring `loadDir`'s file branch.
fn accumulate_entry(
    frame: &mut Frame,
    entry: &EntryFacts,
    entry_text: &str,
    store: &LocalDir,
    follow_symlinks: bool,
    library_id: i64,
    summary: &mut WalkSummary,
) {
    // Go's isDirOrSymlinkToDir rejects broken followed links before Info()/mtime.
    // Do not postpone all accumulation: disabled links and unclassified files still count.
    if entry.kind == EntryKind::Symlink
        && follow_symlinks
        && !matches!(
            entry.endpoint.as_ref().map(|endpoint| endpoint.kind),
            Some(EndpointKind::File | EndpointKind::Dir | EndpointKind::Other)
        )
    {
        summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::SymlinkSkipped,
            library_id: Some(library_id),
            path: Some(entry.path.clone()),
            message: "broken followed symlink skipped before mtime accumulation".to_string(),
        });
        return;
    }
    // `scanner/walk_dir_tree.go:157-163`: Go raises the folder mtime from `DirEntry.Info()` — the
    // entry's own lstat — *before* deciding whether the entry is classified at all.
    if let Ok(modified) = entry.stat.modified {
        if modified > frame.mod_time {
            frame.mod_time = modified;
        }
    }

    // `resolveEntryName` (`scanner/walk_dir_tree.go:253-298`): classify by the resolved endpoint's
    // base name; a link that cannot resolve is skipped with a defined warning.
    let classification = if entry.kind != EntryKind::Symlink {
        Some((entry_text.to_string(), false))
    } else if !follow_symlinks {
        summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::SymlinkSkipped,
            library_id: Some(library_id),
            path: Some(entry.path.clone()),
            message: "symlink skipped: following is disabled".to_string(),
        });
        None
    } else {
        match store.resolve(&entry.path, Follow::Yes) {
            Ok(ResolvedTarget::Endpoint(endpoint)) => match endpoint.kind {
                EndpointKind::File | EndpointKind::Other => {
                    let Some(base) =
                        endpoint_name(frame, &entry.path, &endpoint.target, library_id, summary)
                    else {
                        return;
                    };
                    Some((base, is_outside_root(store, &endpoint.target)))
                }
                EndpointKind::Dir | EndpointKind::Missing | EndpointKind::Symlink => {
                    summary.warnings.push(ScanWarning {
                        kind: ScanWarningKind::SymlinkSkipped,
                        library_id: Some(library_id),
                        path: Some(entry.path.clone()),
                        message: format!("symlink endpoint is {:?}; not classified", endpoint.kind),
                    });
                    None
                }
            },
            Ok(ResolvedTarget::LinkLoop { .. }) => {
                summary.warnings.push(ScanWarning {
                    kind: ScanWarningKind::SymlinkSkipped,
                    library_id: Some(library_id),
                    path: Some(entry.path.clone()),
                    message: "symlink chain exceeds the resolution bound".to_string(),
                });
                None
            }
            Err(error) => {
                summary.warnings.push(ScanWarning {
                    kind: ScanWarningKind::SymlinkSkipped,
                    library_id: Some(library_id),
                    path: Some(entry.path.clone()),
                    message: format!("symlink cannot be resolved: {error}"),
                });
                None
            }
        }
    };
    let Some((classification, outside_root)) = classification else {
        return;
    };

    if outside_root {
        // Go indexes a followed link whose endpoint lies outside the root when the target
        // extension is audio (`walk_dir_tree_test.go:520`); the logical path stays inside.
        summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::OutsideRootEndpoint,
            library_id: Some(library_id),
            path: Some(entry.path.clone()),
            message: "symlink endpoint lies outside the library root; classified by target"
                .to_string(),
        });
    }

    if entry.stat.modified.is_err() {
        frame.coverage = FolderCoverage::EntriesIncomplete;
        let warning = ScanWarning {
            kind: ScanWarningKind::EntryStatFailed,
            library_id: Some(library_id),
            path: Some(entry.path.clone()),
            message: "entry mtime not reported; excluded from the fingerprint facts".to_string(),
        };
        frame.warnings.push(warning.clone());
        summary.warnings.push(warning);
        return;
    }

    let category = classify(&classification);
    if category == EntryCategory::Image {
        // `folder.imagesUpdatedAt = utils.TimeNewest(imagesUpdatedAt, fileInfo.ModTime(), folder.modTime)`
        let mut times: Vec<SystemTime> = Vec::with_capacity(3);
        times.push(frame.mod_time);
        if let Some(existing) = frame.images_updated_at {
            times.push(existing);
        }
        if let Ok(modified) = entry.stat.modified {
            times.push(modified);
        }
        frame.images_updated_at = newest(&times);
    }

    frame.entries_classified.push(FolderEntry {
        name: entry_text.to_string(),
        path: entry.path.clone(),
        classification_name: classification,
        stat: entry.stat.clone(),
        endpoint: entry
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.stat.clone()),
        outside_root,
        category,
    });
}

/// Classify by extension, reusing nd-mapping rather than duplicating MIME logic.
fn classify(name: &str) -> EntryCategory {
    if nd_mapping::file_types::is_audio_file(name) {
        EntryCategory::Audio
    } else if nd_mapping::file_types::is_valid_playlist(name) {
        EntryCategory::Playlist
    } else if nd_mapping::file_types::is_image_file(name) {
        EntryCategory::Image
    } else {
        EntryCategory::Other
    }
}

/// Whether a resolved endpoint lies outside the library root. The root is trusted
/// configuration, so a symlinked root resolves to itself and nothing counts as outside.
fn is_outside_root(store: &LocalDir, target: &Path) -> bool {
    let root = canonical_identity(store.root());
    let endpoint = canonical_identity(target);
    root != endpoint && !endpoint.starts_with(&root)
}

/// Canonicalized directory identity, used only for the active-ancestor cycle check.
/// A failed `canonicalize` keeps the raw path, so the check stays best-effort rather than
/// silently reporting "no cycle".
fn canonical_identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `scanner/walk_dir_tree.go:237-245` (`isSymlinkedPath`): the path or any ancestor is a link. The
/// configured root component is not examined (trusted configuration).
fn is_symlinked_path(store: &LocalDir, path: &LogicalPath) -> bool {
    let mut current = path.clone();
    while !current.is_root() {
        if let Ok(snapshot) = store.snapshot(&current, Follow::No) {
            if snapshot.is_symlink() {
                return true;
            }
        }
        current = current.parent();
    }
    false
}

enum SendOutcome {
    Sent,
    Cancelled,
    Closed,
}

/// Bounded send with a real wakeup: try, and while the queue is full park on the cancel
/// token (which the consumer wakes after taking a folder). An atomic flag alone could never
/// wake a parked full-queue send.
fn send_blocking(
    tx: &mpsc::Sender<FolderEvent>,
    event: FolderEvent,
    cancel: &CancelToken,
    mut on_full: impl FnMut(),
) -> SendOutcome {
    let mut pending = event;
    loop {
        if cancel.is_cancelled() {
            return SendOutcome::Cancelled;
        }
        match tx.try_send(pending) {
            Ok(()) => return SendOutcome::Sent,
            Err(mpsc::error::TrySendError::Full(event)) => {
                pending = event;
                on_full();
                // Park until cancelled or until the consumer frees queue space.
                if cancel.wait_for_wakeup() {
                    return SendOutcome::Cancelled;
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return SendOutcome::Closed,
        }
    }
}

enum EventOutcome {
    Emitted(FolderEvent),
    /// No snapshot is claimed for this folder; the reason is reported, never implied by
    /// silence.
    Incomplete(ScanWarning),
    Failed(ScanError),
}

/// Build the postorder snapshot and fingerprint from captured facts only — nothing here
/// re-stats the filesystem.
fn build_event(frame: &Frame, library_id: i64) -> EventOutcome {
    if frame.dir_stat.modified.is_err() {
        return EventOutcome::Incomplete(ScanWarning {
            kind: ScanWarningKind::TimestampUnavailable,
            library_id: Some(library_id),
            path: Some(frame.dir.clone()),
            message: "directory mtime not reported; no snapshot emitted".to_string(),
        });
    }
    let mut entries = frame.entries_classified.clone();
    // Go sorts each category's map keys before hashing (`scanner/folder_entry.go:117-123`); the
    // snapshot keeps a single name-sorted list and the hash sorts per category.
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let snapshot = FolderSnapshot {
        library_id,
        path: frame.dir.clone(),
        dir_stat: frame.dir_stat.clone(),
        mod_time: frame.mod_time,
        images_updated_at: frame.images_updated_at,
        num_sub_folders: frame.num_sub_folders,
        entries,
        coverage: frame.coverage.clone(),
        warnings: frame.warnings.clone(),
    };
    match snapshot.hash() {
        Ok(hash) => EventOutcome::Emitted(FolderEvent { snapshot, hash }),
        Err(error) => EventOutcome::Failed(ScanError::HashTime {
            error: clone_folder_error(&error),
        }),
    }
}

/// Slash-separated library-relative text for the ignore matcher. Under the current
/// ordering every component reaching here is representable: raw names are excluded by
/// [`representable_name`] before the matcher is consulted, and a raw *target* component
/// fails the preflight in `run_walk` before anything is opened. So the `expect` below is
/// an unreachable fail-fast on that ordering, not a lossy fallback — the earlier
/// "matcher input only" wording understated it (`S9` item b). If a future caller ever
/// feeds a raw path here, it must be reported as a typed warning rather than panicking.
fn logical_to_string(path: &LogicalPath) -> String {
    path.components()
        .iter()
        .map(|component| {
            component
                .to_str()
                .expect("representable logical path")
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Preflight seam accepts an explicit name classification for portable invalid-name tests.
fn preflight_component(
    target: &WalkTarget,
    component: &OsStr,
    name: crate::contract::NameText<'_>,
    summary: &mut WalkSummary,
) -> Result<(), ScanError> {
    if matches!(name, crate::contract::NameText::Invalid) {
        summary.warnings.push(ScanWarning {
            kind: ScanWarningKind::NameNotUtf8,
            library_id: Some(target.library_id),
            path: Some(target.path.clone()),
            message: format!("non-UTF-8 logical target raw path {:?}", target.path),
        });
        return Err(ScanError::NonUtf8Name {
            name: format!("{component:?}"),
        });
    }
    Ok(())
}

fn endpoint_name(
    frame: &mut Frame,
    logical: &LogicalPath,
    endpoint: &Path,
    library_id: i64,
    summary: &mut WalkSummary,
) -> Option<String> {
    let name = endpoint.file_name().map(crate::contract::name_text);
    endpoint_name_text(frame, logical, endpoint, name, library_id, summary)
}

fn endpoint_name_text(
    frame: &mut Frame,
    logical: &LogicalPath,
    endpoint: &Path,
    name: Option<crate::contract::NameText<'_>>,
    library_id: i64,
    summary: &mut WalkSummary,
) -> Option<String> {
    if let Some(crate::contract::NameText::Valid(text)) = name {
        return Some(text.to_string());
    }
    frame.coverage = FolderCoverage::EntriesIncomplete;
    let warning = ScanWarning {
        kind: ScanWarningKind::NameNotUtf8,
        library_id: Some(library_id),
        path: Some(logical.clone()),
        message: format!("unrepresentable resolved endpoint raw path {endpoint:?}; excluded"),
    };
    frame.warnings.push(warning.clone());
    summary.warnings.push(warning);
    None
}

fn send_event(
    tx: &mpsc::Sender<FolderEvent>,
    event: FolderEvent,
    cancel: &CancelToken,
    summary: &mut WalkSummary,
    on_full: impl FnMut(),
) -> SendOutcome {
    let outcome = send_blocking(tx, event, cancel, on_full);
    if matches!(outcome, SendOutcome::Sent) {
        summary.folders_emitted += 1;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{BirthTimeUnavailable, StatUnavailable};

    fn summary() -> WalkSummary {
        WalkSummary {
            outcome: WalkOutcome::Complete,
            folders_emitted: 0,
            folders_incomplete: 0,
            targets_walked: 0,
            targets_skipped: 0,
            warnings: Vec::new(),
        }
    }

    fn frame() -> Frame {
        Frame {
            dir: LogicalPath::parse("owner").unwrap(),
            entries: Vec::new(),
            index: 0,
            dir_stat: FileStat {
                size: 0,
                modified: Ok(SystemTime::UNIX_EPOCH),
                created: Err(BirthTimeUnavailable),
            },
            mod_time: SystemTime::UNIX_EPOCH,
            images_updated_at: None,
            entries_classified: Vec::new(),
            num_sub_folders: 0,
            coverage: FolderCoverage::Complete,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn invalid_name_seam_marks_the_actual_owner_incomplete_on_every_platform() {
        let mut owner = frame();
        let entry = EntryFacts {
            path: LogicalPath::parse("owner/raw-name").unwrap(),
            kind: EntryKind::File,
            stat: owner.dir_stat.clone(),
            endpoint: None,
        };
        let mut summary = summary();
        assert!(representable_name(
            &mut owner,
            &entry,
            crate::contract::NameText::Invalid,
            42,
            &mut summary,
        )
        .is_none());
        assert_eq!(owner.coverage, FolderCoverage::EntriesIncomplete);
        assert!(owner.entries_classified.is_empty());
        assert_eq!(owner.warnings, summary.warnings);
        assert_eq!(summary.warnings[0].path, Some(entry.path));
        assert_eq!(summary.warnings[0].library_id, Some(42));
    }

    #[tokio::test]
    async fn unavailable_entry_mtime_is_excluded_not_hashed_name_only() {
        let mut owner = frame();
        let entry = EntryFacts {
            path: LogicalPath::parse("owner/song.mp3").unwrap(),
            kind: EntryKind::File,
            stat: FileStat {
                size: 4,
                modified: Err(StatUnavailable),
                created: Err(BirthTimeUnavailable),
            },
            endpoint: None,
        };
        let store = LocalDir::from_path(OsStr::new(".")).unwrap();
        let mut summary = summary();
        accumulate_entry(
            &mut owner,
            &entry,
            "song.mp3",
            &store,
            true,
            42,
            &mut summary,
        );
        assert_eq!(owner.coverage, FolderCoverage::EntriesIncomplete);
        assert!(owner.entries_classified.is_empty());
        assert_eq!(owner.warnings[0].kind, ScanWarningKind::EntryStatFailed);
        assert_eq!(owner.warnings, summary.warnings);
        let (tx, receiver) = mpsc::channel(1);
        drop(tx);
        let (done, terminal) = oneshot::channel();
        done.send(summary).unwrap();
        let terminal_only = WalkHandle {
            receiver,
            terminal: Some(terminal),
            cancel: CancelToken::new(),
            worker: None,
        };
        let result = terminal_only.terminal().await;
        assert_eq!(result.warnings[0].kind, ScanWarningKind::EntryStatFailed);
    }

    #[tokio::test]
    async fn terminal_drains_a_known_buffer_before_joining_and_preserves_worker_failure() {
        let EventOutcome::Emitted(event) = build_event(&frame(), 42) else {
            panic!("fixture must hash");
        };
        let (tx, receiver) = mpsc::channel(1);
        let (done_tx, terminal) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            tx.try_send(event.clone()).expect("first event buffered");
            ready_tx.send(()).unwrap();
            assert!(matches!(
                send_blocking(&tx, event, &worker_cancel, || {}),
                SendOutcome::Sent
            ));
            let mut summary = summary();
            summary.folders_emitted = 2;
            summary.outcome = WalkOutcome::Failed(ScanError::NoLibraries);
            done_tx.send(summary).unwrap();
        });
        ready_rx.await.unwrap(); // buffer is occupied before terminal is called
        let handle = WalkHandle {
            receiver,
            terminal: Some(terminal),
            cancel,
            worker: Some(worker),
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle.terminal())
            .await
            .expect("terminal drains buffered and future events without a sleep");
        assert_eq!(result.folders_emitted, 2);
        assert!(matches!(
            result.outcome,
            WalkOutcome::Failed(ScanError::NoLibraries)
        ));
    }

    #[test]
    fn target_preflight_and_endpoint_invalid_name_seams_are_portable() {
        let target = WalkTarget {
            library_id: 7,
            path: LogicalPath::parse("ancestor/child").unwrap(),
        };
        let mut summary = summary();
        assert!(matches!(
            preflight_component(
                &target,
                OsStr::new("raw"),
                crate::contract::NameText::Invalid,
                &mut summary,
            ),
            Err(ScanError::NonUtf8Name { .. })
        ));
        assert_eq!(summary.warnings[0].path, Some(target.path.clone()));
        assert!(summary.warnings[0].message.contains("ancestor"));
        let mut owner = frame();
        let raw = Path::new("raw/endpoint");
        assert!(endpoint_name_text(
            &mut owner,
            &target.path,
            raw,
            Some(crate::contract::NameText::Invalid),
            7,
            &mut summary,
        )
        .is_none());
        assert_eq!(owner.coverage, FolderCoverage::EntriesIncomplete);
        assert!(owner.entries_classified.is_empty());
        assert!(owner.warnings[0].message.contains(&format!("{raw:?}")));
        assert_eq!(owner.warnings[0], summary.warnings[1]);
    }

    #[test]
    fn an_unsent_full_queue_event_is_not_counted_on_cancel_or_close() {
        for close in [false, true] {
            let EventOutcome::Emitted(event) = build_event(&frame(), 42) else {
                panic!("fixture must hash");
            };
            let (tx, receiver) = mpsc::channel(1);
            let cancel = CancelToken::new();
            let mut totals = summary();
            assert!(matches!(
                send_event(&tx, event.clone(), &cancel, &mut totals, || {}),
                SendOutcome::Sent
            ));
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let worker_cancel = cancel.clone();
            let worker = std::thread::spawn(move || {
                // Signal only after the second event has actually encountered Full.
                let mut ready = Some(ready_tx);
                let result = send_event(&tx, event, &worker_cancel, &mut totals, || {
                    if let Some(ready) = ready.take() {
                        ready.send(()).unwrap();
                    }
                });
                let _ = result_tx.send((result, totals));
            });
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            if close {
                drop(receiver);
            } else {
                cancel.cancel();
            }
            let (outcome, totals) = result_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("full-queue producer must terminate");
            worker.join().unwrap();
            if close {
                assert!(matches!(outcome, SendOutcome::Closed));
            } else {
                assert!(matches!(outcome, SendOutcome::Cancelled));
            }
            assert_eq!(totals.folders_emitted, 1);
        }
    }

    #[test]
    fn depth_gate_boundary_is_exact_and_keeps_the_production_cap() {
        // The seam takes an explicit limit, so the boundary is exercised without weakening
        // `MAX_WALK_DEPTH`: the first refused directory is the one whose entry would make
        // the walk deeper than the cap, and the one before it still enters.
        assert!(!depth_limit_exceeded(0, 2));
        assert!(!depth_limit_exceeded(1, 2));
        assert!(depth_limit_exceeded(2, 2));
        assert!(depth_limit_exceeded(3, 2));

        assert!(!depth_limit_exceeded(MAX_WALK_DEPTH - 1, MAX_WALK_DEPTH));
        assert!(depth_limit_exceeded(MAX_WALK_DEPTH, MAX_WALK_DEPTH));
        assert!(depth_limit_exceeded(MAX_WALK_DEPTH + 1, MAX_WALK_DEPTH));
        // `enter_directory` checks the *live* active length before pushing, and `active`
        // is seeded with the root identity plus one push per entered directory — so the
        // first refused directory sits at logical depth `MAX_WALK_DEPTH - 1` (asserted
        // end-to-end in `tests/walk_policy.rs`).
        assert_eq!(
            MAX_WALK_DEPTH, 255,
            "the cap itself is unchanged by this seam"
        );
    }

    #[test]
    fn a_frame_without_a_directory_mtime_refuses_the_snapshot_with_a_typed_warning() {
        // `build_event` is the walk-level seam for `TimestampUnavailable`: a directory whose
        // own mtime is not reported gets no snapshot and a named reason. On this platform
        // `Metadata::modified()` cannot fail for a real directory, so this seam is the
        // reachable production path and `tests/walk_policy.rs` states that limit.
        let mut owner = frame();
        owner.dir_stat.modified = Err(StatUnavailable);
        let EventOutcome::Incomplete(warning) = build_event(&owner, 42) else {
            panic!("a folder with no directory mtime must not claim a snapshot");
        };
        assert_eq!(warning.kind, ScanWarningKind::TimestampUnavailable);
        assert_eq!(warning.path, Some(owner.dir.clone()));
        assert_eq!(warning.library_id, Some(42));
        assert!(
            owner.entries_classified.is_empty(),
            "no fingerprint facts are gathered for an unstamped folder"
        );
    }

    #[tokio::test]
    async fn a_directory_timestamp_refusal_reaches_the_terminal_summary() {
        let mut owner = frame();
        owner.dir_stat.modified = Err(StatUnavailable);
        let EventOutcome::Incomplete(warning) = build_event(&owner, 42) else {
            panic!("fixture must refuse the snapshot");
        };
        let mut totals = summary();
        // Mirrors `walk_target`'s `EventOutcome::Incomplete` arm: counted as incomplete and
        // pushed into the terminal collection, never dropped together with the frame.
        totals.folders_incomplete += 1;
        totals.warnings.push(warning);
        let (tx, receiver) = mpsc::channel(1);
        drop(tx);
        let (done, terminal) = oneshot::channel();
        done.send(totals).unwrap();
        let handle = WalkHandle {
            receiver,
            terminal: Some(terminal),
            cancel: CancelToken::new(),
            worker: None,
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle.terminal())
            .await
            .expect("terminal must join without a sleep");
        assert_eq!(result.folders_incomplete, 1);
        assert_eq!(result.folders_emitted, 0);
        assert_eq!(
            result.warnings[0].kind,
            ScanWarningKind::TimestampUnavailable
        );
    }

    #[tokio::test]
    async fn one_unstatable_entry_still_emits_the_folder_and_reaches_the_summary_once() {
        // Walk-level `EntryStatFailed`: the statable entry survives, the folder is still
        // fingerprinted as entry-incomplete, and the *same* warning object lands in both the
        // snapshot and the terminal summary — not only on the frame.
        let mut owner = frame();
        let store = LocalDir::from_path(OsStr::new(".")).unwrap();
        let statable = EntryFacts {
            path: LogicalPath::parse("owner/song.mp3").unwrap(),
            kind: EntryKind::File,
            stat: FileStat {
                size: 4,
                modified: Ok(SystemTime::UNIX_EPOCH),
                created: Err(BirthTimeUnavailable),
            },
            endpoint: None,
        };
        let unstatable = EntryFacts {
            path: LogicalPath::parse("owner/cover.jpg").unwrap(),
            kind: EntryKind::File,
            stat: FileStat {
                size: 4,
                modified: Err(StatUnavailable),
                created: Err(BirthTimeUnavailable),
            },
            endpoint: None,
        };
        let mut totals = summary();
        accumulate_entry(
            &mut owner,
            &statable,
            "song.mp3",
            &store,
            true,
            42,
            &mut totals,
        );
        accumulate_entry(
            &mut owner,
            &unstatable,
            "cover.jpg",
            &store,
            true,
            42,
            &mut totals,
        );
        assert_eq!(totals.warnings.len(), 1, "one entry, one stat failure");
        assert_eq!(totals.warnings[0].kind, ScanWarningKind::EntryStatFailed);
        assert_eq!(totals.warnings[0].path, Some(unstatable.path.clone()));
        assert_eq!(owner.coverage, FolderCoverage::EntriesIncomplete);
        assert_eq!(
            owner
                .entries_classified
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["song.mp3"],
            "the statable entry still contributes"
        );

        let EventOutcome::Emitted(event) = build_event(&owner, 42) else {
            panic!("a folder with one unstatable entry is still fingerprinted");
        };
        assert_eq!(event.snapshot.coverage, FolderCoverage::EntriesIncomplete);
        assert_eq!(event.snapshot.warnings, totals.warnings);

        let (tx, receiver) = mpsc::channel(1);
        drop(tx);
        let (done, terminal) = oneshot::channel();
        done.send(totals).unwrap();
        let handle = WalkHandle {
            receiver,
            terminal: Some(terminal),
            cancel: CancelToken::new(),
            worker: None,
        };
        let result = handle.terminal().await;
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].kind, ScanWarningKind::EntryStatFailed);
        assert_eq!(result.warnings[0].path, Some(unstatable.path));
    }
}
