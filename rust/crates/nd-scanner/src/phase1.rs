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
//! ## Deliberately NOT here (SC-06 owns each of these)
//!
//! * **Folder outdated/empty-skip policy** (`:186-201`, `folder_entry.go:60-77`): no
//!   `isOutdated`/`isNew`/`prevHash` comparison, no "skip new empty folder".
//!   `ingest_folder_snapshot` (crate-private; reached through [`scan_folder`]) ingests whatever
//!   captured snapshot it is handed, so the vertical is testable without phase-1's change
//!   detection.
//!   absent from the DB, `FullScanInProgress`, the file mtime is after the child's
//!   `updated_at`, or the child is `missing`. Here every on-disk audio file is
//!   imported — the full-scan arm of the same code, with `filesToImport` = everything
//!   — and the `modTime > UpdatedAt` comparison is left unimplemented rather than
//!   approximated.
//! * **Quick-scan `prevHash` bookkeeping** (`folder_entry.go:60-77`): the previous
//!   fingerprint is never stored or compared here.
//!
//! Ported but UNWIRED until SC-06 supplies it: [`FolderJob::prev_album_pid`] is Go's
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
//! ## Capture and ingest are two halves (D-SC05-SLICE item 4)
//!
//! `capture_selected_folder` (crate-private) is the filesystem half: it preflights the selected
//! target, captures ONE folder's listing into a [`FolderSnapshot`], and fingerprints it — no DB
//! work at all. `ingest_folder_snapshot` (crate-private) is the persistence half and takes that
//! snapshot as trusted input, so SC-06's walker can later hand it its own authoritative
//! captured folder instead of making this lane rescan. Both are crate-private; the only
//! public entry is [`scan_folder`], which runs one then the other. No controller,
//! orchestration, or traversal lives here.
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
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use nd_db::album_repo::AlbumRecord;
use nd_db::artist_repo::ArtistRecord;
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
use crate::contract::{name_text, CancelToken, ScanWarning, ScanWarningKind};
use crate::folder::{newest, FolderCoverage, FolderEntry, FolderSnapshot, FolderSnapshotError};
use crate::ignore::{ignore_file_name, IgnoreFileOutcome, IgnoreStack};
use crate::mapping_bridge::{self, BridgeError, TrackStat};
use crate::projection::{self, ProjectionError};
use crate::storage::{
    EndpointKind, EntryKind, FileStat, Follow, LocalDir, LogicalPath, ResolvedTarget, StorageError,
};

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
    /// Go's extractor skips and the batch-abort arm, in emission order.
    failures: Vec<TagFailure>,
    /// `true` when Go's `:274-279` arm fired: no tracks, albums, or artists — but the
    /// folder row is still persisted.
    import_aborted: bool,
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

        let previous = children
            .iter()
            .find(|(stored, _)| *stored == &entry.path.to_string())
            .map(|(_, record)| record.clone());
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
/// wrap (`:277`'s `%v`). The failure TEXT is not formed here: this lane always imports
/// every file, so a stat failure always surfaces at `processFolder`'s folder-level arm
/// (`:274-279`), and [`folder_import_abort_message`] is the only place that spelling
/// exists. Go's quick-scan filter text (`:254-259`, `"Error getting file info for
/// %s/%s: %v"`) belongs to SC-06's filter, not to this stat, and must not be emitted here.
fn stat_for_mapping(store: &LocalDir, entry: &FolderEntry) -> Result<FileStat, StorageError> {
    if let Some(endpoint) = entry.endpoint.clone() {
        return Ok(endpoint);
    }
    store
        .snapshot(&entry.path, Follow::Yes)
        .map(|snapshot| snapshot.stat)
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
    //    the DB map regardless of whether it was imported.
    let on_disk: BTreeMap<String, ()> = snapshot
        .category(crate::folder::EntryCategory::Audio)
        .into_iter()
        .map(|entry| (entry.path.to_string(), ()))
        .collect();
    let missing = missing_leg(&children, &on_disk);

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

    /// The trust boundary of [`ingest_folder_snapshot`] (D-SC05-SLICE item 4): the captured
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
