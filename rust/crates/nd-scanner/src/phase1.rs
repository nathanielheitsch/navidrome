//! SC-05 — the phase-1 ingest vertical for ONE folder: children read → walk →
//! tag-read → SC-03 bridge/projection → [`FolderWriteBatch`] → DB-14 persist.
//!
//! This is `scanner/phase_1_folders.go`'s `processFolder` (`:224-286`) plus
//! `loadTagsFromFiles` (`:292-325`), `createAlbumsFromMediaFiles` (`:328-337`) and
//! `createArtistsFromMediaFiles` (`:340-346`), and the batch-*construction* half of
//! `persistFolder` (`:374-475`). The transaction itself is DB-14's
//! [`nd_db::scanner_writes::persist_folder_delta`], which reproduces Go's
//! `WithTxRetry` and write order; this lane builds the immutable batch and consumes
//! the committed outcome. Only the seams ratified NARROW in
//! `docs/rust-port/decisions.md` ("SC-01 seams ratification") are consumed: SC-02
//! storage facts, DB-11 `visit_media_in_folder`, DB-14 `persist_folder_delta`,
//! [`CancelToken`] (adapted here), and the nd-tagreader → nd-mapping → `to_media_file`
//! bridge through SC-03.
//!
//! ## Deliberately NOT here (later SC-06 lanes own each of these)
//!
//! * **Global `finalize` unvisited->missing pass** (`:520-547`): [`scan_library`] removes
//!   every visited folder from its `last_updates` map exactly as Go's `popLastUpdate`
//!   (`:113-120`) does, so the residue is reported as
//!   [`LibraryScanOutcome::folders_unvisited`] — but nothing is marked missing. The
//!   inaccessible-subtree-vs-deleted policy is a still-open parent decision
//!   (`decisions.md` D-SC06-FINALIZE): a finalizer here would delete rows on an
//!   operator-visible I/O failure, and `persist_folder_delta_in_tx` stays `pub(crate)`
//!   (D-SC06-TX), so no Go-shaped global transaction is reachable from this crate anyway.
//! * **Image-state diff and the artwork legs** (`folder_entry.go:81-92`, `:363-369`,
//!   `:437-441`, `:462-473`): `prevImageFiles`/`prevImagesUpdatedAt` from
//!   `get_folder_update_info` are read but not compared, and `EnqueueIfMissing` is DB-14's
//!   own full-scan arm, not new scanner behavior. `image_changes.rs` is not this slice's.
//! * **Selective-target scoping, overlap, `markFailed`, PID-rescan->full promotion,
//!   progress sink, `ScanBegin`/`ScanEnd`, multi-library orchestration** (`scanner.go:50,
//!   `:78-105`; `phase_1_folders.go:85-90`, `:172-177`). [`ScanScope::Targets`] takes the
//!   target list straight to Go's `GetFolderUpdateInfo(ctx, lib, targetFolders...)` and
//!   the walker's own target list; a rejected target is a skipped target, not a failed
//!   library (D-SC05-SLICE item 1).
//!
//! Wired by [`scan_library`] now: [`FolderJob::prev_album_pid`] is Go's
//! `scanJob.prevAlbumPIDConf` (`scanner.go:85-87`, set when `lib.ScannedPIDAlbum !=
//! pid.Album`) and drives the third `prevAlbumID` arm (`:313-315`). [`FolderJob`] takes it
//! as `Option<&str>` — `None` is Go's empty string, which disables the arm — and every
//! test either leaves it `None` or supplies it explicitly. Likewise
//! [`FolderJob::no_birth_time`] is Go's per-device `localFS.noBirthTime`
//! (`core/storage/local/local.go:129-145`), which SC-06's storage layer will set from its
//! own device memory rather than from a per-folder flag.
//! * **Multi-track album aggregation**: SC-04 landed, so
//!   [`projection::album_drafts`] now runs Go's real `MediaFiles.ToAlbum` loop
//!   (`crate::aggregate`) over each album group — `SongCount`, the duration/size sums,
//!   date consensus, `minMax` years, tag frequency and the lowest-disc artwork pick are
//!   aggregated per group instead of refused. A group that cannot be aggregated (mixed
//!   album/library, an overflowing sum, or a member whose storage facts are missing) is
//!   still refused through [`Phase1Error::Projection`], so nothing half-computed is
//!   written.
//! * **Post-commit work**: `imageChanges.record` (`:363-369`), progress events, the
//!   unvisited-folder missing finalizer (`:520-547`), and phase-3 `refresh_*`
//!   triggers are all absent. Nothing runs after the commit but the returned outcome.
//!
//! ## What SC-06's narrow slice added (D-SC06-SLICE)
//!
//! [`scan_library`] is the multi-folder entry. It drives the SC-02 postorder walker
//! ([`crate::walk::WalkHandle`]), applies Go's change detection per folder
//! (`isOutdated`/`isNew` plus the skip-new-empty-folder arm, `:186-201`), runs Go's import
//! selection inside `ingest_folder_snapshot` (`:247-262` — not found in the DB, full scan
//! in progress, `mtime > updated_at`, or the stored child is `missing`), feeds re-imported
//! known tracks through the same `albumIDMap` transitions (`:309-321`, already ported by
//! `album_transitions`), and persists each folder UNCHANGED through the public
//! `persist_folder_delta` (DB-14, `scanner_writes.rs:627`) with missing-marking and `Touch`
//! inside that same folder tx (`:446-456`). A persist failure stops the walk through
//! [`crate::walk::WalkHandle::cancel`] exactly as Go's `stopWalk(err)` (`:358`) does, so
//! folders the walk never reached keep their stored rows and are reported unvisited.
//!
//! ## Deliberate deviations from Go in this slice
//!
//! * **One consumer instead of `DevScannerThreads` stage instances** (`:216`): Go runs
//!   `processFolder` with several concurrent stages and `persistChanges` with exactly one
//!   (`:219`). Sequentially consuming the postorder stream reproduces the observable
//!   behavior — the persist order is the walk order either way, and Go's own pipeline drops
//!   a stage error without emitting the item, so a folder-level abort cannot interleave with
//!   a persist.
//! * **The quick-scan filter's `Info()` failure trigger.** Go's arm fires when
//!   `af.Info()` — the `DirEntry`'s cached lstat facts — cannot be read. The port captures
//!   those facts once into [`FolderEntry::stat`], so the faithful trigger is a captured
//!   entry whose mtime is absent (`Err(BirthTimeUnavailable)`-shaped absence), never a
//!   re-stat. Both arms' warning texts are pinned by the in-crate tests so a future edit
//!   cannot route one arm's wording through the other formatter.
//! * **A NULL `media_file.updated_at` reads as Go's zero time.** Go's `MediaFile.UpdatedAt`
//!   is a non-pointer `time.Time`, so a NULL cell fails its row scan before the filter ever
//!   runs; the port's record types it as `Option<DateTime<Utc>>`, and the filter treats
//!   `None` as the year-one instant, i.e. "the file is always newer", which is what an unset
//!   Go timestamp would mean. Documented at the comparison site.
//! * **`prev_album_pid` is `Option<String>`, never `Some("")`.** Go's `prevAlbumPIDConf`
//!   is an empty string when the scanned spec is empty and its consuming arm is guarded by
//!   `!= ""` (`:313`); `from_library_row` filters the empty spec out so the re-derivation
//!   arm cannot be enabled with an empty spec.
//! * **`ScanScope::Targets` skips the target-overlap and `markFailed` behavior**
//!   (`scanner.go:78-105`, `:40-45`): targets go straight to Go's
//!   `GetFolderUpdateInfo(ctx, lib, targetFolders...)` shape and the walker's target list,
//!   a rejected target is reported as a skipped target with the walker's warning, and no
//!   library is marked failed. Deferred per D-SC06-SLICE.
//!
//! ## Capture and ingest are two halves (D-SC05-SLICE item 4)
//!
//! `capture_selected_folder` (crate-private) is the filesystem half: it preflights the selected
//! target, captures ONE folder's listing into a [`FolderSnapshot`], and fingerprints it — no DB
//! work at all. `ingest_folder_snapshot` (crate-private) is the persistence half and takes that
//! snapshot as trusted input, so a walker can hand it its own authoritative captured
//! folder instead of making this lane rescan. Both are crate-private: [`scan_folder`] is the
//! single-folder public entry (capture then ingest), and [`scan_library`] is SC-06's
//! multi-folder one — it hands the walker's own captured snapshot to the same ingest core,
//! never rescanning a tree that may have changed in between. No controller or
//! multi-library orchestration lives here.
//!
//! The selected folder is a **fresh walker root**: its active identity context is the
//! canonicalized configured library root plus the canonicalized target
//! (`crate::walk::selected_identity_context`), never its logical ancestors, and
//! `crate::walk::enter_refusal` is the shared gate for its child directories — the same
//! cycle and [`crate::walk::MAX_WALK_DEPTH`] rules the SC-02 walker applies, so the two
//! paths cannot disagree about which child directories are entered (a refused child still
//! falls through to the same mtime-plus-classification accumulation the walker does).
//!
//! ## Go's abort policy, ported arm by arm
//!
//! * Children-read failure (`:234-243`): `log.Error` + `return entry, err`. Go's
//!   pipeline drops a stage's error without emitting the item
//!   (`go-pipeline/pkg/pipeline/pipeline.go:524-537`: `out, err = s.fn(in)`;
//!   `if err == nil && s.emitToOutCh`), so `persistChanges` never sees that folder and
//!   it writes nothing. `stopWalk` is NOT called — only `persistChanges`'s own failure
//!   calls it (`:358`) — so the surviving stage instances keep processing other folders,
//!   while `eg.Wait` surfaces the first error as a failed phase
//!   (`pipeline.go:540-548, 682-689`). Rust: [`Phase1Error::ChildrenRead`], with SC-06's
//!   controller owning the fail-the-library decision.
//! * File-stat failure (`:254-259`): `af.Info()` failing on a child that WAS found in
//!   the DB logs `Scanner: Error getting file info`, calls `sendWarning` with
//!   `"Error getting file info for %s/%s: %v"`, and returns **`entry, nil`** BEFORE
//!   `entry.missingTracks` is computed (`:270`) — so the folder is persisted with no
//!   tracks, no albums, no artists, and no missing-marking. The same shape appears one
//!   layer down: `loadTagsFromFiles` returns the `fs.Stat` fallback failure
//!   (`core/storage/local/local.go:97-103`) and `:274-279` warn + `return entry, nil`
//!   with `sendWarning`'s **folder-level** text,
//!   `"Error loading tags from files in %s: %v"` — whose `%s` is the FOLDER, not the
//!   file. That is the text this lane's [`TagFailureKind::FolderImportAborted`] carries
//!   ([`Phase1FolderOutcome::tag_failures`]), and the folder row is still committed. It
//!   is NOT the quick-scan filter's per-file text above: the ingest always imports every
//!   file (the full-scan arm), so a stat failure always travels the second site, and the
//!   first site belongs to the quick-scan filter SC-06 owns. The file whose stat failed
//!   is kept on [`TagFailure::path`] — Go's raw error names it even though the warning
//!   text does not — and the raw [`StorageError`] is preserved rather than pre-stringified.
//! * Per-file tag failure: the pinned extractor
//!   (`adapters/gotaglib/gotaglib.go:34-43`) `continue`s on a parse error and returns
//!   `nil`, so the file is **omitted from the batch** and the folder survives. Rust:
//!   [`TagFailureKind::PerFileSkipped`]. This is why one corrupt file cannot reach
//!   Go's folder-level arm: `ReadTags` only errors on the `fs.Stat` fallback
//!   (`core/storage/local/local.go:97-103`) or a whole-batch extractor failure.
//! * Persist failure (`:356-360`): fatal, calls `stopWalk(err)`, phase fails, and
//!   `finalize` (`:524-526`) returns early so unvisited folders are never marked
//!   missing. Rust: [`Phase1Error::Persist`]; the stop-the-walk decision belongs to
//!   SC-06's controller.
//! * Cancellation: checked at the top of `processFolder` (`:226-228`) and again
//!   inside the enumeration loop (`:141`). Rust: [`Phase1Error::Cancelled`] before the
//!   DB read, mid-listing, before persist, and per-row inside the children visitor.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use nd_db::album_repo::AlbumRecord;
use nd_db::artist_repo::ArtistRecord;
use nd_db::folder_repo::{FolderError, FolderRepo, FolderUpdateInfo};
use nd_db::library_repo::{LibraryError, LibraryRepo};
use nd_db::mediafile_repo::MediaFileRecord;
use nd_db::scanner_queries::{self, Control, CursorError, VisitOutcome};
use nd_db::scanner_writes::{
    persist_folder_delta, AlbumTransition, CommittedFolderOutcome, FolderRunError, FolderStamps,
    FolderWriteBatch, MissingMedia,
};
use nd_db::store::{SQLStore, StoreError};
use nd_db::tag_repo::Tag;
use nd_mapping::config::{MappingContext, PidOptions};
use nd_mapping::mdfile::MediaFile;
use nd_mapping::metadata::Metadata;
use nd_utils::context::Cancellation;

use crate::aggregate::{AggregateError, TrackFacts};
use crate::contract::{
    name_text, CancelToken, LibraryRoot, ScanError, ScanWarning, ScanWarningKind, WalkRequest,
    WalkTarget,
};
use crate::folder::{newest, FolderCoverage, FolderEntry, FolderSnapshot, FolderSnapshotError};
use crate::ignore::{ignore_file_name, IgnoreFileOutcome, IgnoreStack};
use crate::mapping_bridge::{self, BridgeError, TrackStat};
use crate::projection::{self, ProjectionError};
use crate::storage::{
    EndpointKind, EntryKind, FileStat, Follow, LocalDir, LogicalPath, ResolvedTarget, StorageError,
};
use crate::walk::{WalkHandle, WalkOutcome};

/// `conf.Server.Subsonic.AppendSubtitle` as it reaches `media_file.full_text`
/// (`model/mediafile.go:110` → `sql_feeders.full_title`). Go reads it from global
/// config; this workspace injects configuration, so the caller supplies it. The
/// Go default is `true` (`conf/configuration.go:1074`).
pub const DEFAULT_APPEND_SUBTITLE: bool = true;

/// What one folder ingest needs, everything injected: no clock read, no global
/// config, no DB-constructed scope.
///
/// `folder_path` is library-relative and logical (slash-separated): the same string
/// Go hands `model.NewFolder`/`model.FolderID` and the same value stored in
/// `folder.path`+`folder.name`. OS paths are derived from `library_root` +
/// `folder_path` for I/O only, never for ids or PIDs.
#[derive(Debug)]
pub struct FolderJob<'a> {
    /// `library.id`, the scope every batch leg carries.
    pub library_id: i64,
    /// The configured library root (`lib.Path`): trusted configuration.
    pub library_root: &'a OsStr,
    /// Library-relative logical folder path; [`LogicalPath::root`] is the library root.
    pub folder_path: LogicalPath,
    /// One caller-chosen instant for the whole scan. Go samples `time.Now()` per file
    /// inside the mapping wrapper (`core/storage/local/local.go:117-128`); SC-03's
    /// rustdoc records a single scan-start stamp as Go-parity. The same instant is the
    /// birth-time substitute and every [`FolderStamps`] value, so one instant names
    /// every stamp the folder writes.
    pub scan_stamp: DateTime<Utc>,
    /// `entry.job.lib.FullScanInProgress` (`:467`), read from the library row.
    pub full_scan_mode: bool,
    /// See [`DEFAULT_APPEND_SUBTITLE`].
    pub append_subtitle: bool,
    /// `conf.Server.Scanner.FollowSymlinks` (default `true`, `conf/configuration.go:1071`).
    pub follow_symlinks: bool,
    /// `conf.Server.Scanner.IgnoreDotFolders` (default `true`, `conf/configuration.go:1072`).
    pub ignore_dot_folders: bool,
    /// Mapping context (options + table), built once per scan run like Go's
    /// `sync.OnceValues` load.
    pub mapping: &'a MappingContext,
    /// The library's PID overrides; empty values resolve to the Go defaults through
    /// [`PidOptions::effective`] (`model/library.go:47-52`).
    pub pid: PidOptions,
    /// `scanJob.prevAlbumPIDConf` (`scanner.go:95`, set at `:85-87` when
    /// `lib.ScannedPIDAlbum != pid.Album`): the album PID spec of the last finished scan,
    /// only when it differs from the current one. `None` is Go's empty string, which
    /// disables the re-derivation arm of `entry.albumIDMap` (`:313-315`).
    pub prev_album_pid: Option<&'a str>,
    /// Treat this device as reporting **no** birth time, the port of `localFS.noBirthTime`
    /// (`core/storage/local/local.go:129-145`): Go remembers per device which filesystems
    /// never give one, so a scan asks each only once. Set on platforms/filesystems that
    /// genuinely lack a creation timestamp (many Linux filesystems); on macOS/APFS leave it
    /// `false`. When set, the record's `birth_time` and the album draft's `created_at`
    /// become the injected [`FolderJob::scan_stamp`] — Go's `BirthTime()` falls back to
    /// `time.Now()` (`local.go:120-128`) and SC-03's ruling makes that substitution the
    /// caller's injected clock instead of a wall-clock read.
    pub no_birth_time: bool,
}

impl FolderJob<'_> {
    /// The folder row id: `model.FolderID(lib, path)` (`model/folder.go:46-52`),
    /// ported as [`nd_db::folder_repo::folder_id`]. The library-relative logical path
    /// means Go's `TrimPrefix(path, lib.Path)` is a no-op, so `lib_path` is empty.
    #[must_use]
    pub fn folder_id(&self) -> String {
        nd_db::folder_repo::folder_id(self.library_id, "", &display_path(&self.folder_path))
    }

    /// The parent folder id: Go's `NewFolder` splits the path and hashes the cleaned
    /// directory (`model/folder.go:54-75`). The library root has no parent row.
    #[must_use]
    pub fn parent_id(&self) -> String {
        if self.folder_path.is_root() {
            return String::new();
        }
        nd_db::folder_repo::folder_id(
            self.library_id,
            "",
            &display_path(&self.folder_path.parent()),
        )
    }
}

/// Why a folder ingest stopped, each variant naming the Go arm it ports so a caller
/// branches on the abort policy rather than on message text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Phase1Error {
    /// Cancellation was requested before the DB read, mid-listing, per-row in the
    /// children visitor, or before persist (`:226-228`, `:141`). No write happened
    /// and no DB transaction was opened by the ingest.
    #[error("scan cancelled for folder '{folder}'")]
    Cancelled {
        /// Logical folder path.
        folder: String,
    },
    /// The folder's directory could not be opened or listed. Go's `loadDir` returns
    /// the error to `walkDirTree`, which logs it and stops that library's walk
    /// (`walk_dir_tree.go:111-127`, `:58-66`); the folder is never persisted.
    #[error("cannot read folder '{folder}' from the filesystem: {source}")]
    Walk {
        /// Logical folder path.
        folder: String,
        /// Storage-level cause.
        #[source]
        source: StorageError,
    },
    /// The selected target was rejected by policy **before** any `.ndignore` read, any
    /// listing, and any DB work. Mirrors the two skip arms of Go's `walkDirTree`
    /// (`scanner/walk_dir_tree.go:39-51`): a missing target, and a target that is a
    /// symlink or is reached through one while following is disabled
    /// (`scanner/walk_dir_tree.go:47-51`, ported by `walk.rs::is_symlinked_path`). Go
    /// counts those as `targetsSkipped` + a warning rather than an error, so a caller —
    /// SC-06's controller, per `decisions.md` D-SC05-SLICE item 1 — treats this as a
    /// **skipped target**, not a fatal library failure. [`Phase1Error::TargetRejected::warning`]
    /// carries the walker's own typed warning for the same rule.
    #[error("selected folder '{folder}' is not scannable ({rejection})")]
    TargetRejected {
        /// Logical folder path.
        folder: String,
        /// Which preflight rule rejected it.
        rejection: TargetRejection,
        /// The warning the SC-02 walker emits for the same rule.
        warning: ScanWarning,
    },
    /// The captured snapshot does not describe the folder the job asked for. This is a
    /// caller/plumbing defect, not a Go arm: `ingest_folder_snapshot` trusts its
    /// snapshot, so library id and logical path are checked against the job BEFORE any DB
    /// work and a mismatch aborts with no writes rather than persisting a folder row under
    /// the wrong id.
    #[error(
        "captured snapshot does not belong to folder '{folder}' (library {expected_library_id} \
         path '{expected_path}', got library {captured_library_id} path '{captured_path}')"
    )]
    SnapshotMismatch {
        /// Logical folder path the job asked for.
        folder: String,
        /// Job's `library_id`.
        expected_library_id: i64,
        /// Snapshot's `library_id`.
        captured_library_id: i64,
        /// Job's logical folder path.
        expected_path: String,
        /// Snapshot's logical folder path.
        captured_path: String,
    },
    /// The snapshot's fingerprint could not be computed. Go's `fmt.Fprintf` cannot
    /// fail; here an unrenderable timestamp or an oversized size stops the hash
    /// rather than being hashed over.
    #[error("cannot fingerprint folder '{folder}': {source}")]
    Hash {
        /// Logical folder path.
        folder: String,
        /// The fingerprint input that cannot be rendered.
        #[source]
        source: FolderSnapshotError,
    },
    /// The children read failed (`:234-243`). This aborts THIS folder's ingest only:
    /// nothing is written for it and other folders remain scannable. The pool is not
    /// poisoned — a statement or decode failure releases its lease.
    #[error("Scanner: Error loading mediafiles from DB, folder '{folder}': {source}")]
    ChildrenRead {
        /// Logical folder path.
        folder: String,
        /// Cursor failure, or the pool checkout that preceded it.
        #[source]
        source: ChildrenReadSource,
    },
    /// A walked entry's own `Info()` reported no mtime (`walk_dir_tree.go:157-161`): Go
    /// logs `Scanner: Error getting fileInfo` and returns the error from `loadDir`, so
    /// the folder is not emitted and never persisted. Go's `FileInfo.ModTime()` cannot
    /// fail; this platform can report no timestamp at all, and the absence aborts the
    /// folder rather than being hashed as a fabricated epoch. This covers every entry the
    /// accumulation reaches — a file, an unclassified entry, AND a directory that cannot be
    /// opened, since Go's `else` branch `Info()`s all of them. A **broken followed symlink**
    /// never reaches this arm: `isDirOrSymlinkToDir` errors on it and `loadDir` skips the entry
    /// before `Info()` (`:144-150`), so its link mtime is neither folded in nor aborted here.
    #[error("cannot read file info for '{path}' in folder '{folder}': {source}")]
    EntryInfo {
        /// Logical folder path.
        folder: String,
        /// Logical path of the entry whose stat failed.
        path: String,
        /// The platform's timestamp absence.
        #[source]
        source: crate::storage::StatUnavailable,
    },
    /// A record could not be projected (`mapping_bridge`). Go has no equivalent arm
    /// (TagLib cannot produce an out-of-`int` value), so this is a declared refusal,
    /// not a parity translation: the record is not written with a fabricated field.
    #[error("cannot project track '{path}' into a record: {source}")]
    Bridge {
        /// Logical path of the offending file.
        path: String,
        /// Bridge-level cause.
        #[source]
        source: BridgeError,
    },
    /// A projection refusal (`crate::aggregate::AggregateError` or the bridge): a mixed
    /// album/library group, a sum that does not fit the record's column, a member with no
    /// supplied storage facts, or an invalid participant role. Go aggregates whatever its
    /// grouping produced, so this arm is a declared refusal of a defective input, not a
    /// parity translation — nothing is written for the folder.
    #[error("cannot project folder '{folder}' into album/artist drafts: {source}")]
    Projection {
        /// Logical folder path.
        folder: String,
        /// Projection-level cause.
        #[source]
        source: ProjectionError,
    },
    /// The folder transaction failed (`:356-360`): Go logs it, calls `stopWalk(err)`,
    /// and fails the phase, so `finalize` (`:524-526`) skips its missing-folder
    /// mutation. Nothing is committed and no warning is published for this folder.
    #[error("Scanner: Error persisting changes to DB, folder '{folder}': {source}")]
    Persist {
        /// Logical folder path.
        folder: String,
        /// DB-14 run failure (batch rejection, SQLite, busy exhaustion, rollback, or
        /// pool checkout).
        #[source]
        source: FolderRunError,
    },
}

/// Which `walkDirTree` preflight arm rejected a selected target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRejection {
    /// The target does not exist (`scanner/walk_dir_tree.go:39-45`): Go skips it, so a
    /// watcher event for a deleted folder stays in the job's last-updates.
    Missing,
    /// The target is a symlink or is reached through one while following is disabled
    /// (`:47-51`). The configured library root stays trusted/exempt, as in the walker.
    SymlinkedPath,
}

impl std::fmt::Display for TargetRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetRejection::Missing => f.write_str("target folder does not exist"),
            TargetRejection::SymlinkedPath => {
                f.write_str("target is a symlink or is reached through one; following is disabled")
            }
        }
    }
}

/// Which leg of the children read failed.
///
/// Go has ONE arm for both (`:234-243`: `GetCursor` failure and iteration failure both
/// `log.Error` + `return entry, err`), so both ride the same [`Phase1Error::ChildrenRead`]
/// variant. Keeping them distinct is what lets a caller tell "cannot lease a
/// connection" from "a stored row cannot be decoded" — Go's driver makes the same
/// distinction inside its error, and the port keeps it typed rather than stringified.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChildrenReadSource {
    /// Statement, bind, execution, or `PostScan` decode failure inside the visitor.
    #[error(transparent)]
    Cursor(#[from] CursorError),
    /// Pool checkout failure, before any statement was prepared.
    #[error("cannot lease a pooled connection for the children read: {0}")]
    Pool(#[source] StoreError),
}

/// Which tag-read arm degraded, in Go's own distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagFailureKind {
    /// The extractor's per-file `continue` (`adapters/gotaglib/gotaglib.go:38-40`):
    /// this file is omitted from the batch; the folder's ingest continues.
    PerFileSkipped,
    /// `loadTagsFromFiles`' batch failure surfaced at `processFolder` (`:274-279`):
    /// Go warns and returns `entry, nil`, so the folder row is still persisted and
    /// NO tracks, albums, or artists are imported for it. `message` is Go's
    /// folder-level `sendWarning` text (`:277`), which names the FOLDER; the file whose
    /// stat failed is carried on [`TagFailure::path`].
    FolderImportAborted,
    /// The quick-scan selection loop's own stat failure (:254-259): Go warns with the
    /// PER-FILE text (:259, the 'Error getting file info for %s/%s: %v' spelling), calls
    /// sendWarning, and returns entry, nil BEFORE entry.missingTracks is computed (:270).
    /// The folder row is therefore persisted with no tracks, no albums, no artists AND no
    /// missing-marking — the one arm whose missing leg is empty. In the port this fires
    /// when a captured entry's own lstat reported no mtime: Go's filter uses the
    /// DirEntry.Info() facts (af.Info(), :256) and never re-stats, so the absence of a
    /// captured timestamp is the faithful trigger.
    QuickScanStatFailed,
}

/// One tag-read degradation, carried in the outcome rather than lost as a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFailure {
    /// Which arm.
    pub kind: TagFailureKind,
    /// The file, for [`TagFailureKind::PerFileSkipped`]. The folder-level arm's warning
    /// text names only the folder (Go's `:277`), but the file whose stat failed is kept
    /// here so the raw cause stays attributable.
    pub path: Option<String>,
    /// Failure text (Go's `log.Warn` payload / `state.sendWarning` text).
    pub message: String,
}

/// What one folder ingest produced: everything SC-06 needs (image-change detection,
/// progress, hash diffing) without re-reading the DB.
/// `CommittedFolderOutcome` is `Debug`-only in `nd-db` (its warnings carry a
/// `rusqlite::Error`), so this outcome is `Debug`-only too: tests assert on named
/// fields rather than comparing a whole struct.
#[derive(Debug)]
pub struct Phase1FolderOutcome {
    /// The SC-generated folder id (`model.FolderID`).
    pub folder_id: String,
    /// Library-relative logical folder path.
    pub folder_path: String,
    /// DB-14's committed report, published post-commit only (D-BATCH).
    pub committed: CommittedFolderOutcome,
    /// The fingerprint written to `folder.hash`.
    pub hash: String,
    /// Direct subfolders counted under this folder — Go's `folder.numSubFolders`
    /// (`walk_dir_tree.go:156-161`). Carried so a caller can compare this ingest's folder
    /// facts against the SC-02 walker's `FolderSnapshot::num_sub_folders` for the same scope.
    pub num_sub_folders: usize,
    /// Audio files found on disk: Go's `len(folder.audioFiles)` in `logFolder`
    /// (`:513-516`) and the progress `FileCount` (`:172-177`).
    pub audio_count: usize,
    /// Paths that replaced a pre-existing child row, so the DB's Match arm reused
    /// their ids. Go's `toImport[filePath] != nil` (`:252-254`).
    pub reimported_paths: Vec<String>,
    /// Tracks with no prior row: their batch ids are empty and DB's Match arm mints
    /// the durable id, which arrives in `committed.inserted_media_ids`.
    pub new_media_count: usize,
    /// Rows the batch's missing leg flagged (`:444-460`).
    pub missing_count: usize,
    /// Album-ID transitions this folder declared (`entry.albumIDMap`, `:316-321`).
    pub album_transitions: Vec<AlbumTransition>,
    /// Album ids the batch wrote.
    pub album_ids: Vec<String>,
    /// Tag-dictionary entries the batch added (`:396`).
    pub tag_count: usize,
    /// Artwork queue items the batch enqueued — derived by DB from artists and
    /// albums with Go's unknown/various filters (`:412-414`, `:423-425`).
    pub queued_item_count: usize,
    /// Whether the queue ran in `EnqueueIfMissing` mode (`:467-469`).
    pub enqueued_if_missing: bool,
    /// `true` when the folder row was newly inserted.
    pub folder_inserted: bool,
    /// Tag-read degradations, in emission order.
    pub tag_failures: Vec<TagFailure>,
    /// Walker/ignore policy warnings for this folder, in emission order.
    /// [`CommittedFolderOutcome::warnings`] carries the warn-only DB arms separately.
    pub warnings: Vec<ScanWarning>,
}

/// A track after tag-reading and bridging, before projection.
///
/// Go holds these as `entry.tracks` beside the `filesToImport` map values
/// (`folder_entry.go:52-57`, `:249-267`). Keeping the mapping DTO beside the record is
/// what lets [`projection::album_drafts`] and [`projection::artist_drafts`] consume
/// `&MediaFile` as SC-03's signatures require.
#[derive(Debug, Clone)]
struct ImportedTrack {
    /// Library-relative logical path, also `media_file.path`.
    path: String,
    /// Mapping DTO (SC-03's projection input).
    mapped: MediaFile,
    /// The record the batch writes.
    record: MediaFileRecord,
    /// Storage facts (size/mtime/birth) for the album draft.
    stat: TrackStat,
    /// Resolved birth: storage birth when reported, else the scan stamp
    /// ([`mapping_bridge::effective_birth_time`]). ONE resolution per file, shared by
    /// the media row and the album draft — the SC-03 contract that keeps the two rows
    /// from drifting (`mapping_bridge.rs:334-341`).
    birth: Option<DateTime<Utc>>,
    /// The pre-existing child row this track replaces, when any.
    previous: Option<MediaFileRecord>,
    /// Go's candidate previous album ids for this track (`loadTagsFromFiles:309-315`):
    /// the freshly derived id first, then the stored child's `album_id`, then — only
    /// when `prev_album_pid` is set — the id re-derived from that previous PID spec.
    /// [`album_transitions`] picks the first entry that differs from the new id, which
    /// is exactly Go's `prevAlbumID` precedence.
    previous_album_ids: Vec<String>,
}

/// Outcome of the tag-read pass.
struct TagReadPass {
    tracks: Vec<ImportedTrack>,
    /// Go's extractor skips and the two abort arms, in emission order.
    failures: Vec<TagFailure>,
    /// `true` when Go's `:274-279` arm fired: no tracks, albums, or artists — but the
    /// folder row is still persisted and `entry.missingTracks` was already computed.
    import_aborted: bool,
    /// `true` when Go's quick-scan filter arm (`:257-261`) fired instead: the same
    /// `entry, nil` return, but positioned BEFORE `:270`, so `entry.missingTracks` is
    /// never populated and the batch's missing leg must stay empty.
    missing_suppressed: bool,
}

/// `CancelToken` → `nd_utils::context::Cancellation` (SC-01 ratification seam 4:
/// this adapter is owned by SC and lives here, at the ingest boundary).
///
/// The DB visitor takes the plain flag; the scanner's token is wakeable and cannot be
/// converted into it (no shared interior), so the flag carries the *already-requested*
/// state and the ingest's own callback re-checks the token on every row. That is the
/// `D-CURSOR(2)` caller-stop arm: mid-read cancellation ends the visit through the
/// callback rather than through the flag, and the visit reports
/// [`VisitOutcome::Stopped`], which [`read_children`] maps to
/// [`Phase1Error::Cancelled`] (Go's cursor returns `ctx.Err()` as the iteration error
/// at `:240-243`, so a mid-read cancel is an abort, not a partial success).
fn cancellation_adapter(token: &CancelToken) -> Cancellation {
    let flag = Cancellation::new();
    if token.is_cancelled() {
        flag.cancel();
    }
    flag
}

/// Read one folder's existing children through DB-11's frozen folder-scoped visitor.
///
/// [`nd_db::scanner_queries::visit_media_in_folder`] IS `MediaFile.GetCursor` with
/// `Eq{"folder_id": …}` (`phase_1_folders.go:231-233`), frozen by DB-21's SigNN pins;
/// `scope: Option<&UserScope>` is that frozen shape and `None` is Go's clause-omitted
/// arm (`applyLibraryFilter` skips for the headless/admin logged user,
/// `persistence/sql_base_repository.go:264-270`), which is what a system scan runs as
/// per `D-SCOPE`.
///
/// The callback performs no DB work: the lease spans the whole read and a nested
/// checkout would block (`scanner_queries.rs:336-340` caller obligation).
///
/// # Errors
/// [`Phase1Error::ChildrenRead`] on a statement or decode failure (`:236`, `:242`),
/// and [`Phase1Error::Cancelled`] when the visit stopped because the token was
/// tripped. No rows are returned beside a failure: `D-CURSOR(3)` records that Go's
/// value-beside-error has no `Result` shape, and using a partial map to compute the
/// missing set would flag rows Go never saw.
fn read_children(
    store: &SQLStore,
    folder_id: &str,
    token: &CancelToken,
    folder: &str,
) -> Result<BTreeMap<String, MediaFileRecord>, Phase1Error> {
    let flag = cancellation_adapter(token);
    let mut collected: Vec<MediaFileRecord> = Vec::new();
    // `with_connection_sync`'s callback must return `Result<_, rusqlite::Error>`, so the
    // cursor's typed error travels as a VALUE out of the callback rather than being
    // folded into a fabricated rusqlite error; the only `Err` this callback can produce
    // is the checkout failure, which becomes `ChildrenReadSource::Pool`.
    let (outcome, cursor_error) = store
        .with_connection_sync(|conn| {
            let result =
                scanner_queries::visit_media_in_folder(conn, folder_id, None, &flag, |record| {
                    if token.is_cancelled() {
                        return Control::Stop;
                    }
                    collected.push(record);
                    Control::Continue
                });
            Ok(match result {
                Ok(outcome) => (outcome, None),
                Err(source) => (VisitOutcome::Completed { visited: 0 }, Some(source)),
            })
        })
        .map_err(|source| Phase1Error::ChildrenRead {
            folder: folder.to_string(),
            source: ChildrenReadSource::Pool(source),
        })?;
    if let Some(source) = cursor_error {
        return Err(Phase1Error::ChildrenRead {
            folder: folder.to_string(),
            source: ChildrenReadSource::Cursor(source),
        });
    }
    match outcome {
        VisitOutcome::Stopped { .. } => Err(Phase1Error::Cancelled {
            folder: folder.to_string(),
        }),
        VisitOutcome::Completed { .. } => Ok(collected
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect()),
    }
}

/// A folder selected as its own walker root: the listing captured under policy plus the
/// fingerprint Go would store for it.
///
/// This is the trusted object [`ingest_folder_snapshot`] consumes, so SC-06's walker can
/// hand it the folder it already captured instead of making this lane rescan the tree
/// (`decisions.md` D-SC05-SLICE item 4).
pub(crate) struct CapturedFolder {
    pub(crate) snapshot: FolderSnapshot,
    pub(crate) hash: String,
}

/// The preflight of a selected target, before any `.ndignore` read, listing, or DB work.
///
/// Reuses the SC-02 walker's own skip rules rather than inventing a second policy
/// (`decisions.md` D-SC05-SLICE item 1): a missing target, and with following disabled a
/// target that is a link or sits under one (`walk.rs::is_symlinked_path`, Go
/// `scanner/walk_dir_tree.go:47-51`). With following enabled nothing is rejected here, so
/// the configured-symlink-root exemption and the endpoint behavior of a followed target are
/// exactly as before.
///
/// # Errors
/// [`Phase1Error::TargetRejected`] for either rule, carrying the walker's warning for the
/// same rule so a caller can report it as a skipped target.
fn preflight_target(
    store: &LocalDir,
    job: &FolderJob<'_>,
    folder_text: &str,
) -> Result<(), Phase1Error> {
    let reject = |rejection: TargetRejection, kind: ScanWarningKind, message: String| {
        Phase1Error::TargetRejected {
            folder: folder_text.to_string(),
            rejection,
            warning: ScanWarning {
                kind,
                library_id: Some(job.library_id),
                path: Some(job.folder_path.clone()),
                message,
            },
        }
    };
    // `walk_dir_tree.go:39-45`: existence is checked with the same storage read the walker
    // uses, and it comes first, in Go's order.
    if store.snapshot(&job.folder_path, Follow::Yes).is_err() {
        return Err(reject(
            TargetRejection::Missing,
            ScanWarningKind::TargetMissing,
            "target folder does not exist; skipped".to_string(),
        ));
    }
    // `walk_dir_tree.go:47-51`: the walker's own port of the rule, reused verbatim.
    if !job.follow_symlinks && crate::walk::is_symlinked_path(store, &job.folder_path) {
        return Err(reject(
            TargetRejection::SymlinkedPath,
            ScanWarningKind::SymlinkedTargetSkipped,
            "target is a symlink or is reached through one; following is disabled".to_string(),
        ));
    }
    Ok(())
}

/// Capture the selected folder as its own walker root: preflight, `.ndignore` stack for the
/// target and its logical ancestors, one listing, and the fingerprint.
///
/// The listing itself is `loadDir` (`scanner/walk_dir_tree.go:110-178`) for one directory:
/// `.ndignore` patterns for the target and every ancestor (`:34-43`), then per entry the
/// ignore check (`:138-142`), the dir/file split (`:143-152`), the blocklist and dot rule
/// (`:153-155`), readable-directory accounting (`:156-158`), and mtime/image accounting
/// (`:159-176`). A **broken followed symlink** is skipped where Go skips it — after
/// `isDirOrSymlinkToDir` reports its endpoint as unresolvable (`:144-150`), before
/// `entry.Info()` — so its link mtime never reaches `mod_time` and it contributes no hash
/// line. Every other entry, INCLUDING a directory that cannot be opened, folds its `lstat`
/// mtime into `mod_time` before any classification decides its fate, and is then classified by
/// name exactly as Go's `else` branch does: an unreadable directory named `songs.mp3` counts
/// as audio, one named `cover.jpg` as an image. The SC-02 walker does the same (`walk.rs`
/// `accumulate_entry`), which is what makes the two paths fingerprint one tree identically —
/// divergence here would break SC-06's hash comparison. The `FolderSnapshot`'s own fingerprint
/// rules are SC-02's and are reused rather than re-expressed.
///
/// The only behavior change relative to a bare single-folder listing is that the target's
/// **child directories** are enterability-checked with the SC-02 walker's shared gate ([`crate::walk::enter_refusal`]) instead of being counted unconditionally. That
/// matters only where the walker refuses too — a child whose resolved identity is one of the
/// capture's live ancestors (`[root, target]`: a self-link, or an alias of the target itself) —
/// and keeping the two paths in agreement is the point: SC-06's change detection compares the
/// walker's hash with the hash this lane persists. The live set is **not** extended with an
/// entered child: this listing never descends, so Go's per-child recursion frame — which pops on
/// the way out (`scanner/walk_dir_tree.go:76-95`) — has no counterpart to push here, and a later
/// alias of an already-entered sibling stays indexable exactly as it is in Go.
///
/// # Errors
/// [`Phase1Error::TargetRejected`] before anything else, then [`Phase1Error::Walk`] when the
/// directory cannot be listed (`:111-127`), [`Phase1Error::EntryInfo`] when an entry's
/// captured mtime is absent, and [`Phase1Error::Cancelled`] when cancellation arrives
/// mid-listing (`:141`).
fn capture_selected_folder(
    job: &FolderJob<'_>,
    token: &CancelToken,
) -> Result<CapturedFolder, Phase1Error> {
    let folder_text = display_path(&job.folder_path);
    let store = LocalDir::from_path(job.library_root).map_err(|source| Phase1Error::Walk {
        folder: folder_text.clone(),
        source,
    })?;

    // Policy first: a rejected target produces a typed error and provably no persistence,
    // before the ignore stack is read or anything is listed.
    preflight_target(&store, job, &folder_text)?;

    // `walk_dir_tree.go:34-43`: the ignore stack covers the target and every ancestor.
    let mut checker = IgnoreStack::new();
    let mut warnings: Vec<ScanWarning> = Vec::new();
    let root = store.root().to_path_buf();
    let loader = |ancestors: &[&OsStr]| -> IgnoreFileOutcome {
        let mut path = root.clone();
        for component in ancestors {
            path.push(component);
        }
        path.push(ignore_file_name());
        crate::ignore::read_ignore_file(&path)
    };
    checker.push_all_parents(folder_text.as_str(), loader);
    warnings.extend(checker.take_warnings().into_iter().map(|mut warning| {
        warning.library_id = Some(job.library_id);
        warning.path = Some(job.folder_path.clone());
        warning
    }));

    let listing = store
        .list_annotated(&job.folder_path, Follow::Yes)
        .map_err(|source| Phase1Error::Walk {
            folder: folder_text.clone(),
            source,
        })?;

    // Go's folder modTime starts at the directory's own mtime (`:119`). An unreported mtime
    // is Go's zero `time.Time`, which the fingerprint renders as
    // `0001-01-01 00:00:00 +0000 UTC`; [`go_zero_time`] is that instant as a `SystemTime`,
    // so the preimage renders Go's string rather than silently hashing a Unix-epoch
    // timestamp no file and no directory ever had.
    let dir_modified = listing.dir_stat.modified.unwrap_or_else(go_zero_time);
    let mut mod_time = dir_modified;
    let mut images_updated_at: Option<SystemTime> = None;
    let mut num_sub_folders = 0usize;
    let mut entries: Vec<FolderEntry> = Vec::new();
    let mut coverage = FolderCoverage::Complete;

    // The selected folder is a fresh walker root, so the live identities are the configured
    // root plus this target — not its logical ancestors. Entering the target itself is never
    // refused (`is_walk_root`), exactly as Go starts a fresh recursion at a target.
    let active = crate::walk::selected_identity_context(&store, &job.folder_path);

    for entry in &listing.entries {
        if token.is_cancelled() {
            return Err(Phase1Error::Cancelled {
                folder: folder_text,
            });
        }

        let raw_name = entry
            .path
            .file_name()
            .unwrap_or(entry.path.components()[0].as_os_str());
        let name = match name_text(raw_name) {
            crate::contract::NameText::Valid(text) => text.to_string(),
            crate::contract::NameText::Invalid => {
                coverage = FolderCoverage::EntriesIncomplete;
                warnings.push(ScanWarning {
                    kind: ScanWarningKind::NameNotUtf8,
                    library_id: Some(job.library_id),
                    path: Some(entry.path.clone()),
                    message: format!("unrepresentable raw name {raw_name:?}; excluded"),
                });
                continue;
            }
        };

        // `checker.ShouldIgnore(ctx, entryPath)` (`:138-142`), on the entry's
        // library-relative logical path.
        if checker.should_ignore(&entry.path.to_string()) {
            continue;
        }

        // `isDirOrSymlinkToDir` (`:216-233`): a followed link to a directory counts as
        // a directory; with following disabled a symlink is a file-shaped entry.
        let is_dir = entry.kind == EntryKind::Dir
            || (job.follow_symlinks
                && entry.kind == EntryKind::Symlink
                && matches!(
                    entry.endpoint.as_ref().map(|endpoint| endpoint.kind),
                    Some(EndpointKind::Dir)
                ));

        if crate::contract::is_ignored_entry(&name, is_dir, job.ignore_dot_folders) {
            continue;
        }

        // `:156`: `if isDir && isDirReadable(ctx, job.fs, entryPath)`. A readable directory is
        // a subfolder only when the shared walker gate allows descending into it; anything that
        // falls through is never `Info()`ed on the counted path, so only a fell-through
        // directory contributes its own mtime below.
        let mut gate_refused = false;
        // A child listing that FAILED is not the same event as a readable-but-not-descendable
        // child, and the walker distinguishes them: `enter_directory` maps the storage error onto
        // one of its own warning kinds (`walk::listing_failure_warning`) and turns anything
        // outside that class into a fatal `ScanError::RootUnreadable`. Sharing that mapping here
        // is what keeps a vanished child (`NotFound`), a child that is no longer a directory
        // (`NotADirectory`), and an unopenable one (`PermissionDenied`) from reporting a warning
        // kind the walker never emits for the same tree — the capture's fall-through was already
        // Go-accurate (`walk_dir_tree.go:156-176`, `:301-312`); only the kind diverged while
        // every listing error collapsed into `DirectoryUnreadable`.
        let mut listing_failure: Option<ScanWarning> = None;
        if is_dir {
            match store.list(&entry.path, Follow::Yes) {
                Ok(_) => {
                    // The gate is the walker's own (`enter_refusal`: depth cap, then the
                    // active-identity cycle check), so a child that resolves to the selected
                    // folder is refused here exactly where the walker refuses it, and then falls
                    // through to the accumulation below — no subfolder count, the walker's
                    // warning, and classification by name, as `walk.rs`'s `EnterOutcome::Skipped`
                    // arm does.
                    //
                    // The identity context is NOT extended with an entered child: this is a flat
                    // sibling loop that never descends, so `active` stays exactly the capture's
                    // live ancestors (`[root, target]`, or `[root]` at the library root). Pushing
                    // an entered sibling would make a second alias to that SAME sibling look like
                    // an active cycle — Go indexes both aliases (`walk_dir_tree.go:144-152`,
                    // `walk_dir_tree_test.go:213`) — and would grow `active.len()` toward
                    // `MAX_WALK_DEPTH` with the number of children, so a wide folder would
                    // eventually report a depth refusal the descending walker never emits at that
                    // level. The walker pushes an identity only when it DESCENDS and pops it when
                    // it leaves (`walk.rs`, `EnterOutcome::Entered` then `state.active.pop()`);
                    // nothing here descends, so nothing is pushed.
                    let identity = canonical_identity(&store.os_path(&entry.path));
                    match crate::walk::enter_refusal(
                        &entry.path,
                        &identity,
                        false,
                        &active,
                        crate::walk::MAX_WALK_DEPTH,
                        job.library_id,
                    ) {
                        crate::walk::EnterCheck::Enter => {
                            num_sub_folders += 1;
                            continue;
                        }
                        crate::walk::EnterCheck::Refused(warning) => {
                            warnings.push(warning);
                            gate_refused = true;
                        }
                    }
                }
                Err(source) => {
                    match crate::walk::listing_failure_warning(&entry.path, &source, job.library_id)
                    {
                        // The walker's `EnterOutcome::Skipped` class: warn with ITS kind, then
                        // fall through to the same accumulation and classification.
                        Some(warning) => listing_failure = Some(warning),
                        // The walker's `EnterOutcome::Failed` class. Go's `loadDir` returns the
                        // listing error to `walkDirTree`, which fails that library's walk
                        // (`walk_dir_tree.go:111-127`, `:58-66`) and persists nothing, so this
                        // capture aborts with the same typed storage error rather than reporting
                        // a warning for a failure the walker treats as fatal.
                        None => {
                            return Err(Phase1Error::Walk {
                                folder: folder_text,
                                source,
                            })
                        }
                    }
                }
            }
        }

        // `isDirOrSymlinkToDir` (`:216-233`) reports a **followed** link whose endpoint cannot
        // be stat'd as an error, and `loadDir` (`:144-150`) logs `Scanner: Invalid symlink` and
        // `continue`s BEFORE `entry.Info()` (`:157-163`). A broken link therefore contributes
        // neither its own `lstat` mtime to `mod_time` nor a hash line, and its missing mtime
        // must never reach the `EntryInfo` abort below: Go's `DirEntry.Info()` on a broken
        // link is its `lstat`, which succeeds and whose mtime Go never even reads on this
        // path. The SC-02 walker ports the same order (`walk.rs` `accumulate_entry` returns
        // for a broken followed link BEFORE raising `frame.mod_time`), so the two paths
        // fingerprint a tree with a dangling link identically. With following DISABLED Go
        // does not skip — `isDirOrSymlinkToDir` returns `(false, nil)`, the entry reaches the
        // file branch and its mtime is folded in — which is why the check requires
        // `job.follow_symlinks`.
        if entry.kind == EntryKind::Symlink
            && job.follow_symlinks
            && !matches!(
                entry.endpoint.as_ref().map(|endpoint| endpoint.kind),
                Some(EndpointKind::File | EndpointKind::Dir | EndpointKind::Other)
            )
        {
            warnings.push(ScanWarning {
                kind: ScanWarningKind::SymlinkSkipped,
                library_id: Some(job.library_id),
                path: Some(entry.path.clone()),
                message: "broken followed symlink skipped before mtime accumulation".to_string(),
            });
            continue;
        }

        // Go's `else` branch (`:159-163`) is entered by EVERY other entry, INCLUDING a
        // directory that cannot be opened: `entry.Info()` — the entry's own `lstat` — is read and
        // its mtime raises `folder.modTime` BEFORE any classification or early `continue`
        // decides the entry's fate. An unreadable directory therefore still contributes its
        // lstat mtime to the fingerprint, and skipping the accumulation here would give this
        // ingest a different `folder.hash` from the same tree (`nd-scanner`'s SC-02 walker
        // accumulates it: `walk.rs`, the `EnterOutcome::Skipped` arm feeding `accumulate_entry`).
        match entry.stat.modified {
            Ok(modified) => {
                if modified > mod_time {
                    mod_time = modified;
                }
            }
            // `walk_dir_tree.go:157-161`: `entry.Info()` failing returns the error from
            // `loadDir`, which aborts this folder's ingest (the warning is logged, but
            // the folder is not emitted and never persisted). Rust reports the absence
            // as an error rather than fabricating a timestamp, so the same abort is
            // reachable here where Go's `FileInfo.ModTime()` cannot fail.
            Err(source) => {
                return Err(Phase1Error::EntryInfo {
                    folder: folder_text.clone(),
                    path: entry.path.to_string(),
                    source,
                })
            }
        }

        // Reaching here with `is_dir` means the child was NOT counted as a subfolder: either the
        // shared gate refused it, or its listing failed. A gate refusal is readable-but-not-
        // descendable and the walker already reported `DepthLimitReached`/`DirectoryCycleSkipped`
        // for it above, so `gate_refused` suppresses a second warning here — warning again would
        // give this ingest a warning set the walker never emits. A FAILED listing is the walker's
        // warn-and-skip class and carries the warning `walk::listing_failure_warning` derived from
        // the storage error, i.e. the exact kind the walker pushes for that failure. There is no
        // invented `DirectoryUnreadable` fallback here: for an `is_dir` child the listing either
        // succeeded — then the shared gate either counted it (`continue`) or refused it
        // (`gate_refused`) — or it failed, and the failed arm below either set `listing_failure`
        // (warn class) or returned `Phase1Error::Walk` (fatal class). Reaching this statement with
        // `is_dir && !gate_refused` therefore always has a listing warning to report. Its mtime and
        // classification are still folded in above and below, which is the part the fingerprint
        // depends on.
        if is_dir && !gate_refused {
            // Not counted as a subfolder and not descended into by this single-folder ingest
            // (SC-06's walker does). Go logs
            // `Scanner: Skipping unreadable directory` (`:301-312`) and then FALLS THROUGH the
            // same `else` branch it just entered: `resolveEntryName` (`:253-255`, which returns
            // the entry's own name for a non-symlink) and the classification switch (`:167-176`)
            // still run. So a directory named `songs.mp3` is indexed as an audio file, one named
            // `cover.jpg` as an image file (raising `imagesUpdatedAt`), and anything else is
            // unclassified — with the directory's own `lstat` size and mtime as hash facts. The
            // SC-02 walker has no early `continue` here either: an unopenable directory goes
            // straight through `accumulate_entry` and lands in `frame.entries_classified`
            // (`walk.rs`, `EnterOutcome::Skipped`). Warning first (Go's `log.Warn` precedes the
            // switch), then classify below: stopping here would persist `num_audio_files`,
            // `image_files`, `images_updated_at` and `folder.hash` that disagree with the
            // walker's snapshot of the same tree.
            warnings.push(listing_failure.take().expect(
                "an is_dir child that was neither counted by the gate nor gate-refused had a \
                 FAILED listing, whose warn-class warning is set above; the fatal class returns \
                 Phase1Error::Walk instead of falling through",
            ));
        }

        // `resolveEntryName` (`:253-298`): classify a followed link by its endpoint's base
        // name, and report whether that endpoint lies outside the root — the same relation the
        // SC-02 walker records (`walk.rs` `is_outside_root`). A non-symlink (including an
        // unreadable directory, which falls through above) is classified by its own name.
        let classification = if entry.kind != EntryKind::Symlink {
            Some((name.clone(), false))
        } else if !job.follow_symlinks {
            None
        } else {
            match store.resolve(&entry.path, Follow::Yes) {
                Ok(ResolvedTarget::Endpoint(endpoint))
                    if matches!(endpoint.kind, EndpointKind::File | EndpointKind::Other) =>
                {
                    match endpoint.target.file_name().map(name_text) {
                        Some(crate::contract::NameText::Valid(base)) => {
                            Some((base.to_string(), is_outside_root(&store, &endpoint.target)))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        };
        let Some((classification, outside_root)) = classification else {
            warnings.push(ScanWarning {
                kind: ScanWarningKind::SymlinkSkipped,
                library_id: Some(job.library_id),
                path: Some(entry.path.clone()),
                message: "symlink not classified (following disabled or endpoint unusable)"
                    .to_string(),
            });
            continue;
        };

        if outside_root {
            // Go indexes a followed link whose endpoint lies outside the root when the target
            // extension is audio (`walk_dir_tree_test.go:520`); the logical path stays inside,
            // and the warning records why the entry is unusual.
            warnings.push(ScanWarning {
                kind: ScanWarningKind::OutsideRootEndpoint,
                library_id: Some(job.library_id),
                path: Some(entry.path.clone()),
                message: "symlink endpoint lies outside the library root; classified by target"
                    .to_string(),
            });
        }

        // Go's switch order (`:167-176`): audio, playlist, image.
        let category = if nd_mapping::file_types::is_audio_file(&classification) {
            crate::folder::EntryCategory::Audio
        } else if nd_mapping::file_types::is_valid_playlist(&classification) {
            crate::folder::EntryCategory::Playlist
        } else if nd_mapping::file_types::is_image_file(&classification) {
            crate::folder::EntryCategory::Image
        } else {
            crate::folder::EntryCategory::Other
        };

        if category == crate::folder::EntryCategory::Image {
            // `utils.TimeNewest(imagesUpdatedAt, fileInfo.ModTime(), folder.modTime)`
            // (`:172`, `utils/time.go:11-19`).
            let mut times = vec![mod_time];
            if let Some(existing) = images_updated_at {
                times.push(existing);
            }
            images_updated_at = newest(&times);
        }

        entries.push(FolderEntry {
            name,
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

    // Go keys its maps by entry name and sorts keys before hashing
    // (`folder_entry.go:117-123`); `FolderSnapshot::category` sorts by name bytes.
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    let snapshot = FolderSnapshot {
        library_id: job.library_id,
        path: job.folder_path.clone(),
        dir_stat: listing.dir_stat,
        mod_time,
        images_updated_at,
        num_sub_folders,
        entries,
        coverage,
        warnings,
    };
    // The fingerprint is part of the capture: it is derived from the captured facts only,
    // so the object handed to [`ingest_folder_snapshot`] carries the exact hash that the
    // snapshot describes.
    let hash = fingerprint(&snapshot, &folder_text)?;
    Ok(CapturedFolder { snapshot, hash })
}

/// Fingerprint a captured snapshot and wrap a failure in [`Phase1Error::Hash`], so the arm has
/// one owner and can be executed directly.
///
/// `capture_selected_folder` reaches this only with facts it just read from disk, and every
/// failure class the fingerprint can report is already aborted earlier on that path (an entry
/// with no mtime aborts as [`Phase1Error::EntryInfo`], and no supported filesystem can store an
/// mtime outside chrono's renderable range). The arm therefore exists for the OTHER caller the
/// split exposes: SC-06 hands [`ingest_folder_snapshot`] a snapshot it captured itself, and
/// that snapshot's facts are not this lane's listing. The unit test below drives exactly that
/// shape — a snapshot whose entry reports no mtime — rather than claiming a filesystem path
/// this lane cannot produce.
///
/// # Errors
/// [`Phase1Error::Hash`] wrapping the [`FolderSnapshotError`] the fingerprint reported.
fn fingerprint(snapshot: &FolderSnapshot, folder: &str) -> Result<String, Phase1Error> {
    snapshot.hash().map_err(|source| Phase1Error::Hash {
        folder: folder.to_string(),
        source,
    })
}

/// Read tags for the folder's audio files and bridge each into a record.
///
/// `filesToImport` here is every audio file found (see module docs for the quick-scan
/// filter SC-06 owns). Per file: `nd_tagreader::read_metadata` on the real OS path,
/// [`Metadata::new`] with the storage stat plus the mapping context,
/// `to_media_file(library_id, folder_id, effective_pid)` (`map_mediafile.go:24`), then
/// [`mapping_bridge::bridge_media_file`] with the scan stamp as the birth substitute.
///
/// Files are read one at a time rather than in Go's `filesBatchSize = 200` chunks
/// (`:294-300`): the per-file skip arm is identical either way, and the batch arm aborts
/// the whole folder on the FIRST failing chunk in Go too, so the observable outcome does
/// not depend on the chunk size. SC-06 can chunk without changing this lane's semantics.
///
/// Tag failures follow Go's arms exactly: a per-file parse failure omits the file
/// (`gotaglib.go:34-43`, whose `continue` means `Parse` still returns `nil`), and a stat
/// that cannot be read at all is the failure Go turns into warn + `return entry, nil`
/// (`:254-259` in the quick-scan filter, or the `fs.Stat` fallback inside
/// `localFS.ReadTags` at `core/storage/local/local.go:97-103` reaching `:274-279`).
/// Neither is a [`Phase1Error`]: Go's stage returns `entry, nil`, so the folder still
/// commits.
fn tag_and_bridge(
    job: &FolderJob<'_>,
    store: &LocalDir,
    snapshot: &FolderSnapshot,
    children: &BTreeMap<String, MediaFileRecord>,
    token: &CancelToken,
) -> Result<TagReadPass, Phase1Error> {
    let folder_id = job.folder_id();
    let pid = job.pid.effective();
    let folder_text = display_path(&job.folder_path);
    let mut tracks = Vec::new();
    let mut failures = Vec::new();

    for entry in snapshot.category(crate::folder::EntryCategory::Audio) {
        if token.is_cancelled() {
            return Err(Phase1Error::Cancelled {
                folder: folder_text,
            });
        }
        let path = entry.path.to_string();

        // `:252-265` — Go's import selection. A child with no stored row, or any folder
        // scanned while a full scan is in progress, is imported outright. Otherwise the
        // child's own `DirEntry.Info()` facts decide (`af.Info()`, `:256`: the entry's own
        // lstat, NOT the re-stat `localFS.ReadTags` performs later) — import when the file
        // is newer than the row's `updated_at`, or when the stored row is already missing.
        // A file present on disk but NOT imported is still removed from the DB map by the
        // missing leg (`:266`), so it is never flagged missing.
        let previous = children.get(&path);
        if !job.full_scan_mode {
            if let Some(stored) = previous {
                // `:257-261`: a child whose `Info()` cannot be read warns with Go's
                // PER-FILE text and returns `entry, nil`, which happens BEFORE
                // `entry.missingTracks` is computed (`:270`). The folder row is still
                // persisted — with no tracks, albums or artists and no missing-marking,
                // hence [`TagReadPass::missing_suppressed`]. In the port the absence of a
                // captured mtime is that failure: Go's filter never re-stats.
                let modified = match entry.stat.modified {
                    Ok(modified) => modified,
                    Err(source) => {
                        failures.push(TagFailure {
                            kind: TagFailureKind::QuickScanStatFailed,
                            path: Some(path.clone()),
                            message: quick_scan_stat_message(&folder_text, &entry.name, &source),
                        });
                        return Ok(TagReadPass {
                            tracks: Vec::new(),
                            failures,
                            import_aborted: false,
                            missing_suppressed: true,
                        });
                    }
                };
                // Go compares against `dbTrack.UpdatedAt`, a non-pointer `time.Time`, so a
                // NULL column would have failed Go's row scan outright. The port's record
                // types it as `Option`, and the year-one instant is the faithful reading of
                // "no stored stamp": the file is then always newer, exactly as an unset
                // Go timestamp would be. `nd_db::time::zero` is that instant.
                let stored_updated_at = stored.updated_at.unwrap_or_else(nd_db::time::zero);
                if system_time_to_utc(modified) <= stored_updated_at && !stored.missing {
                    continue;
                }
            }
        }

        // `localFS.ReadTags` stats the file and attaches the FileInfo
        // (`local.go:90-109`); a stat failure there fails the whole batch, which
        // `processFolder` reports with its FOLDER-level warning (`:274-279`) — the raw
        // error is kept and formatted here, at the arm that owns Go's warning text.
        let stat_source = match stat_for_mapping(store, entry) {
            Ok(stat) => stat,
            Err(source) => {
                failures.push(TagFailure {
                    kind: TagFailureKind::FolderImportAborted,
                    path: Some(path),
                    message: folder_import_abort_message(&folder_text, &source),
                });
                return Ok(TagReadPass {
                    tracks: Vec::new(),
                    failures,
                    import_aborted: true,
                    missing_suppressed: false,
                });
            }
        };

        let mut stat = track_stat_for_mapping(&stat_source, &path)?;
        // `localFS.noBirthTime`: the device is known to report no creation time, so the
        // storage fact is dropped here and the record layer substitutes the injected
        // clock. Dropping it in ONE place keeps the media row and the album draft
        // resolving to the same instant.
        if job.no_birth_time {
            stat.birth_time = None;
        }

        // The backend reads the real OS file. A parse failure is Go's extractor
        // `continue`: the file is omitted, the folder survives.
        let os_path = store.os_path(&entry.path);
        let info = match nd_tagreader::read_metadata(&os_path) {
            Ok(info) => info,
            Err(source) => {
                failures.push(TagFailure {
                    kind: TagFailureKind::PerFileSkipped,
                    path: Some(path),
                    message: format!("error extracting metadata from file, skipping: {source}"),
                });
                continue;
            }
        };

        let metadata = Metadata::new(
            path.clone(),
            &info,
            stat.to_mapping_file_stat(),
            job.mapping,
        );
        let library_id = i32::try_from(job.library_id).unwrap_or_else(|_| {
            panic!(
                "library_id {} exceeds the mapping DTO's i32",
                job.library_id
            )
        });
        let mapped = metadata.to_media_file(library_id, &folder_id, pid);
        // One birth resolution, shared by the media row and the album draft.
        let birth = mapping_bridge::effective_birth_time(&stat, Some(job.scan_stamp));
        let record = match mapping_bridge::bridge_media_file(
            &metadata,
            &mapped,
            &stat,
            Some(job.scan_stamp),
        ) {
            Ok(record) => record,
            Err(source) => return Err(Phase1Error::Bridge { path, source }),
        };

        // The stored child this track replaces: the SAME entry the selection arms above
        // consulted, so the filter and Go's album-id precedence cannot disagree about
        // which row a path matched (`:311-312`).
        let previous = previous.cloned();
        // Go's `prevAlbumID` precedence (`:309-315`): the derived id, then the stored
        // child's id, then the id re-derived from the previous PID spec. The third arm
        // runs only when no child row exists (Go's `else if`).
        let mut previous_album_ids = vec![record.album_id.clone()];
        if let Some(stored) = previous.as_ref() {
            previous_album_ids.push(stored.album_id.clone());
        } else if let Some(spec) = job.prev_album_pid {
            previous_album_ids.push(nd_mapping::persistent_ids::album_id(
                &mapped, &metadata, spec,
            ));
        }

        tracks.push(ImportedTrack {
            path,
            mapped,
            record,
            stat,
            birth,
            previous,
            previous_album_ids,
        });
    }

    Ok(TagReadPass {
        tracks,
        failures,
        import_aborted: false,
        missing_suppressed: false,
    })
}

/// Project one entry's storage facts into [`TrackStat`], wrapping a conversion failure in
/// [`Phase1Error::Bridge`] so the arm has one owner and can be executed directly.
///
/// # Errors
/// [`Phase1Error::Bridge`] for the [`BridgeError`] the conversion reported — a size past
/// `i64::MAX` or an instant chrono cannot place in UTC. Neither is producible from a real file
/// on a 64-bit host (the unit test below pins the arm and states that limit rather than
/// implying the arm runs end-to-end), and nothing is clamped or defaulted across it.
fn track_stat_for_mapping(source: &FileStat, path: &str) -> Result<TrackStat, Phase1Error> {
    TrackStat::from_storage_stat(source).map_err(|source| Phase1Error::Bridge {
        path: path.to_string(),
        source,
    })
}

/// The stat the record and the album draft use: `fs.Stat` of the file
/// (`core/storage/local/local.go:97-103`), which for a followed symlink is the
/// endpoint while the folder hash uses the link's own `lstat`
/// (`folder_entry.go:126-131`). A snapshot that cannot be read is the batch failure.
///
/// Returns the raw [`FileStat`], or the raw [`StorageError`] Go's `fs.Stat` arm would
/// wrap (`:277`'s `%v`). The failure TEXT is not formed here: this stat belongs to
/// `localFS.ReadTags`, whose failure always surfaces at `processFolder`'s folder-level
/// arm (`:274-279`), and [`folder_import_abort_message`] is the only place that spelling
/// exists. Go's quick-scan filter text (`:254-259`) belongs to
/// [`quick_scan_stat_message`] and must never be emitted from this arm.
fn stat_for_mapping(store: &LocalDir, entry: &FolderEntry) -> Result<FileStat, StorageError> {
    if let Some(endpoint) = entry.endpoint.clone() {
        return Ok(endpoint);
    }
    store
        .snapshot(&entry.path, Follow::Yes)
        .map(|snapshot| snapshot.stat)
}

/// Go's quick-scan filter failure text (`phase_1_folders.go:259`):
/// `fmt.Sprintf("Error getting file info for %s/%s: %v", entry.path, af.Name(), err)`.
/// Two differences from [`folder_import_abort_message`] matter and are asserted:
/// this one names the FOLDER **and the file name**, and the file name is `af.Name()` —
/// the entry's own name, not its logical path. Go's error here is `af.Info()`'s failure,
/// so in the port the carried cause is the platform's timestamp absence rather than a
/// [`StorageError`].
fn quick_scan_stat_message(
    folder_text: &str,
    name: &str,
    source: &dyn std::fmt::Display,
) -> String {
    format!("Error getting file info for {folder_text}/{name}: {source}")
}

/// Go's folder-level tag-failure warning (`phase_1_folders.go:277`):
/// `fmt.Sprintf("Error loading tags from files in %s: %v", entry.path, err)`. The `%s` is
/// the FOLDER (`entry.path`), not the file that failed, so this is the only formatter
/// [`TagFailureKind::FolderImportAborted`] messages go through.
fn folder_import_abort_message(folder_text: &str, source: &StorageError) -> String {
    format!("Error loading tags from files in {folder_text}: {source}")
}

/// Whether a resolved endpoint lies outside the library root — the SC-02 walker's
/// `is_outside_root` (`walk.rs:1125-1129`) restated: canonicalized endpoint against the
/// canonicalized configured root, with the root itself never counted as outside and a
/// failed `canonicalize` keeping the raw path so the check stays best-effort rather than
/// silently reporting "inside". The walker's helper is private, so the relation is
/// restated rather than diverged from: the flag drives only the warning and
/// [`FolderEntry::outside_root`], never the fingerprint (whose facts are the link's own
/// `lstat`), so an ingest and a walk of the same tree must agree on it entry for entry.
#[must_use]
fn is_outside_root(store: &LocalDir, target: &Path) -> bool {
    let root = canonical_identity(store.root());
    let endpoint = canonical_identity(target);
    root != endpoint && !endpoint.starts_with(&root)
}

/// Canonicalized path identity, falling back to the raw path when the platform cannot
/// canonicalize it.
#[must_use]
fn canonical_identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `entry.missingTracks` (`:269-270`): children whose path is absent from disk. Go
/// deletes every observed path from the DB map first (`:266`), whether or not it was
/// imported, so a file present on disk is never missing even when its tag read was
/// skipped.
fn missing_leg(
    children: &BTreeMap<String, MediaFileRecord>,
    on_disk: &BTreeMap<String, ()>,
) -> Vec<MissingMedia> {
    children
        .values()
        .filter(|record| !on_disk.contains_key(&record.path))
        .map(|record| MissingMedia {
            id: record.id.clone(),
            album_id: record.album_id.clone(),
        })
        .collect()
}

/// `entry.albumIDMap` (`:309-321`): the previous album id for a newly derived one,
/// keyed by the NEW id. Go's `prevAlbumID` precedence is reproduced exactly
/// (`:309-315`): the derived id first, then a matched child's stored `album_id`
/// (`:311-312`), then — only when no child exists and `prev_album_pid` is set — the id
/// re-derived from the previous PID spec (`:313-315`). The first candidate that differs
/// from the derived id wins, so an unchanged track declares nothing. Go's
/// `_, ok := entry.albumIDMap[track.AlbumID]` guard (`:316-317`) lets the FIRST track of
/// an album decide the mapping; with one track per album that is the same decision, and
/// the guard is ported rather than elided.
fn album_transitions(tracks: &[ImportedTrack], albums: &[AlbumRecord]) -> Vec<AlbumTransition> {
    let drafted: BTreeMap<&str, ()> = albums.iter().map(|album| (album.id.as_str(), ())).collect();
    let mut map: BTreeMap<&str, AlbumTransition> = BTreeMap::new();
    for track in tracks {
        let new_id = track.record.album_id.as_str();
        if !drafted.contains_key(new_id) {
            continue;
        }
        // Go's guard (`:316-317`): the FIRST track of an album decides its mapping, so a
        // later track with the same new id never overwrites it.
        if map.contains_key(new_id) {
            continue;
        }
        // Go's `prevAlbumID` (`:309-315`): the first candidate that differs from the
        // derived id, i.e. the stored child's id when present, else the previous-PID
        // re-derivation. Candidates start with the derived id itself, so an unchanged
        // track yields no transition at all.
        let prev_id = track
            .previous_album_ids
            .iter()
            .find(|candidate| *candidate != new_id && !candidate.is_empty());
        let Some(prev_id) = prev_id else {
            continue;
        };
        map.insert(
            new_id,
            AlbumTransition {
                new_id: new_id.to_string(),
                prev_id: prev_id.clone(),
            },
        );
    }
    map.into_values().collect()
}

/// `entry.tags` (`:305-307`, `:323`): unique dictionary entries over every imported
/// track in Go's `Tags.FlattenAll` shape (`model/tag.go:111-119`), deduped by id. Go
/// dedups through a map keyed by `t.ID`; the `BTreeMap` gives deterministic order
/// where Go's map iteration is nondeterministic (`nd-mapping/tests/go_oracle.rs:442-455`).
fn tag_list(tracks: &[ImportedTrack]) -> Vec<Tag> {
    let mut unique: BTreeMap<String, Tag> = BTreeMap::new();
    for track in tracks {
        for (name, values) in &track.record.tags {
            for value in values {
                let id = nd_id::new_tag_id(name, value);
                unique.insert(
                    id.clone(),
                    Tag {
                        id,
                        tag_name: name.clone(),
                        tag_value: value.clone(),
                        album_count: 0,
                        song_count: 0,
                    },
                );
            }
        }
    }
    unique.into_values().collect()
}

/// Build the immutable batch: `entry.toFolder()` (`folder_entry.go:94-104`) plus
/// `entry.tags`/`artists`/`albums`/`tracks`/`missingTracks`/`albumIDMap`, with caller
/// stamps and the two configured booleans.
///
/// Every [`FolderStamps`] field is the same `scan_stamp`: Go samples the clock
/// microseconds apart inside one folder transaction, and `nd-db`'s
/// `tests/runtime_folder_writes.rs` records that equal values are permitted while
/// separate fields are what stops a commit-clock implementation from passing a named
/// assertion.
/// The projection's drafts plus the missing leg, bundled for [`build_batch`] so the
/// builder stays within a readable argument list.
struct Drafts {
    albums: Vec<AlbumRecord>,
    artists: Vec<ArtistRecord>,
    missing: Vec<MissingMedia>,
    transitions: Vec<AlbumTransition>,
}

fn build_batch(
    job: &FolderJob<'_>,
    snapshot: &FolderSnapshot,
    hash: String,
    pass: &TagReadPass,
    drafts: Drafts,
) -> FolderWriteBatch {
    let stamp = job.scan_stamp;
    let root = job.folder_path.is_root();
    let folder = nd_db::folder_repo::FolderRecord {
        id: job.folder_id(),
        library_id: job.library_id,
        // Go splits the full path into `Path` (cleaned directory) and `Name`
        // (basename) (`model/folder.go:56-57`).
        path: if root {
            String::new()
        } else {
            display_path(&job.folder_path.parent())
        },
        name: if root {
            // Go's `NewFolder` (`model/folder.go:57-75`) splits the library root's cleaned
            // path `"."` into `dir = ""` and `name = "."`: `path.Split(".")` yields an empty
            // directory and the name `"."`, and only the `dir == "." && name == "."` arm
            // empties `dir` — never `name`. `model/folder_test.go:115` pins `folder.Name == "."`
            // for the root folder, so the root row stores `"."`, not `""`.
            ".".to_string()
        } else {
            job.folder_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        parent_id: job.parent_id(),
        num_audio_files: i64::try_from(snapshot.num_audio_files()).unwrap_or(i64::MAX),
        // `playlists.InPath` (`core/playlists/playlists.go:76-87`) with the default
        // empty `PlaylistsPath` (`conf/configuration.go:998`) matches every folder.
        num_playlists: i64::try_from(
            snapshot
                .category(crate::folder::EntryCategory::Playlist)
                .len(),
        )
        .unwrap_or(i64::MAX),
        // `folder.ImageFiles = slices.Collect(maps.Keys(f.imageFiles))` (`:100`). Go's
        // empty map yields a non-nil empty slice, and `dbFolder.PostMapArgs` renders
        // nil and empty alike as `"[]"`; `Some(vec![])` is that state.
        image_files: Some(
            snapshot
                .image_names()
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        images_updated_at: images_updated_at_of(snapshot),
        hash,
        missing: false,
        updated_at: stamp,
        created_at: Some(stamp),
        // `library_path` is the join's `library.path as library_path`, not a column of
        // `folder`: empty here, since the batch's Put never writes it.
        library_path: String::new(),
    };

    FolderWriteBatch {
        library_id: job.library_id,
        folder,
        tags: tag_list(&pass.tracks),
        artists: drafts.artists,
        albums: drafts.albums,
        media: pass
            .tracks
            .iter()
            .map(|track| track.record.clone())
            .collect(),
        missing: drafts.missing,
        album_transitions: drafts.transitions,
        full_scan_mode: job.full_scan_mode,
        append_subtitle: job.append_subtitle,
        stamps: FolderStamps {
            folder_put: stamp,
            artist_put: stamp,
            album_put: stamp,
            mediafile_put: stamp,
            mark_missing: stamp,
            album_touch: stamp,
            artwork: stamp,
        },
    }
}

/// `folderEntry.imagesUpdatedAt`: Go's zero `time.Time` for a folder with no images
/// (`folder_entry.go:47`, left zero by `newFolderEntry`). `FolderRecord`'s column is
/// `NOT NULL`, so the port spells Go's zero as the year-one instant rather than `None`.
fn images_updated_at_of(snapshot: &FolderSnapshot) -> DateTime<Utc> {
    match snapshot.images_updated_at {
        Some(at) => system_time_to_utc(at),
        None => nd_db::time::zero(),
    }
}

/// Go's zero `time.Time` as a [`SystemTime`]: the year-one instant
/// (`0001-01-01 00:00:00 +0000 UTC`), which is 621_355_968_00 seconds before the Unix
/// epoch. [`crate::folder::go_time_string`] renders it exactly as Go's
/// `time.Time{}.String()` does, so an unreported directory mtime fingerprints the same
/// bytes Go hashes instead of a fabricated Unix-epoch timestamp.
#[must_use]
fn go_zero_time(_unreported: crate::storage::StatUnavailable) -> SystemTime {
    SystemTime::UNIX_EPOCH - Duration::from_secs(62_135_596_800)
}

/// `SystemTime` → `DateTime<Utc>`, falling back to Go's zero time for an instant
/// chrono cannot place (Go's `time.Time` renders year-one rather than failing).
fn system_time_to_utc(value: SystemTime) -> DateTime<Utc> {
    let nanos: i128 = match value.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i128::from(d.as_secs()) * 1_000_000_000 + i128::from(d.subsec_nanos()),
        Err(e) => -(i128::from(e.duration().as_secs()) * 1_000_000_000),
    };
    let secs = i64::try_from(nanos.div_euclid(1_000_000_000)).unwrap_or(if nanos < 0 {
        i64::MIN
    } else {
        i64::MAX
    });
    chrono::TimeZone::timestamp_opt(&Utc, secs, nanos.rem_euclid(1_000_000_000) as u32)
        .single()
        .unwrap_or_else(nd_db::time::zero)
}

/// Slash-separated display form of a logical path, used as the DB path and the folder
/// id key's cleaned form. [`LogicalPath::root`] renders `"."`, which is what Go stores
/// for the library root (`path.Clean(".")`).
#[must_use]
fn display_path(path: &LogicalPath) -> String {
    path.to_string()
}

/// Persist one folder from a captured snapshot: read its DB children, tag and bridge every
/// audio file in the snapshot, project album/artist drafts, and commit one folder
/// transaction through DB-14.
///
/// The snapshot is **trusted input** (`decisions.md` D-SC05-SLICE item 4): nothing here
/// re-lists the tree, so SC-06's walker can hand this core the folder it already captured —
/// the authoritative snapshot whose hash drove the change detection — instead of making the
/// ingest rescan a tree that may have changed in between. Trust is bounded by ONE check
/// before any DB work: the snapshot's library id and logical path must be the job's, else
/// the ingest aborts with [`Phase1Error::SnapshotMismatch`] and no writes.
///
/// `store` is reused, not rebuilt: the children read holds one lease, DB-14 acquires its own
/// for the transaction, and no lease spans the filesystem or tag I/O (`subplans/scanner.md`
/// transaction rules). No second retry loop is added — `D-BATCH` gives the store busy retry
/// and this lane the abort decision.
///
/// Crate-private deliberately: SC-06's controller is the caller that decides WHEN a captured
/// folder is ingested; no orchestration or traversal is exposed here, and no outside consumer
/// can hand it an unvalidated snapshot.
///
/// # Errors
/// Every [`Phase1Error`] arm except [`Phase1Error::TargetRejected`] — that preflight belongs
/// to the capture half — each matching a distinct Go abort. Nothing is committed on any error
/// path, and `CommittedFolderOutcome` warnings are published only after a commit.
///
/// # Panics
/// Only on a `library_id` too large for the mapping DTO's `i32` (`MediaFile.LibraryID`
/// is Go's `int`); callers pass a 1-based SQLite autoincrement key.
pub(crate) fn ingest_folder_snapshot(
    store: &SQLStore,
    job: &FolderJob<'_>,
    captured: &CapturedFolder,
    token: &CancelToken,
) -> Result<Phase1FolderOutcome, Phase1Error> {
    let folder_text = display_path(&job.folder_path);

    // Trust boundary, checked before any DB work: a snapshot describing another folder or
    // library would otherwise write a folder row under an id its listing does not cover.
    if captured.snapshot.library_id != job.library_id || captured.snapshot.path != job.folder_path {
        return Err(Phase1Error::SnapshotMismatch {
            folder: folder_text.clone(),
            expected_library_id: job.library_id,
            captured_library_id: captured.snapshot.library_id,
            expected_path: folder_text,
            captured_path: captured.snapshot.path.to_string(),
        });
    }

    // `phase_1_folders.go:226-228`: the abort check precedes any DB work.
    if token.is_cancelled() {
        return Err(Phase1Error::Cancelled {
            folder: folder_text,
        });
    }

    let snapshot = &captured.snapshot;
    let hash = captured.hash.clone();
    let folder_id = job.folder_id();

    // 1. Existing children (`:231-245`). A failure aborts only this folder.
    let children = read_children(store, &folder_id, token, &folder_text)?;

    let local = LocalDir::from_path(job.library_root).map_err(|source| Phase1Error::Walk {
        folder: folder_text.clone(),
        source,
    })?;

    // 2. Tag + bridge (`:272-283`).
    let pass = tag_and_bridge(job, &local, snapshot, &children, token)?;

    // 3. Missing children (`:266-270`): every path observed on disk is removed from
    //    the DB map regardless of whether it was imported. The quick-scan filter's own
    //    abort arm (`:257-261`) returns BEFORE `:270`, so that folder's missing leg is
    //    empty by Go's ordering, not by a policy choice.
    let missing = if pass.missing_suppressed {
        Vec::new()
    } else {
        let on_disk: BTreeMap<String, ()> = snapshot
            .category(crate::folder::EntryCategory::Audio)
            .into_iter()
            .map(|entry| (entry.path.to_string(), ()))
            .collect();
        missing_leg(&children, &on_disk)
    };

    // 4. Album/artist drafts (`:281-282`), guarded exactly as Go guards it
    //    (`if len(filesToImport) > 0`). A folder-level tag abort leaves `tracks` empty,
    //    so no drafts and no album/artist writes — Go's `entry, nil` arm.
    let mut albums: Vec<AlbumRecord> = Vec::new();
    let mut artists: Vec<ArtistRecord> = Vec::new();
    let mut transitions: Vec<AlbumTransition> = Vec::new();
    if !pass.import_aborted && !pass.tracks.is_empty() {
        let refs: Vec<&MediaFile> = pass.tracks.iter().map(|track| &track.mapped).collect();
        albums = projection::album_drafts(
            &refs,
            // One facts bundle per group MEMBER, keyed by logical path. Go reads
            // `Info.FileInfo` per track and loops the whole group for both time arms
            // (`model/mediafile.go:389-390`), so `created_at` is the oldest birth and
            // `updated_at` the newest mtime across the group — a single per-album
            // resolution cannot express that. A path with no entry means the ingest
            // lost a track it had mapped: refuse (SC-04) rather than fabricate a
            // zero-size `TrackStat` whose lookup could land on another track's facts.
            |track| {
                pass.tracks
                    .iter()
                    .find(|candidate| candidate.path == track.path)
                    .map(|candidate| TrackFacts {
                        stat: candidate.stat.clone(),
                        birth: candidate.birth,
                    })
                    .ok_or_else(|| AggregateError::MissingTrackFacts {
                        path: track.path.clone(),
                    })
            },
            job.mapping.mappings(),
            job.scan_stamp,
        )
        .map_err(|source| Phase1Error::Projection {
            folder: folder_text.clone(),
            source,
        })?;
        artists = projection::artist_drafts(&refs);
        transitions = album_transitions(&pass.tracks, &albums);
    }

    // 6-7. Batch (tags and artwork queue items are DB-derived), validate, persist.
    let batch = build_batch(
        job,
        snapshot,
        hash.clone(),
        &pass,
        Drafts {
            albums,
            artists,
            missing,
            transitions: transitions.clone(),
        },
    );
    // `persist_folder_delta` validates before its checkout; validating here too keeps a
    // structurally invalid batch from reaching the pool at all.
    batch.validate().map_err(|rejection| Phase1Error::Persist {
        folder: folder_text.clone(),
        source: FolderRunError::Domain(rejection),
    })?;

    if token.is_cancelled() {
        return Err(Phase1Error::Cancelled {
            folder: folder_text,
        });
    }

    let committed = persist_folder_delta(store, &batch).map_err(|source| Phase1Error::Persist {
        folder: folder_text.clone(),
        source,
    })?;

    // 8. Return, post-commit only. `imageChanges.record` (`:363-369`) is SC-06 and is
    //    deliberately absent, as are progress events and the missing-folder finalizer.
    // Read the scalars before `committed` is moved into the outcome.
    let queued_item_count = committed.queued_item_count;
    let enqueued_if_missing = committed.enqueued_if_missing;
    let folder_inserted = committed.folder_inserted;
    let missing_count = committed.missing_rows_marked;

    Ok(Phase1FolderOutcome {
        folder_id,
        folder_path: folder_text,
        queued_item_count,
        enqueued_if_missing,
        folder_inserted,
        album_ids: batch.albums.iter().map(|album| album.id.clone()).collect(),
        committed,
        hash,
        num_sub_folders: snapshot.num_sub_folders,
        audio_count: snapshot.num_audio_files(),
        reimported_paths: pass
            .tracks
            .iter()
            .filter(|track| track.previous.is_some())
            .map(|track| track.path.clone())
            .collect(),
        new_media_count: pass.tracks.iter().filter(|t| t.previous.is_none()).count(),
        missing_count,
        album_transitions: transitions,
        tag_count: batch.tags.len(),
        warnings: snapshot.warnings.clone(),
        tag_failures: pass.failures.clone(),
    })
}

/// Scan one selected folder end to end: capture it as its own walker root, then persist
/// exactly that captured snapshot.
///
/// The crate's only public phase-1 entry, and deliberately thin: the capture
/// (`crate::phase1::capture_selected_folder`) performs the policy preflight and the one listing;
/// the ingest (`crate::phase1::ingest_folder_snapshot`) performs the DB work on the snapshot
/// produced. SC-06's walker skips the capture half by handing its own authoritative captured
/// folder to the ingest core instead — that is the whole point of the split, and nothing
/// about either half's behavior changes because of it. No controller, orchestration, or
/// traversal lives here.
///
/// # Errors
/// [`Phase1Error::TargetRejected`] from the preflight — which runs before any `.ndignore`
/// read, any listing, and any DB work, so a rejected target provably writes nothing — plus
/// every ingest arm. Nothing is committed on any error path, and `CommittedFolderOutcome`
/// warnings are published only after a commit.
///
/// # Panics
/// Only on a `library_id` too large for the mapping DTO's `i32` (`MediaFile.LibraryID`
/// is Go's `int`); callers pass a 1-based SQLite autoincrement key.
pub fn scan_folder(
    store: &SQLStore,
    job: &FolderJob<'_>,
    token: &CancelToken,
) -> Result<Phase1FolderOutcome, Phase1Error> {
    // `phase_1_folders.go:226-228`: the abort check precedes everything else for this
    // folder, including the capture, so a cancelled scan touches neither the filesystem nor
    // the DB.
    if token.is_cancelled() {
        return Err(Phase1Error::Cancelled {
            folder: display_path(&job.folder_path),
        });
    }
    let captured = capture_selected_folder(job, token)?;
    ingest_folder_snapshot(store, job, &captured, token)
}
/// Which folders of one library this scan is responsible for.
///
/// Go's `scanJob.targetFolders` (`phase_1_folders.go:58`), reached from
/// `scanState.targets[lib.ID]` (`scanner/scanner.go:78-105`). Two things consume it:
/// [`nd_db::folder_repo::FolderRepo::get_folder_update_info`] — Go's
/// `GetFolderUpdateInfo(ctx, lib, targetFolders...)` (`:66`), whose batch arm scopes the
/// update-info map to the targets plus their descendants
/// (`persistence/folder_repository.go:144-175`) — and the walker's own target list
/// (`walk_dir_tree.go:29-33`, ported by [`crate::contract::WalkRequest::validated_targets`]).
///
/// **Selective-target ancestor retention and overlap dedupe are NOT this slice**
/// (`decisions.md` D-SC06-SLICE): the target list is passed through as given, and Go's
/// "walk the same overlap twice" behavior is preserved by not deduping here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanScope {
    /// The whole library: Go's empty `targetFolders`, which makes `getFolderUpdateInfo`
    /// run its all-folders statement and the walker start at each library root.
    WholeLibrary,
    /// Selected library-relative logical paths plus their descendants. Go's selective
    /// scan (`scanner.go:78-105`); paths are the slash form Go stores, so `"."` means the
    /// library root just as it does in Go's `getFolderUpdateInfoBatch`.
    Targets(Vec<String>),
}

impl ScanScope {
    /// The target paths to hand `get_folder_update_info` and the walker. Empty is Go's
    /// all-folders arm (`folder_repo.rs:1403-1412` treats an empty list, `""` and `"."`
    /// identically).
    #[must_use]
    pub fn targets(&self) -> &[String] {
        match self {
            ScanScope::WholeLibrary => &[],
            ScanScope::Targets(paths) => paths.as_slice(),
        }
    }
}

/// What one library scan needs, everything injected — the port of Go's `scanJob`
/// (`phase_1_folders.go:54-62`) minus the legs this slice defers.
///
/// `full_scan_in_progress` is Go's `lib.FullScanInProgress = lib.FullScanInProgress ||
/// fullScan` (`:97`), i.e. the value AFTER job creation, which `isOutdated` (`folder_entry.go:73`)
/// and the import filter's `FullScanInProgress` arm (`:253`) both read. The library row already
/// carries it (`library_repo.rs:156`), so [`LibraryScan::from_library_row`] derives the field
/// instead of guessing it (D-SC06's B5 withdrawal: `LibraryRepo::get` is `pub` and its record
/// carries the scan/PID fields).
///
/// **Deferred, deliberately:** PID-config-change promotion to a full scan
/// (`lib.NeedsPIDRescan()`, `:83-91`) is NOT applied here — a rescan-requested scan stays
/// quick unless the caller sets the flag. That promotion is a later lane (`D-SC06-SLICE`).
#[derive(Debug, Clone)]
pub struct LibraryScan<'a> {
    /// `library.id`, the scope every folder batch leg carries.
    pub library_id: i64,
    /// The configured library root (`lib.Path`): trusted configuration, same as
    /// [`FolderJob::library_root`]. Owned here because [`LibraryScan::from_library_row`]
    /// reads it out of the library row it also consumes.
    pub library_root: OsString,
    /// Display name for diagnostics only (Go's `job.lib.Name` in its log lines).
    pub library_name: String,
    /// One caller-chosen instant for the whole scan run, shared by every folder (see
    /// [`FolderJob::scan_stamp`]).
    pub scan_stamp: DateTime<Utc>,
    /// Go's `lib.FullScanInProgress` after job creation (`:97`).
    pub full_scan_in_progress: bool,
    /// `library.last_scan_started_at`: the instant `isOutdated`'s full-scan arm compares
    /// `updTime` against (`folder_entry.go:73`). Injected, because `ScanBegin` is out of
    /// this slice's scope; [`LibraryScan::from_library_row`] reads the stored value.
    pub last_scan_started_at: DateTime<Utc>,
    /// See [`DEFAULT_APPEND_SUBTITLE`].
    pub append_subtitle: bool,
    /// `conf.Server.Scanner.FollowSymlinks`.
    pub follow_symlinks: bool,
    /// `conf.Server.Scanner.IgnoreDotFolders`.
    pub ignore_dot_folders: bool,
    /// Mapping context, built once per scan run like Go's `sync.OnceValues` load.
    pub mapping: &'a MappingContext,
    /// The library's PID overrides (`lib.PIDAlbum`/`lib.PIDTrack`).
    pub pid: PidOptions,
    /// Go's `scanJob.prevAlbumPIDConf` (`:92-95`): `Some(scanned_pid_album)` only when it
    /// differs from the effective album spec.
    pub prev_album_pid: Option<String>,
    /// See [`FolderJob::no_birth_time`].
    pub no_birth_time: bool,
}

impl<'a> LibraryScan<'a> {
    /// A quick scan with Go's configured defaults and no PID override — the shape every
    /// change-detection test starts from.
    #[must_use]
    pub fn quick(
        library_id: i64,
        library_root: impl Into<OsString>,
        scan_stamp: DateTime<Utc>,
        last_scan_started_at: DateTime<Utc>,
        mapping: &'a MappingContext,
    ) -> LibraryScan<'a> {
        LibraryScan {
            library_id,
            library_root: library_root.into(),
            library_name: String::new(),
            scan_stamp,
            full_scan_in_progress: false,
            last_scan_started_at,
            append_subtitle: DEFAULT_APPEND_SUBTITLE,
            follow_symlinks: true,
            ignore_dot_folders: true,
            mapping,
            pid: PidOptions::default(),
            prev_album_pid: None,
            no_birth_time: false,
        }
    }

    /// Derive the scan fields from the stored library row through the existing public
    /// [`nd_db::library_repo::LibraryRepo::get`] (`library_repo.rs:1481`), which is what
    /// makes `last_scan_at`/`full_scan_in_progress`/`scanned_pid_album` executable reads
    /// rather than guesses (B5, withdrawn as a seam question in
    /// `docs/rust-port/decisions.md`).
    ///
    /// `full_scan_in_progress` becomes `row.full_scan_in_progress || full_scan_requested`
    /// exactly as Go's `:97` does, and `prev_album_pid` becomes the stored scanned album
    /// spec only when it differs from the effective one (`:93-95`). Go's
    /// `NeedsPIDRescan()` promotion (`:83-91`) is deliberately NOT applied.
    ///
    /// # Errors
    /// [`LibraryError::NotFound`] (as [`ScanReadSource::Library`]) when the row does not
    /// exist, and [`LibraryError::Sqlite`] on a statement or scan failure. Neither opens a
    /// walk: the read precedes every filesystem and write action, matching Go's
    /// `newScanJob` order (`:64-81`), whose failure means the library is skipped rather
    /// than scanned.
    pub fn from_library_row(
        store: &SQLStore,
        library_id: i64,
        scan_stamp: DateTime<Utc>,
        mapping: &'a MappingContext,
        full_scan_requested: bool,
    ) -> Result<LibraryScan<'a>, LibraryScanError> {
        // The same value-out-of-the-callback shape `read_children` uses: the repo's typed
        // error cannot ride `with_connection_sync`'s `rusqlite::Error` channel, so only a
        // pool checkout can be `Err` here.
        let (record, library_error) = store
            .with_connection_sync(|conn| {
                Ok(match LibraryRepo::new(conn).get(library_id) {
                    Ok(record) => (Some(record), None),
                    Err(source) => (None, Some(source)),
                })
            })
            .map_err(|source| LibraryScanError::LastUpdates {
                library_id,
                source: ScanReadSource::Pool(source),
            })?;
        if let Some(source) = library_error {
            return Err(LibraryScanError::LastUpdates {
                library_id,
                source: ScanReadSource::Library(source),
            });
        }
        let record = record.ok_or(LibraryScanError::LastUpdates {
            library_id,
            source: ScanReadSource::Library(LibraryError::NotFound),
        })?;
        // Go's `pid := lib.EffectivePID()` (`:82`): an empty per-library spec falls back to
        // the Go default, which is what `prevAlbumPIDConf` is compared against (`:93-95`).
        let effective = nd_mapping::config::PidOptions {
            album: record.pid_album.clone(),
            track: record.pid_track.clone(),
        }
        .effective()
        .album
        .to_string();
        // Go's `var prevAlbumPIDConf string` + `if lib.ScannedPIDAlbum != pid.Album` (`:93-95`)
        // leaves the value EMPTY when the scanned spec is empty, and the consuming arm is
        // guarded by `prevAlbumPIDConf != ""` (`:313`). The port spells that empty string as
        // `None`, so an empty scanned spec must NOT become `Some("")` — that would enable
        // the re-derivation arm with an empty spec, which Go never runs.
        let prev_album_pid = (record.scanned_pid_album != effective)
            .then_some(record.scanned_pid_album.clone())
            .filter(|spec| !spec.is_empty());
        Ok(LibraryScan {
            library_id,
            library_root: record.path.clone().into(),
            library_name: record.name,
            scan_stamp,
            full_scan_in_progress: record.full_scan_in_progress || full_scan_requested,
            last_scan_started_at: record.last_scan_started_at,
            append_subtitle: DEFAULT_APPEND_SUBTITLE,
            follow_symlinks: true,
            ignore_dot_folders: true,
            mapping,
            pid: PidOptions {
                album: record.pid_album,
                track: record.pid_track,
            },
            prev_album_pid,
            no_birth_time: false,
        })
    }

    /// The per-folder job for one walked folder: the same fields Go's `newFolderEntry`
    /// reads off `folder.job` for every folder of the library, with only the path varying.
    #[must_use]
    pub fn folder_job(&self, path: &LogicalPath) -> FolderJob<'_> {
        FolderJob {
            library_id: self.library_id,
            library_root: self.library_root.as_os_str(),
            folder_path: path.clone(),
            scan_stamp: self.scan_stamp,
            full_scan_mode: self.full_scan_in_progress,
            append_subtitle: self.append_subtitle,
            follow_symlinks: self.follow_symlinks,
            ignore_dot_folders: self.ignore_dot_folders,
            mapping: self.mapping,
            pid: self.pid.clone(),
            prev_album_pid: self.prev_album_pid.as_deref(),
            no_birth_time: self.no_birth_time,
        }
    }

    /// Go's `isOutdated` (`folder_entry.go:72-77`) for one emitted folder: the full-scan
    /// arm compares the stored `updated_at` against `last_scan_started_at`, otherwise the
    /// stored hash is compared with the walker's own fingerprint. `None` info is Go's
    /// zero-value `FolderUpdateInfo` (`:117` of `phase_1_folders.go` returns the map's
    /// zero value for an absent key), whose `UpdatedAt` is the year-one instant and whose
    /// `Hash` is empty.
    #[must_use]
    pub fn is_outdated(&self, info: Option<&FolderUpdateInfo>, hash: &str) -> bool {
        let stored_updated_at = info
            .map(|info| info.updated_at)
            .unwrap_or_else(nd_db::time::zero);
        if self.full_scan_in_progress && stored_updated_at < self.last_scan_started_at {
            return true;
        }
        info.map(|info| info.hash.as_str()).unwrap_or("") != hash
    }

    /// Go's `folderEntry.isNew` (`folder_entry.go:68-70`): a zero `updTime`, i.e. no
    /// stored row for this folder id. `nd_db::time::zero` is that zero rendered as a
    /// `DateTime<Utc>`.
    #[must_use]
    pub fn is_new(info: Option<&FolderUpdateInfo>) -> bool {
        info.map(|info| info.updated_at)
            .unwrap_or_else(nd_db::time::zero)
            == nd_db::time::zero()
    }

    /// Go's `model.FolderID(lib, path)` for a walked folder — the same hash
    /// [`FolderJob::folder_id`] computes, and the key of the update-info map (which
    /// `folder_repo.rs:1384-1390` records is keyed by FOLDER ID, never by path).
    #[must_use]
    pub fn folder_id(&self, path: &LogicalPath) -> String {
        nd_db::folder_repo::folder_id(self.library_id, "", &display_path(path))
    }
}

/// Why a library scan stopped before every folder was ingested. Each arm names the Go
/// mechanism it ports, so a caller branches on the cause rather than on message text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScanStop {
    /// Go's `stopWalk(err)` (`:358`), called only by `persistChanges`' own failure: the
    /// folder transaction failed, the walk context is cancelled, later folders are not
    /// processed, and Go's `finalize` (`:524-526`) returns early so no unvisited folder is
    /// marked missing. This slice has no finalizer, so the residue is only reported
    /// ([`LibraryScanOutcome::folders_unvisited`]).
    #[error("Scanner: Error persisting changes to DB, folder '{folder}': {source}")]
    Persist {
        /// Logical folder path whose persist failed.
        folder: String,
        /// DB-14 run failure.
        #[source]
        source: FolderRunError,
    },
    /// Cancellation was requested (`processFolder`'s `context.Cause(p.walkCtx)` check
    /// at `:226-228`, plus the producer's `utils.IsCtxDone` at `:162`). No write happens
    /// for the folder that observed it.
    #[error("scan cancelled")]
    Cancelled,
    /// The walk itself ended with a fatal error. Go logs
    /// `Scanner: Error scanning library` and keeps the phase running (`:167-169`), so this
    /// is reported, not returned as a failure of the whole scan.
    #[error("walk ended fatally: {source}")]
    Walk {
        /// The walker's typed failure.
        #[source]
        source: ScanError,
    },
}

/// What one library scan did, folder by folder.
///
/// The counters mirror Go's `producer()` bookkeeping (`:159-205`): `folders_visited` is
/// `job.numFolders` (every folder the walk emitted, changed or not), `folders_changed` is
/// `totalChanged`, and `folders_persisted` counts folders that reached `persistChanges`.
/// Nothing here claims the phase completed: [`LibraryScanOutcome::folders_unvisited`] is
/// exactly the input Go's finalizer would use, and no finalizer runs
/// (`decisions.md` D-SC06-FINALIZE).
#[derive(Debug)]
pub struct LibraryScanOutcome {
    /// Folders the walk emitted (`job.numFolders.Add(1)`, `:171`).
    pub folders_visited: usize,
    /// Folders `isOutdated` reported as changed (`totalChanged`, `:196`).
    pub folders_changed: usize,
    /// Folders ingested and committed (`:199-201`'s else arm is skipped instead).
    pub folders_persisted: usize,
    /// Up-to-date folders skipped (`:199-201`).
    pub folders_skipped_up_to_date: usize,
    /// New folders with no files and no subfolders, skipped in quick mode (`:187-193`).
    pub folders_skipped_new_empty: usize,
    /// Every committed folder's ingest outcome, in walk (postorder) order.
    pub reports: Vec<Phase1FolderOutcome>,
    /// Folder-level aborts: Go's stage error, which drops that folder and fails the phase
    /// without stopping the walk (`:234-243`, `pipeline.go:524-537`). A non-empty list means
    /// the caller must treat the library as failed — `markFailed` itself is a later lane.
    pub failures: Vec<Phase1Error>,
    /// Set when the scan stopped early. [`ScanStop::Persist`] is Go's `stopWalk` (`:358`).
    pub stopped: Option<ScanStop>,
    /// Update-info entries never popped by a visited folder — Go's `job.lastUpdates`
    /// residue at `finalize` (`:530-533`), which is what the deferred finalizer would mark
    /// missing. Reported so the deferral is visible rather than silent.
    pub folders_unvisited: usize,
    /// Those folder ids, sorted. Empty in the common case where the walk covered the tree.
    pub unvisited_folder_ids: Vec<String>,
    /// Targets the walk completed.
    pub targets_walked: usize,
    /// Targets the walk skipped (missing, or symlinked with following disabled). Go's
    /// skipped target is a warning, not a failed library (`decisions.md` D-SC05-SLICE item 1).
    pub targets_skipped: usize,
    /// The walker's own warnings, in emission order.
    pub warnings: Vec<ScanWarning>,
}

impl LibraryScanOutcome {
    /// Whether the caller should treat the library as failed: a folder abort, a persist
    /// failure that stopped the walk, or a fatal walk outcome. Go's equivalent signal is
    /// `scanState.failedLibs` (`scanner.go:50-56`), which this slice does not own.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        !self.failures.is_empty() || self.stopped.is_some()
    }

    /// The ingest report for one logical folder path, when it was committed.
    #[must_use]
    pub fn report_of(&self, path: &str) -> Option<&Phase1FolderOutcome> {
        self.reports
            .iter()
            .find(|report| report.folder_path == path)
    }
}

/// Which leg of a pre-walk DB read failed.
///
/// Go has one arm for each (`phase_1_folders.go:66-69` for the update-info read and
/// `:82-81`/`library_repository.go:35-40` for the row read), so both callers report
/// through this enum. Keeping the legs typed distinguishes "cannot lease a connection"
/// from "a stored row cannot be decoded", the same split [`ChildrenReadSource`] makes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScanReadSource {
    /// The update-info statement, bind, execution, or `image_files` decode failure
    /// (`folder_repo.rs:1386-1397`: a bad cell aborts the whole map, exactly as Go's
    /// `return nil, err` hands back a nil map).
    #[error(transparent)]
    Folder(#[from] FolderError),
    /// The library-row read (`LibraryRepo::get`), including
    /// [`LibraryError::NotFound`] — Go's `model.ErrNotFound`, which means the library has
    /// no row to build a job from.
    #[error(transparent)]
    Library(#[from] LibraryError),
    /// Pool checkout failure, before any statement was prepared.
    #[error("cannot lease a pooled connection for the scan read: {0}")]
    Pool(#[source] StoreError),
}

/// Failure of the parts of a library scan that run before any folder is walked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryScanError {
    /// Go's `newScanJob` failure (`:66-69`): the update-info read failed, so the library
    /// gets no job at all. Go logs `Scanner: Error creating scan context`, sends the error
    /// as progress, calls `markFailed(lib.ID)` and skips the job (`:40-45`); it writes
    /// nothing, and neither does this.
    #[error("Scanner: Error creating scan context for library {library_id}: {source}")]
    LastUpdates {
        /// Library whose job creation failed.
        library_id: i64,
        /// Folder-repo failure, or the pool checkout that preceded it.
        #[source]
        source: ScanReadSource,
    },
    /// A selective target is not a library-relative logical path. Go accepts any string
    /// here because its paths come from the caller's `ScanTarget`; the port refuses a
    /// malformed one rather than walking something the caller did not name.
    #[error("selective scan target '{path}' is not a library-relative logical path: {source}")]
    InvalidTarget {
        /// Target as given.
        path: String,
        /// Storage-level cause.
        #[source]
        source: StorageError,
    },
}

/// Scan one library: walk its tree postorder, ingest every changed folder, and persist
/// each folder through its own DB-14 transaction.
///
/// This is `phaseFolders` (`phase_1_folders.go:142-222`) with Go's pipeline stages run in
/// order on the consumer side instead of as concurrent goroutines: `producer` emits in
/// postorder, and `processFolder` + `persistChanges` are the same ingest core
/// `ingest_folder_snapshot` already uses for one folder. `Concurrency(1)` for persist is
/// Go's own constraint (`:219`), so the single-threaded shape is not a simplification.
///
/// Per folder, in Go's order:
/// 1. pop the update-info entry (`popLastUpdate`, `:113-120`) — this happens when the walk
///    emits the folder, which is when Go's `createFolderEntry` pops it (`loadDir:118`);
/// 2. `isOutdated` (`folder_entry.go:72-77`) against that entry and the walker's hash;
/// 3. the quick-mode skip of a new folder with no files and no subfolders (`:187-193`);
/// 4. the ingest: children read, Go's import-selection arms (`:247-262`), tag/bridge,
///    drafts, `albumIDMap` transitions (`:309-321`), missing leg (`:266-270`);
/// 5. persist one folder transaction (`persist_folder_delta`, DB-14), missing-marking and
///    `Touch` included in that same transaction (`:444-460`).
///
/// A persist failure cancels the walk ([`crate::walk::WalkHandle::cancel`], Go's
/// `stopWalk(err)` at `:358`) and is reported as [`ScanStop::Persist`]. Folders the walk
/// never reached keep their stored rows: no finalizer runs in this slice.
///
/// # Errors
/// [`LibraryScanError::LastUpdates`] when the update-info read fails (Go: the library gets
/// no job), and [`LibraryScanError::InvalidTarget`] for a malformed selective target. Both
/// happen before any folder is walked, so neither writes anything. A fatal walk outcome is
/// reported as [`ScanStop::Walk`] rather than an error, because Go logs it as a warning and
/// keeps the phase running (`:167-169`).
///
/// # Panics
/// Only through `ingest_folder_snapshot` on a `library_id` too large for the mapping
/// DTO's `i32`.
pub async fn scan_library(
    store: &SQLStore,
    scan: &LibraryScan<'_>,
    scope: &ScanScope,
    token: &CancelToken,
) -> Result<LibraryScanOutcome, LibraryScanError> {
    let targets = scope.targets();

    // `:66-69`: the update-info map is read BEFORE anything is walked. Keyed by folder id
    // over non-missing rows; the batch arm scopes it to targets plus descendants.
    // `with_connection_sync`'s callback must return `Result<_, rusqlite::Error>`, so the
    // repo's typed error travels out as a VALUE (the same shape `read_children` uses) and
    // the only `Err` this callback can produce is the pool checkout.
    let (mut last_updates, folder_error) = store
        .with_connection_sync(|conn| {
            Ok(
                match FolderRepo::new(conn).get_folder_update_info(scan.library_id, targets) {
                    Ok(map) => (map, None),
                    Err(source) => (BTreeMap::new(), Some(source)),
                },
            )
        })
        .map_err(|source| LibraryScanError::LastUpdates {
            library_id: scan.library_id,
            source: ScanReadSource::Pool(source),
        })?;
    if let Some(source) = folder_error {
        return Err(LibraryScanError::LastUpdates {
            library_id: scan.library_id,
            source: ScanReadSource::Folder(source),
        });
    }

    let mut request = WalkRequest {
        libraries: vec![LibraryRoot {
            id: scan.library_id,
            name: scan.library_name.clone(),
            root: scan.library_root.to_os_string(),
        }],
        targets: Vec::new(),
        follow_symlinks: scan.follow_symlinks,
        ignore_dot_folders: scan.ignore_dot_folders,
    };
    for path in targets {
        let parsed =
            LogicalPath::parse(path).map_err(|source| LibraryScanError::InvalidTarget {
                path: path.clone(),
                source,
            })?;
        request.targets.push(WalkTarget {
            library_id: scan.library_id,
            path: parsed,
        });
    }

    let mut handle = WalkHandle::start(request, token.clone());
    let mut outcome = LibraryScanOutcome {
        folders_visited: 0,
        folders_changed: 0,
        folders_persisted: 0,
        folders_skipped_up_to_date: 0,
        folders_skipped_new_empty: 0,
        reports: Vec::new(),
        failures: Vec::new(),
        stopped: None,
        folders_unvisited: last_updates.len(),
        unvisited_folder_ids: last_updates.keys().cloned().collect(),
        targets_walked: 0,
        targets_skipped: 0,
        warnings: Vec::new(),
    };

    while let Some(event) = handle.next().await {
        outcome.folders_visited += 1;
        // Go's `createFolderEntry` (`:125-129`) pops at emit time, so a folder that exists
        // on disk leaves the finalizer's residue even when it is skipped as up-to-date.
        let info = last_updates.remove(&scan.folder_id(&event.snapshot.path));

        if !scan.is_outdated(info.as_ref(), &event.hash) {
            outcome.folders_skipped_up_to_date += 1;
            continue;
        }

        // `:186-195`: the new-empty-folder skip runs only when no full scan is in progress,
        // and ancestors of a changed subtree are kept even when empty. It `continue`s BEFORE
        // `totalChanged++` (`:202`), so a skipped folder is not counted as changed.
        if !scan.full_scan_in_progress
            && event.snapshot.is_empty()
            && LibraryScan::is_new(info.as_ref())
        {
            outcome.folders_skipped_new_empty += 1;
            continue;
        }
        outcome.folders_changed += 1;

        if token.is_cancelled() {
            outcome.stopped = Some(ScanStop::Cancelled);
            break;
        }

        let job = scan.folder_job(&event.snapshot.path);
        let captured = CapturedFolder {
            snapshot: event.snapshot.clone(),
            hash: event.hash.clone(),
        };
        match ingest_folder_snapshot(store, &job, &captured, token) {
            Ok(report) => {
                outcome.folders_persisted += 1;
                outcome.reports.push(report);
            }
            Err(error) => {
                if matches!(error, Phase1Error::Persist { .. }) {
                    // `:356-360`: log, `stopWalk(err)`, fail the phase. Nothing later is
                    // processed, so the remaining update-info entries stay residue.
                    let folder = error_folder(&error);
                    handle.cancel();
                    outcome.stopped = match error {
                        Phase1Error::Persist { source, .. } => {
                            Some(ScanStop::Persist { folder, source })
                        }
                        other => {
                            outcome.failures.push(other);
                            Some(ScanStop::Cancelled)
                        }
                    };
                    break;
                }
                // Go's stage error (`:234-243`): that folder is dropped and never
                // persisted, but `stopWalk` is NOT called, so the walk continues.
                outcome.failures.push(error);
            }
        }
    }

    let summary = handle.terminal().await;
    outcome.targets_walked = summary.targets_walked;
    outcome.targets_skipped = summary.targets_skipped;
    outcome.warnings = summary.warnings;
    if outcome.stopped.is_none() && !summary.outcome.is_complete() {
        if summary.outcome.is_cancelled() {
            outcome.stopped = Some(ScanStop::Cancelled);
        } else if let WalkOutcome::Failed(source) = summary.outcome {
            // Go logs this and keeps the phase running (`:167-169`).
            outcome.stopped = Some(ScanStop::Walk { source });
        }
    }
    // A cancelled or failed phase is exactly the case Go's `finalize` returns early on
    // (`:524-526`), so the residue below is reported WITHOUT mutation: no unvisited folder
    // is marked missing in this slice (D-SC06-FINALIZE).
    outcome.folders_unvisited = last_updates.len();
    outcome.unvisited_folder_ids = last_updates.keys().cloned().collect();
    Ok(outcome)
}

/// The folder a [`Phase1Error`] names, for the stop-the-walk report. Every arm that can
/// reach the controller carries its own logical path.
#[must_use]
fn error_folder(error: &Phase1Error) -> String {
    match error {
        Phase1Error::Cancelled { folder }
        | Phase1Error::TargetRejected { folder, .. }
        | Phase1Error::SnapshotMismatch { folder, .. }
        | Phase1Error::Hash { folder, .. }
        | Phase1Error::ChildrenRead { folder, .. }
        | Phase1Error::EntryInfo { folder, .. }
        | Phase1Error::Projection { folder, .. }
        | Phase1Error::Persist { folder, .. } => folder.clone(),
        Phase1Error::Bridge { path, .. } => path.clone(),
        Phase1Error::Walk { folder, .. } => folder.clone(),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// The folder-level abort arm (`:274-279`) is reachable exactly when the stat of a
    /// walked entry fails after the listing succeeded — in practice a file that vanished
    /// mid-scan. Asserted here because on POSIX no permission mode produces it: a
    /// chmod-`000` file still `lstat`s and `stat`s fine, so Go's extractor (and the Rust
    /// backend) fail on OPEN and take the per-file skip arm instead. What this proves is
    /// the FORMATTING the abort branch uses — [`stat_for_mapping`] hands back the raw
    /// error and [`folder_import_abort_message`] is the only formatter that arm can
    /// reach — not that the ingest arm itself ran.
    #[test]
    fn stat_failure_is_formatted_with_go_s_folder_level_warning_text() {
        let root = std::env::temp_dir();
        let store = LocalDir::from_path(root.as_os_str()).expect("temp dir as root");
        let path = LogicalPath::parse("nd-sc05-definitely-not-here.mp3").expect("logical path");
        let entry = FolderEntry {
            name: "nd-sc05-definitely-not-here.mp3".to_string(),
            path: path.clone(),
            classification_name: "nd-sc05-definitely-not-here.mp3".to_string(),
            stat: crate::storage::FileStat {
                size: 1,
                modified: Ok(std::time::SystemTime::UNIX_EPOCH),
                created: Err(crate::storage::BirthTimeUnavailable),
            },
            endpoint: None,
            outside_root: false,
            category: crate::folder::EntryCategory::Audio,
        };
        // The raw error is preserved, not pre-stringified into a warning of some other
        // site's making.
        let source = stat_for_mapping(&store, &entry)
            .expect_err("a path that does not exist cannot be stat'd");
        let folder_text = display_path(&LogicalPath::parse("album/one").expect("logical path"));
        let message = folder_import_abort_message(&folder_text, &source);

        // Go's `phase_1_folders.go:277` spelling, `%v` substituted.
        assert_eq!(
            message,
            format!("Error loading tags from files in {folder_text}: {source}"),
            "the folder-level warning is Go's exact text with the raw error appended"
        );
        assert!(
            message.starts_with("Error loading tags from files in album/one: "),
            "the warning names the FOLDER, got {message}"
        );
        assert!(
            !message.contains("Error getting file info"),
            "the quick-scan filter's per-file text (`:254-259`, SC-06's arm) is not this arm's wording, got {message}"
        );
    }

    /// The quick-scan filter's own failure arm (`:257-261`) has DIFFERENT wording from
    /// [`folder_import_abort_message`]: Go's `sendWarning` at `:259` is
    /// `"Error getting file info for %s/%s: %v"` with the FOLDER and `af.Name()`, where the
    /// folder-level arm at `:277` names only the folder. Asserting both spellings side by
    /// side is what keeps a future edit from routing one arm's message through the other.
    #[test]
    fn the_quick_scan_filter_failure_uses_go_s_per_file_warning_text() {
        let folder_text = display_path(&LogicalPath::parse("album/one").expect("logical path"));
        let source = crate::storage::BirthTimeUnavailable;
        let message = quick_scan_stat_message(&folder_text, "known.mp3", &source);

        assert_eq!(
            message,
            format!("Error getting file info for {folder_text}/known.mp3: {source}"),
            "Go's `:259` spelling, `%v` substituted"
        );
        assert!(
            message.starts_with("Error getting file info for album/one/known.mp3: "),
            "the warning names the FOLDER and the file's own name, got {message}"
        );
        assert!(
            !message.contains("Error loading tags from files in"),
            "the folder-level text (`:277`) is not this arm's wording, got {message}"
        );
    }

    /// snapshot is trusted input, so its library id and logical path are checked against the job
    /// BEFORE any DB work. A snapshot describing another library or another folder would
    /// otherwise persist a folder row under an id its listing does not cover, under another
    /// library's scope, with a `folder.hash` describing the wrong tree. Asserted in-crate because
    /// the branch is pure plumbing: the snapshot is altered in memory while the filesystem is
    /// untouched, so nothing but the check can fire; the no-writes half is checked against a real
    /// migrated DB.
    #[test]
    fn ingest_aborts_with_no_writes_when_the_snapshot_identity_is_wrong() {
        use nd_db::pool::{Pool, PoolConfig};

        let db_path = std::path::PathBuf::from(nd_utils::files::temp_file_name(
            "nd-sc05-snapshot-mismatch-",
            ".db",
        ));
        let (pool, first) = Pool::new(&PoolConfig::with_size(
            format!("file:{}?mode=rwc&_foreign_keys=on", db_path.display()),
            1,
        ))
        .expect("open a pooled DB for the mismatch test");
        nd_dbschema::schema::apply_fresh_schema(&first).expect("fresh schema");
        pool.put_back(first).expect("return the initializer handle");
        let store = nd_db::store::SQLStore::new(pool);

        let mapping =
            nd_mapping::config::MappingContext::new(nd_mapping::config::MappingOptions::default());
        let root = std::env::temp_dir();
        let job = FolderJob {
            library_id: 7,
            library_root: root.as_os_str(),
            folder_path: LogicalPath::parse("album/one").expect("logical path"),
            scan_stamp: chrono::DateTime::from_timestamp(1_751_000_000, 0).expect("scan stamp"),
            full_scan_mode: true,
            append_subtitle: DEFAULT_APPEND_SUBTITLE,
            follow_symlinks: true,
            ignore_dot_folders: true,
            mapping: &mapping,
            pid: PidOptions::default(),
            prev_album_pid: None,
            no_birth_time: false,
        };

        let dir_stat = crate::storage::FileStat {
            size: 4_096,
            modified: Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400)),
            created: Err(crate::storage::BirthTimeUnavailable),
        };
        let captured = |library_id: i64, path: LogicalPath| {
            let snapshot = FolderSnapshot {
                library_id,
                path,
                dir_stat: dir_stat.clone(),
                mod_time: dir_stat.modified.expect("pinned mtime"),
                images_updated_at: None,
                num_sub_folders: 0,
                entries: Vec::new(),
                coverage: FolderCoverage::Complete,
                warnings: Vec::new(),
            };
            let hash = snapshot.hash().expect("fingerprint the altered snapshot");
            CapturedFolder { snapshot, hash }
        };

        let count = |table: &str| {
            store
                .with_connection_sync(|conn| {
                    nd_db::execution::query_optional(
                        conn,
                        &(format!("SELECT COUNT(*) FROM {table}"), Vec::new()),
                        |row| row.get::<_, i64>(0),
                    )
                })
                .unwrap_or_else(|e| panic!("count {table}: {e}"))
                .unwrap_or_else(|| panic!("count {table} returned no row"))
        };
        let tables = [
            "folder",
            "media_file",
            "album",
            "artist",
            "tag",
            "artwork_queue",
        ];
        for table in tables {
            assert_eq!(
                count(table),
                0,
                "the migrated DB starts with no {table} row"
            );
        }

        // Wrong library: the same path, another library's scope.
        let wrong_library = captured(8, job.folder_path.clone());
        let error = ingest_folder_snapshot(&store, &job, &wrong_library, &CancelToken::new())
            .expect_err("a snapshot from another library must not be ingested");
        match &error {
            Phase1Error::SnapshotMismatch {
                folder,
                expected_library_id,
                captured_library_id,
                expected_path,
                captured_path,
            } => {
                assert_eq!(folder, "album/one");
                assert_eq!(*expected_library_id, 7);
                assert_eq!(*captured_library_id, 8);
                assert_eq!(expected_path, "album/one");
                assert_eq!(captured_path, "album/one");
            }
            other => panic!("expected SnapshotMismatch, got {other:?}"),
        }

        // Wrong folder: the right library, a sibling's listing.
        let wrong_path = captured(7, LogicalPath::parse("album/two").expect("logical path"));
        let error = ingest_folder_snapshot(&store, &job, &wrong_path, &CancelToken::new())
            .expect_err("a snapshot of another folder must not be ingested");
        match &error {
            Phase1Error::SnapshotMismatch {
                expected_library_id,
                captured_library_id,
                expected_path,
                captured_path,
                ..
            } => {
                assert_eq!(
                    *expected_library_id, *captured_library_id,
                    "library matches"
                );
                assert_eq!(expected_path, "album/one");
                assert_eq!(captured_path, "album/two");
            }
            other => panic!("expected SnapshotMismatch, got {other:?}"),
        }

        // Nothing was written, and no DB work happened at all: the check precedes the children
        // read, so the folder table stays empty and the pool is neither poisoned nor leaking a
        // lease.
        for table in tables {
            assert_eq!(
                count(table),
                0,
                "{table}: a mismatched snapshot wrote nothing"
            );
        }
        assert_eq!(
            store.idle_count().expect("idle count"),
            1,
            "the mismatch opened no connection that was never returned"
        );

        drop(store);
        let _ = std::fs::remove_file(&db_path);
    }

    /// [`Phase1Error::Hash`] is the capture's fingerprint arm. It cannot be driven through
    /// `scan_folder` on any supported filesystem: the facts it needs to fail on — an entry with
    /// no mtime, an unrenderable instant, a size past `i64::MAX` — are all aborted earlier on the
    /// listing path (`Phase1Error::EntryInfo`) or are not producible by a real file. The arm
    /// exists for the split's other caller, where SC-06 hands [`ingest_folder_snapshot`] a
    /// snapshot it captured itself, so it is executed on that shape: a snapshot with a real
    /// fingerprintable first entry plus one entry whose mtime the platform never reported.
    #[test]
    fn an_unfingerprintable_snapshot_is_the_typed_hash_arm_and_names_the_folder() {
        let folder = "album/one";
        let path = LogicalPath::parse(folder).expect("logical path");
        let dir_stat = FileStat {
            size: 4_096,
            modified: Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400)),
            created: Err(crate::storage::BirthTimeUnavailable),
        };
        let entry =
            |name: &str,
             modified: Result<std::time::SystemTime, crate::storage::StatUnavailable>| {
                FolderEntry {
                    name: name.to_string(),
                    path: path.join(std::ffi::OsStr::new(name)).expect("entry path"),
                    classification_name: name.to_string(),
                    stat: FileStat {
                        size: 1_024,
                        modified,
                        created: Err(crate::storage::BirthTimeUnavailable),
                    },
                    endpoint: None,
                    outside_root: false,
                    category: crate::folder::EntryCategory::Audio,
                }
            };
        let snapshot = FolderSnapshot {
            library_id: 1,
            path: path.clone(),
            dir_stat: dir_stat.clone(),
            mod_time: dir_stat.modified.expect("pinned mtime"),
            images_updated_at: None,
            num_sub_folders: 0,
            entries: vec![
                // A fingerprintable entry FIRST, so the refusal is a mid-preimage failure
                // rather than a trivially empty snapshot.
                entry(
                    "01.mp3",
                    Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400)),
                ),
                entry("02.mp3", Err(crate::storage::StatUnavailable)),
            ],
            coverage: FolderCoverage::Complete,
            warnings: Vec::new(),
        };

        // Non-vacuity: the same snapshot WITHOUT the unstatable entry fingerprints fine, so
        // the error below comes from that entry's missing mtime and nothing else.
        let mut fingerprintable = snapshot.clone();
        fingerprintable.entries.truncate(1);
        assert!(
            fingerprint(&fingerprintable, folder).is_ok(),
            "the first entry alone is fingerprintable, so the failure below is the second entry's"
        );

        let error = fingerprint(&snapshot, folder)
            .expect_err("an entry with no captured mtime cannot be hashed over");
        match error {
            Phase1Error::Hash {
                folder: got,
                source,
            } => {
                assert_eq!(got, folder, "the error names the folder");
                assert_eq!(
                    source,
                    FolderSnapshotError::TimestampUnavailable {
                        path: path
                            .join(std::ffi::OsStr::new("02.mp3"))
                            .expect("entry path"),
                    },
                    "the wrapped cause is the fingerprint's own typed error, naming the entry"
                );
            }
            other => panic!("expected Hash, got {other:?}"),
        }
    }

    /// [`Phase1Error::Bridge`] as the ingest reaches it: the storage-fact conversion in
    /// [`track_stat_for_mapping`], which is the arm a file whose `stat` cannot be projected
    /// takes before any tag read. A real file on a 64-bit host cannot produce either failure
    /// (`u64::MAX` is not a storable size and no supported filesystem reports an mtime outside
    /// chrono's range), so this pins the ARM — typed error, path named, cause retained — and
    /// the coverage limit, rather than implying the end-to-end path ran.
    #[test]
    fn an_unprojectable_storage_stat_is_the_typed_bridge_arm() {
        let path = "album/one/01.mp3";
        let stat = |size: u64, modified: std::time::SystemTime| FileStat {
            size,
            modified: Ok(modified),
            created: Err(crate::storage::BirthTimeUnavailable),
        };
        let in_range = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_750_996_400);

        // The ordinary case, asserted so the failure below is not a constant.
        assert!(
            track_stat_for_mapping(&stat(1_024, in_range), path).is_ok(),
            "an ordinary file's stat projects"
        );

        let error = track_stat_for_mapping(&stat(u64::MAX, in_range), path)
            .expect_err("a size past i64::MAX cannot be a record column");
        match error {
            Phase1Error::Bridge { path: got, source } => {
                assert_eq!(got, path, "the error names the offending file");
                match source {
                    BridgeError::SizeOverflow { value } => assert_eq!(
                        value,
                        u64::MAX,
                        "the cause is the bridge's own typed error, carrying the raw size"
                    ),
                    other => panic!("expected SizeOverflow, got {other:?}"),
                }
            }
            other => panic!("expected Bridge, got {other:?}"),
        }
    }
}
