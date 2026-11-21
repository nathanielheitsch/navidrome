//! DB-14 — the final folder-delta persistence transaction: `scanner/phase_1_folders.go`
//! `persistFolder` (`:374-475`) plus `persistAlbum` (`:478-506`) EXECUTED as one per-folder
//! transaction under `persistence/persistence.go:207-228` `WithTxRetry`.
//!
//! `D-BATCH` (`docs/rust-port/decisions.md`, "Folder-write batch contract") is the binding
//! decision, so nothing here re-decides it: phase 1 persists **exactly one transaction per
//! folder** (`persistChanges` wraps `persistFolder` in `WithTxRetry`, `:353-357`; the persist
//! stage runs `Concurrency(1)`, `:218-219`), DB opens that transaction and owns busy retry,
//! and `persist_folder_delta_in_tx` is `pub(crate)` — SC consumes the batch types and may
//! widen the in-tx function later, which is a one-line change rather than a second write
//! surface.
//!
//! Retry purity is structural, not conventional: [`FolderWriteBatch`] holds no `Cell`, no
//! clock, no filesystem handle and no raw SQL; the album-ID transitions are cloned per
//! attempt exactly like Go's `maps.Clone(entry.albumIDMap)` (`:378`), and every timestamp is
//! a caller-stamped value captured when SC built the batch ([`FolderStamps`], one field per
//! distinct Go `time.Now()` site). The closure touches no second checkout —
//! [`SQLStore::try_with_tx_retry_sync`] (`store.rs:429`) holds ONE lease across all attempts.
//!
//! ID stability: folder/artist/album ids are SC's and empty ones are refused before checkout
//! ([`ScopeRejection`], beside the `EmptyId` refusals in `folder_repo`/`album_repo`/
//! `artist_repo`). The only id minted inside the transaction is Go's `putByMatch` random arm
//! for media (`sql_base_repository.go:577-589`, `:635-638`), and its value is taken from the
//! COMMITTED [`crate::mediafile_repo::MediaFileWriteResult`] — an aborted attempt publishes
//! nothing.
//!
//! Warn-vs-fatal copies Go's own table (a bare `return fmt.Errorf` is fatal, a `log.Warn` is
//! not): fatal = Folder Put `:390`, Tag Add `:396`, Artist Put `:403`, Library AddArtist
//! `:408`, Album Put `:481`, MediaFile Put `:430`, MarkMissing `:446`, Album Touch `:456`;
//! warn-only, transaction still commits = media artwork invalidation `:439`, enqueue `:470`,
//! `ReassignAnnotation` `:490`, and `CopyAttributes` `:496` whose source-not-found case is
//! SILENTLY ignored (`:497-500`). Commit and rollback failures are always fatal and belong to
//! the executor ([`DbRuntimeError::Sqlite`] with
//! [`crate::TransactionStage::Commit`],
//! [`DbRuntimeError::RollbackFailed`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::album_repo::{
    copy_album_created_at_in_tx, put_album_in_tx, touch_albums_in_tx, AlbumRecord,
    CopyAlbumCreatedAtOutcome,
};
use crate::artist_repo::{put_artist_in_tx, ArtistColumn, ArtistRecord};
use crate::artwork_queue_repo::{
    enqueue_if_missing_in_tx, enqueue_in_tx, ArtworkQueueItem, EnqueueMode,
};
use crate::artwork_repo::delete_for_items_in_tx;
use crate::folder_repo::{put_folder_in_tx, FolderRecord};
use crate::library_repo::add_artist_in_tx;
use crate::mediafile_repo::{
    mark_media_missing_in_tx, put_mediafile_in_tx, MediaFileRecord, MediaIdentity,
};
use crate::runtime_error::{DbOperationError, DbRuntimeError};
use crate::sql_annotations::{reassign_annotation_in_tx, AnnotationEntityKind};
use crate::store::SQLStore;
use crate::tag_repo::{add_in_tx, Tag};

/// `scanner/image_changes.go:107-112` `scanArtworkItem`'s priority:
/// `model.ArtworkPriorityScan` (`model/artwork.go:116`), ported as
/// [`crate::playlist_repo::ARTWORK_PRIORITY_SCAN`].
pub const SCAN_ARTWORK_PRIORITY: i64 = crate::playlist_repo::ARTWORK_PRIORITY_SCAN;

/// `scanArtworkItem`'s `ImageType` (`image_changes.go:109`, `model.ImageTypePrimary`),
/// spelled with the queue crate's own constant so the renderer's `cmp.Or` arm
/// (`artwork_queue_repo.rs:140-144`) and this lane agree.
pub const SCAN_ARTWORK_IMAGE_TYPE: &str = crate::artwork_queue_repo::IMAGE_TYPE_PRIMARY;

/// The artist names Go excludes from the scanner's artwork queue (`:412`):
/// `consts.UnknownArtist` and `consts.VariousArtists`.
pub const QUEUE_EXCLUDED_ARTIST_NAMES: [&str; 2] = [
    nd_conf::consts::UNKNOWN_ARTIST,
    nd_conf::consts::VARIOUS_ARTISTS,
];

/// The album name Go excludes from the queue (`:423`), `consts.UnknownAlbum`.
pub const QUEUE_EXCLUDED_ALBUM_NAME: &str = nd_conf::consts::UNKNOWN_ALBUM;

/// `phase_1_folders.go:403-405`, verbatim and in Go's order: the columns the folder
/// transaction's Artist Put is allowed to update. `ArtistColumn` is the typed form of Go's
/// free-form `colsToUpdate` list (`artist_repo.rs:674`), so a caller cannot name a column
/// outside the map.
pub const ARTIST_UPDATE_COLUMNS: [ArtistColumn; 7] = [
    ArtistColumn::Name,
    ArtistColumn::MbzArtistId,
    ArtistColumn::SortArtistName,
    ArtistColumn::OrderArtistName,
    ArtistColumn::FullText,
    ArtistColumn::SearchNormalized,
    ArtistColumn::UpdatedAt,
];

/// The Go arm a failure or warning belongs to — the `D-BATCH` table's row identity, so a
/// caller (SC's warning sink, the equivalent of `state.sendWarning` at `:491`/`:499`) knows
/// which arm degraded without parsing text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderWriteStage {
    /// `folderRepo.Put` (`:390`). Fatal.
    FolderPut,
    /// `tagRepo.Add` (`:396`). Fatal.
    TagAdd,
    /// `artistRepo.Put` with the restricted column list (`:403`). Fatal.
    ArtistPut,
    /// `libraryRepo.AddArtist` (`:408`). Fatal.
    AddArtist,
    /// `repo.Put` inside `persistAlbum` (`:481`). Fatal.
    AlbumPut,
    /// `repo.ReassignAnnotation` inside `persistAlbum` (`:490`). Warn-only.
    ReassignAnnotation,
    /// `repo.CopyAttributes(…, "created_at")` inside `persistAlbum` (`:496`). Warn-only;
    /// source-not-found is not a warning at all (`:497-500`).
    CopyAttributes,
    /// `mfRepo.Put` (`:430`). Fatal.
    MediaFilePut,
    /// `tx.Artwork().DeleteForItems(media_file, ids)` (`:439`). Warn-only.
    ArtworkInvalidate,
    /// `mfRepo.MarkMissing(true, …)` (`:446`). Fatal.
    MarkMissing,
    /// `albumRepo.Touch(…)` over the missing tracks' album ids (`:456`). Fatal.
    AlbumTouch,
    /// `queue.Enqueue`/`EnqueueIfMissing` (`:466-470`). Warn-only.
    Enqueue,
}

impl FolderWriteStage {
    /// `true` when a failure at this stage aborts the whole folder transaction.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        match self {
            Self::FolderPut
            | Self::TagAdd
            | Self::ArtistPut
            | Self::AddArtist
            | Self::AlbumPut
            | Self::MediaFilePut
            | Self::MarkMissing
            | Self::AlbumTouch => true,
            Self::ReassignAnnotation
            | Self::CopyAttributes
            | Self::ArtworkInvalidate
            | Self::Enqueue => false,
        }
    }

    /// Stable machine-readable token for warning sinks and evidence labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FolderPut => "folder_put",
            Self::TagAdd => "tag_add",
            Self::ArtistPut => "artist_put",
            Self::AddArtist => "add_artist",
            Self::AlbumPut => "album_put",
            Self::ReassignAnnotation => "reassign_annotation",
            Self::CopyAttributes => "copy_attributes",
            Self::MediaFilePut => "media_file_put",
            Self::ArtworkInvalidate => "artwork_invalidate",
            Self::MarkMissing => "mark_missing",
            Self::AlbumTouch => "album_touch",
            Self::Enqueue => "enqueue",
        }
    }
}

/// Why a [`FolderWriteBatch`] was refused BEFORE any checkout. `D-BATCH` leaves scope/ID
/// rejection to DB instead of letting an inconsistent batch read as a successful scan; these
/// arms are port-side contract, not Go errors — Go cannot build such a batch
/// (`entry.job.lib` and `track.AlbumID` are always set).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeRejection {
    /// [`FolderRecord::id`] is empty. `folder_repo::FolderWriteError::EmptyId` says the same
    /// at the write seam; refusing here keeps the refusal pre-transaction.
    #[error("folder record has an empty id: the id comes from SC and is never generated here")]
    EmptyFolderId,

    /// An artist at `artists[{index}]` has an empty id.
    #[error("artist record at index {index} has an empty id")]
    EmptyArtistId {
        /// Position in [`FolderWriteBatch::artists`].
        index: usize,
    },

    /// An album at `albums[{index}]` has an empty id.
    #[error("album record at index {index} has an empty id")]
    EmptyAlbumId {
        /// Position in [`FolderWriteBatch::albums`].
        index: usize,
    },

    /// The folder row belongs to another library. Go cannot produce this (`toFolder` takes
    /// `f.job.lib`, `folder_entry.go:93`), so this is the structural guard `D-BATCH` asks DB
    /// to hold.
    #[error("folder record has library_id {found}, batch library is {batch_library}")]
    FolderLibraryMismatch {
        /// The record's own `library_id`.
        found: i64,
        /// [`FolderWriteBatch::library_id`].
        batch_library: i64,
    },

    /// An album row belongs to another library (`album.library_id` is a real FK column,
    /// `schema_ddl.rs:58`).
    #[error(
        "album record at index {index} has library_id {found}, batch library is {batch_library}"
    )]
    AlbumLibraryMismatch {
        /// Position in [`FolderWriteBatch::albums`].
        index: usize,
        /// The record's own `library_id`.
        found: i64,
        /// [`FolderWriteBatch::library_id`].
        batch_library: i64,
    },

    /// A media record at `media[{index}]` belongs to another library: Go's `Put` builds the
    /// match probe from the record's OWN `Path`/`LibraryID` (`mediafile_repository.go:189`),
    /// so such a record would write outside the scanned library.
    #[error(
        "media record at index {index} has library_id {found}, batch library is {batch_library}"
    )]
    MediaLibraryMismatch {
        /// Position in [`FolderWriteBatch::media`].
        index: usize,
        /// The record's own `library_id`.
        found: i64,
        /// [`FolderWriteBatch::library_id`].
        batch_library: i64,
    },

    /// Two albums in one folder delta share an id. Go groups tracks by album id
    /// (`createAlbumsFromMediaFiles`, `:328-331`), so an id appears once per folder; a
    /// duplicate would let the second Put silently rewrite the first's junction rows.
    #[error("album id {id} appears twice in one folder batch")]
    DuplicateAlbumId {
        /// The repeated id.
        id: String,
    },

    /// A transition with an empty previous id. Go only writes the map when both ids exist
    /// and differ (`:316-321`), and an empty `prevID` takes the early return at `:482-484`,
    /// so such an entry is a mapping SC never observed.
    #[error("album transition for new id {new_id} has an empty previous id")]
    EmptyAlbumTransition {
        /// The transition's new-album id (the map key).
        new_id: String,
    },

    /// A transition maps an album id onto itself (`:319`'s `prevAlbumID != track.AlbumID`
    /// makes this unreachable in Go; `sql_annotations.go:186-188` would silently no-op it).
    #[error("album transition maps {new_id} onto itself")]
    SelfAlbumTransition {
        /// The id (key and value are equal).
        new_id: String,
    },

    /// A transition key names no album in this batch. Go's map is keyed by THIS folder's
    /// track album ids (`:316-321`) and unconsumed entries die with the per-attempt clone
    /// (`:378`), so a key SC cannot have produced here is a batch defect, not a scan fact.
    #[error("album transition key {new_id} names no album in this batch")]
    UnresolvableAlbumTransition {
        /// The map key with no matching album record.
        new_id: String,
    },

    /// A missing-media entry has an empty id: Go's `entry.missingTracks` are DB-loaded
    /// `*model.MediaFile` rows (`:250-256`), so they always carry one.
    #[error("missing media entry at index {index} has an empty id")]
    EmptyMissingMediaId {
        /// Position in [`FolderWriteBatch::missing`].
        index: usize,
    },

    /// A fatal primitive returned a DOMAIN failure that [`FolderWriteBatch::validate`] could
    /// not pre-empt — the reachable case is a repo's `Builder` arm
    /// (`FolderWriteError::Builder`, `AlbumWriteError::Builder`, `MediaFileWriteError::Builder`,
    /// all `crate::marshal::MarshalError`). Recorded as `{message}`, never as a forged SQLite
    /// code, so `store::is_busy` is never asked to classify a non-driver failure (`D-ERROR`).
    #[error("write rejected at stage {}: {message}", stage.as_str())]
    WriteRejected {
        /// The arm whose primitive failed.
        stage: FolderWriteStage,
        /// The original domain error's message; no record contents.
        message: String,
    },
}

/// The caller-stamped instants this batch writes. `D-BATCH` forbids clock reads inside the
/// retry closure, so SC captures them at batch construction. One field per DISTINCT Go
/// `time.Now()` site, so a test can name every stored value:
///
/// * [`FolderStamps::folder_put`] — the folder's `updated_at`/`created_at` stamp
///   (`folder_repository.go:255-259` → `put_folder_in_tx`'s `caller_scan_stamp`),
/// * [`FolderStamps::artist_put`] — `dba.CreatedAt = new(time.Now())`, and `updated_at` is
///   the same instant (`artist_repository.go:240-241`),
/// * [`FolderStamps::album_put`] — `al.ImportedAt = time.Now()` (`album_repository.go:210`),
/// * [`FolderStamps::mediafile_put`] — `m.CreatedAt` when unset (`mediafile_repository.go:185`),
/// * [`FolderStamps::mark_missing`] — `Set("updated_at", time.Now())`
///   (`mediafile_repository.go:460`),
/// * [`FolderStamps::album_touch`] — `Set("imported_at", time.Now())` (`album_repository.go:365`),
/// * [`FolderStamps::artwork`] — the queue's `retry_at`/`enqueued_at`, one instant per
///   `enqueue` call (`artwork_queue_repository.go:151`).
///
/// Go samples these microseconds apart inside one folder transaction, so SC may pass equal
/// values; keeping the fields separate is what stops an implementation that stamped the
/// commit clock from passing an assertion on a named value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderStamps {
    /// Folder Put's stamp (`:390`).
    pub folder_put: DateTime<Utc>,
    /// Artist Put's stamp (`:403`).
    pub artist_put: DateTime<Utc>,
    /// Album Put's `imported_at` (`:481`).
    pub album_put: DateTime<Utc>,
    /// MediaFile Put's `created_at` fallback stamp (`:430`).
    pub mediafile_put: DateTime<Utc>,
    /// MarkMissing's `updated_at` (`:446`).
    pub mark_missing: DateTime<Utc>,
    /// Touch's `imported_at` (`:456`).
    pub album_touch: DateTime<Utc>,
    /// Enqueue's `retry_at`/`enqueued_at` (`:466-470`).
    pub artwork: DateTime<Utc>,
}

/// One missing track. Go's `entry.missingTracks` is `[]*model.MediaFile`
/// (`folder_entry.go:57`, filled at `:262-266`) and Touch's album set comes from those rows'
/// `AlbumID` (`:450-453`), so an id alone cannot reproduce the touch set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingMedia {
    /// The `media_file.id` to flag missing (`:446`).
    pub id: String,
    /// That row's `album_id`, which feeds the Touch set (`:450-453`).
    pub album_id: String,
}

/// One album-ID transition: the album id this folder writes (`new_id`) and the id whose
/// annotations and `created_at` it inherits (`prev_id`). Go's `entry.albumIDMap`
/// (`folder_entry.go:54`) is exactly this map, keyed by the NEW id (`:316-321`) and cloned
/// per attempt (`:378`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumTransition {
    /// The album id written by this batch (the map key).
    pub new_id: String,
    /// The previous album id (`prevID`, `:480`).
    pub prev_id: String,
}

/// An immutable folder delta: everything the folder transaction writes, fully constructed by
/// SC. No clock, no `Cell`/`RefCell`, no filesystem handle, no scope/auth actor, no raw SQL,
/// and no mutable album map — `album_transitions` is data and the per-attempt working copy is
/// built inside the closure ([`FolderWriteBatch::album_id_map`], Go's `maps.Clone` at `:378`).
///
/// Deliberately absent:
/// * `queue_items` — Go DERIVES them inside `persistFolder` from the artists (`:412-414`) and
///   albums (`:423-425`) with the unknown/VA filters, so [`FolderWriteBatch::queue_items`]
///   reproduces that derivation instead of letting a caller enqueue entities this folder never
///   touched.
/// * scope/auth — `D-BATCH` forbids an untrusted public actor parameter. The only scope fact
///   is `library_id`, checked structurally by [`FolderWriteBatch::validate`].
/// * media match identity — built from the record like Go's `Put`
///   (`mediafile_repository.go:189`), so a supplied identity cannot disagree with the row.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderWriteBatch {
    /// The configured library this folder belongs to. Go reads it from `entry.job.lib.ID` at
    /// every call site (`:396`, `:408`), never from a request.
    pub library_id: i64,
    /// The folder row (`entry.toFolder()`, `:389`); its `id` must be SC's.
    pub folder: FolderRecord,
    /// Tags to add (`entry.tags`, `:396`); dictionary ids are `model.NewTag`'s, i.e.
    /// `nd_id::new_tag_id`.
    pub tags: Vec<Tag>,
    /// Artists to Put with [`ARTIST_UPDATE_COLUMNS`] (`:403-405`) and link to the library.
    pub artists: Vec<ArtistRecord>,
    /// Albums to Put (`:419` → `persistAlbum`, `:478-506`).
    pub albums: Vec<AlbumRecord>,
    /// Tracks to Put (`:430`).
    pub media: Vec<MediaFileRecord>,
    /// Tracks that disappeared from this folder (`:446-456`).
    pub missing: Vec<MissingMedia>,
    /// Album-ID transitions to consume (`:378`, consumed by `persistAlbum` at `:504`).
    pub album_transitions: Vec<AlbumTransition>,
    /// `entry.job.lib.FullScanInProgress` (`:467`). SC supplies it from the library row; DB
    /// does not read boot policy and does not query `library` here.
    pub full_scan_mode: bool,
    /// `conf.Server.Subsonic.AppendSubtitle` as it affects `media_file.full_text`
    /// (`model/mediafile.go:110`). SC supplies the configured value.
    pub append_subtitle: bool,
    /// Caller-stamped instants (see [`FolderStamps`]); never read from a clock here.
    pub stamps: FolderStamps,
}

impl FolderWriteBatch {
    /// Structural consistency checks, run BEFORE the pool checkout (`D-BATCH`: DB rejects
    /// absent/invalid scope rather than inferring scan success). Order is deterministic —
    /// folder, albums, artists, media, transitions, missing — so the first rejection of a
    /// multi-defect batch is stable across attempts and runs.
    #[must_use = "a rejected batch writes nothing: handle the ScopeRejection instead"]
    pub fn validate(&self) -> Result<(), ScopeRejection> {
        if self.folder.id.is_empty() {
            return Err(ScopeRejection::EmptyFolderId);
        }
        if self.folder.library_id != self.library_id {
            return Err(ScopeRejection::FolderLibraryMismatch {
                found: self.folder.library_id,
                batch_library: self.library_id,
            });
        }

        let mut album_ids: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, album) in self.albums.iter().enumerate() {
            if album.id.is_empty() {
                return Err(ScopeRejection::EmptyAlbumId { index });
            }
            if album.library_id != self.library_id {
                return Err(ScopeRejection::AlbumLibraryMismatch {
                    index,
                    found: album.library_id,
                    batch_library: self.library_id,
                });
            }
            if album_ids.insert(album.id.as_str(), index).is_some() {
                return Err(ScopeRejection::DuplicateAlbumId {
                    id: album.id.clone(),
                });
            }
        }

        for (index, artist) in self.artists.iter().enumerate() {
            if artist.id.is_empty() {
                return Err(ScopeRejection::EmptyArtistId { index });
            }
        }

        for (index, record) in self.media.iter().enumerate() {
            if record.library_id != self.library_id {
                return Err(ScopeRejection::MediaLibraryMismatch {
                    index,
                    found: record.library_id,
                    batch_library: self.library_id,
                });
            }
        }

        for transition in &self.album_transitions {
            if transition.prev_id.is_empty() {
                return Err(ScopeRejection::EmptyAlbumTransition {
                    new_id: transition.new_id.clone(),
                });
            }
            if transition.new_id == transition.prev_id {
                return Err(ScopeRejection::SelfAlbumTransition {
                    new_id: transition.new_id.clone(),
                });
            }
            if !album_ids.contains_key(transition.new_id.as_str()) {
                return Err(ScopeRejection::UnresolvableAlbumTransition {
                    new_id: transition.new_id.clone(),
                });
            }
        }

        for (index, entry) in self.missing.iter().enumerate() {
            if entry.id.is_empty() {
                return Err(ScopeRejection::EmptyMissingMediaId { index });
            }
        }
        Ok(())
    }

    /// Go's `albumIDMap := maps.Clone(entry.albumIDMap)` (`:378`): a fresh owned working copy
    /// that `persistAlbum` consumes with `delete(idMap, a.ID)` (`:504`), so a failed attempt's
    /// deletions never leak into the next attempt.
    #[must_use]
    pub fn album_id_map(&self) -> BTreeMap<String, String> {
        self.album_transitions
            .iter()
            .map(|t| (t.new_id.clone(), t.prev_id.clone()))
            .collect()
    }

    /// `:412-414` then `:423-425`: one scanner artwork item per artist whose name is neither
    /// `UnknownArtist` nor `VariousArtists`, then one per album whose name is not
    /// `UnknownAlbum`, in Go's own loop order (artists first), each built by
    /// [`scan_artwork_item`].
    #[must_use]
    pub fn queue_items(&self) -> Vec<ArtworkQueueItem> {
        let mut items = Vec::with_capacity(self.artists.len() + self.albums.len());
        for artist in &self.artists {
            if QUEUE_EXCLUDED_ARTIST_NAMES.contains(&artist.name.as_str()) {
                continue;
            }
            items.push(scan_artwork_item(
                nd_id::ARTWORK_KIND_ARTIST_PREFIX,
                &artist.id,
                SCAN_ARTWORK_PRIORITY,
            ));
        }
        for album in &self.albums {
            if album.name == QUEUE_EXCLUDED_ALBUM_NAME {
                continue;
            }
            items.push(scan_artwork_item(
                nd_id::ARTWORK_KIND_ALBUM_PREFIX,
                &album.id,
                SCAN_ARTWORK_PRIORITY,
            ));
        }
        items
    }
}

/// `scanArtworkItem` (`scanner/image_changes.go:107-112`). `image_type` is
/// [`SCAN_ARTWORK_IMAGE_TYPE`] exactly as Go sets `model.ImageTypePrimary`.
/// `attempts`/`retry_at`/`enqueued_at`/`trace` are placeholders the queue renderer overwrites
/// with the caller's `now` and its own `0`/`'[]'` arms (`artwork_queue_repository.go:150-155`),
/// and `EnqueueIfMissing` binds only the four leading columns (`:76-77`).
#[must_use]
pub fn scan_artwork_item(kind_prefix: &str, id: &str, priority: i64) -> ArtworkQueueItem {
    ArtworkQueueItem {
        item_kind: kind_prefix.to_string(),
        item_id: id.to_string(),
        image_type: SCAN_ARTWORK_IMAGE_TYPE.to_string(),
        priority,
        attempts: 0,
        retry_at: None,
        enqueued_at: None,
        trace: String::new(),
    }
}

/// A warn-only arm's captured degradation. The stage and ids are the facts Go logs (`:440`,
/// `:471`, `:491`, `:498`); `source` retains the concrete typed cause rather than a stringified
/// summary — no record contents, no paths, no secrets.
#[derive(Debug)]
pub struct FolderWriteWarning {
    /// Which arm degraded.
    pub stage: FolderWriteStage,
    /// The entity the arm targeted (album id, or the folder id for the folder-wide artwork and
    /// enqueue arms, whose Go log lines name no single item).
    pub item_id: String,
    /// A second id where the arm has two (annotation/copy `prev_id`); empty otherwise.
    pub related_id: String,
    /// The original driver cause, retained as the concrete `rusqlite::Error`.
    pub source: rusqlite::Error,
}

impl FolderWriteWarning {
    /// The stage token, for a warning sink (the equivalent of Go's `sendWarning` message).
    #[must_use]
    pub fn stage_str(&self) -> &'static str {
        self.stage.as_str()
    }
}

/// What one folder transaction did, built ONLY from the committed attempt's results
/// (`D-BATCH`: nothing is published pre-commit, and SC's `imageChanges.record`
/// (`:363-367`) plus progress stamping ride on this value alone).
#[derive(Debug)]
pub struct CommittedFolderOutcome {
    /// The folder row's id.
    pub folder_id: String,
    /// `true` when the folder Put took the INSERT leg rather than the UPDATE leg
    /// (`sql_base_repository.go:630-641`). Go's `entry.isNew()` is its own `updTime.IsZero()`
    /// test (`folder_entry.go:68-70`) — that stays SC's fact; this one is the DB's statement
    /// about which leg ran.
    pub folder_inserted: bool,
    /// Album ids written, in batch order (`:419`).
    pub album_ids: Vec<String>,
    /// Artist ids written and linked, in batch order (`:403-408`).
    pub artist_ids: Vec<String>,
    /// Media ids AS COMMITTED by each Put (`:430`): the caller's id, the id the `putByMatch`
    /// probe reused, or the id Go's random arm minted inside the transaction. Only these are
    /// publishable; an aborted attempt's minted ids appear nowhere.
    pub media_ids: Vec<String>,
    /// The subset of `media_ids` whose Put took the INSERT leg.
    pub inserted_media_ids: Vec<String>,
    /// Album ids touched by the missing-track arm (`:450-456`), deduplicated. Go collects
    /// `maps.Keys` of a `slice.ToMap` (`:450-453`) — unspecified iteration order — so this
    /// keeps first-occurrence order; the SET is the observable contract.
    pub touched_album_ids: Vec<String>,
    /// Rows MarkMissing affected. [`crate::mediafile_repo::mark_media_missing_in_tx`]
    /// preserves Go's `err != nil || c == 0` early return (`mediafile_repository.go:463-467`),
    /// so a zero-matching FIRST chunk reports 0 and skips later chunks.
    pub missing_rows_marked: usize,
    /// `item_artwork` rows deleted for the media ids (`:439`).
    pub artwork_rows_deleted: usize,
    /// Scanner artwork items handed to the queue (`:412-425`). What the queue KEPT depends on
    /// the mode; this is what was enqueued.
    pub queued_item_count: usize,
    /// `true` when `EnqueueIfMissing` ran rather than `Enqueue` (`:467-469`), and `false` when
    /// the item list was empty and Go runs no queue call at all (`:464`).
    pub enqueued_if_missing: bool,
    /// Album-ID transitions actually consumed (`:480`, `:504`).
    pub consumed_transitions: Vec<AlbumTransition>,
    /// Warn-only arms that degraded, in Go's execution order. Empty on a clean run.
    pub warnings: Vec<FolderWriteWarning>,
}

/// The callback-level failure of the folder transaction. `Domain` carries a
/// [`ScopeRejection`]; every write failure rides `Sqlite` with the concrete
/// `rusqlite::Error`, so the retry loop's BUSY-only classification still sees the original
/// extended code (`store.rs:79`, Go's `db.go:137-140` + `persistence.go:216-218`). No LOCKED
/// handling and no begin-mode support — that is a separate DB-00 decision.
pub type FolderWriteFailure = DbOperationError<ScopeRejection>;

/// The full typed boundary of [`persist_folder_delta`]: `Domain` for a refused batch, and
/// `Sqlite`/`BusyExhausted`/`RollbackFailed`/`Pool` from the executor.
pub type FolderRunError = DbRuntimeError<ScopeRejection>;

/// Persist one folder delta in Go's order inside ONE retrying transaction, DB-owned.
///
/// This is `persistence.go:207-228` `WithTxRetry` over `phase_1_folders.go:353-357`:
/// [`SQLStore::try_with_tx_retry_sync`] (`store.rs:429`) holds ONE lease across all attempts
/// and retries ONLY `SQLITE_BUSY`. `SQLITE_LOCKED` and every other failure fail on the first
/// occurrence, matching `persistence.go:218`.
///
/// Validation runs before the checkout, so a batch with an empty id or a mismatched library
/// never acquires a connection.
///
/// # Errors
/// [`FolderRunError::Domain`] with the [`ScopeRejection`]; `Sqlite`/`BusyExhausted`
/// (`TransactionStage::Begin`/`Body`/`Commit`) for a fatal arm or an executor failure — in
/// every case the folder transaction is rolled back and nothing is published;
/// [`FolderRunError::RollbackFailed`] when cleanup itself failed; [`FolderRunError::Pool`]
/// for checkout. Warnings are not errors: a warn-only arm's failure is captured in
/// [`CommittedFolderOutcome::warnings`] and the transaction still commits.
pub fn persist_folder_delta(
    store: &SQLStore,
    batch: &FolderWriteBatch,
) -> Result<CommittedFolderOutcome, FolderRunError> {
    batch.validate().map_err(DbRuntimeError::Domain)?;
    store.try_with_tx_retry_sync(|tx| persist_folder_delta_in_tx(tx, batch))
}

/// The whole Go sequence on the CALLER's transaction, in `phase_1_folders.go` order.
///
/// `pub(crate)` by `D-BATCH`: SC consumes the batch types and may widen this one line later;
/// exposing a multi-folder tx now would make a Go-divergent batching reachable whose busy
/// retry replays N folders and whose rollback radius covers siblings Go would have committed.
///
/// Retry safety by construction: the batch is immutable, the album-id map is a fresh
/// per-attempt clone (`:378`), and no stamp comes from a clock. Order:
///
/// 1. cloned album-ID transitions (`:378`),
/// 2. Folder Put (`:390`),
/// 3. Tag Add (`:396`, chunk 200 through `tag_repo::add_in_tx`),
/// 4. per-artist restricted Put (`:403-405`) + Library AddArtist (`:408`),
/// 5. per-album `persistAlbum` (`:419` → `:478-506`): Album Put (`:481`), warn-only
///    ReassignAnnotation (`:490`), warn-only CopyAttributes of `created_at` with NotFound
///    ignored (`:496-500`), then consume the mapping (`:504`),
/// 6. per-track MediaFile Put (`:430`),
/// 7. warn-only Artwork DeleteForItems over the track ids (`:437-441`),
/// 8. MarkMissing (`:446`) then Touch over the missing tracks' album ids (`:450-456`),
/// 9. enqueue (`:466-470`), mode chosen by `full_scan_mode` (`:467`).
///
/// # Errors
/// [`FolderWriteFailure::Sqlite`] from a FATAL arm, carrying the concrete driver error
/// unwrapped — Go's `fmt.Errorf` prefixes are log text, not error identity, and `is_busy`
/// needs the raw code. Warn-only arms never return: their failure becomes a
/// [`FolderWriteWarning`]. A warn-only arm that aborts its statement still leaves the
/// transaction usable, because SQLite's `ABORT` error action undoes only the failing
/// statement — which is what makes Go's "warn and continue" legal inside one transaction.
///
/// A warn-only arm may NOT degrade a failure that ENDED the transaction (`RAISE(ROLLBACK)`,
/// an explicit rollback, crash recovery): every later statement would then run in AUTOCOMMIT
/// mode and stay durable even though the executor's `COMMIT` fails afterwards. [`warn_arm`]
/// detects that with SQLite's own `sqlite3_get_autocommit()` and returns the original typed
/// failure as fatal instead, so `D-BATCH`'s "transaction-aborted is always fatal" holds.
pub(crate) fn persist_folder_delta_in_tx(
    tx: &rusqlite::Transaction<'_>,
    batch: &FolderWriteBatch,
) -> Result<CommittedFolderOutcome, FolderWriteFailure> {
    // :378 — `albumIDMap := maps.Clone(entry.albumIDMap)`. Nothing else is mutable, and this
    // copy is what the album loop consumes.
    let mut album_id_map = batch.album_id_map();
    let mut warnings: Vec<FolderWriteWarning> = Vec::new();

    // :389-393 — folder := entry.toFolder(); folderRepo.Put(ctx, folder).
    let folder = put_folder_in_tx(tx, &batch.folder, &batch.stamps.folder_put, &[])
        .map_err(|err| fatal(FolderWriteStage::FolderPut, err))?;

    // :396 — tagRepo.Add(ctx, lib.ID, entry.tags...). Empty tags render and execute nothing.
    add_in_tx(tx, batch.library_id, &batch.tags).map_err(sqlite_only)?;

    // :401-415 — per artist: restricted Put, then AddArtist.
    let mut artist_ids = Vec::with_capacity(batch.artists.len());
    for artist in &batch.artists {
        put_artist_in_tx(tx, artist, &batch.stamps.artist_put, &ARTIST_UPDATE_COLUMNS)
            .map_err(|err| fatal(FolderWriteStage::ArtistPut, err))?;
        add_artist_in_tx(tx, batch.library_id, &artist.id).map_err(sqlite_only)?;
        artist_ids.push(artist.id.clone());
    }

    // :417-426 — per album: persistAlbum(albumRepo, &entry.albums[i], albumIDMap).
    let mut album_ids = Vec::with_capacity(batch.albums.len());
    let mut consumed_transitions = Vec::new();
    for album in &batch.albums {
        let prev_id = album_id_map.get(&album.id).cloned();
        put_album_in_tx(tx, album, &batch.stamps.album_put)
            .map_err(|err| fatal(FolderWriteStage::AlbumPut, err))?;

        // :482-484 — `if prevID == "" { return nil }`: with no transition NEITHER warn-only
        // arm runs and the mapping is not consumed.
        if let Some(prev_id) = prev_id {
            // :488-493 — ReassignAnnotation, warn-only (`log.Warn` + `sendWarning`).
            if let Err(err) =
                reassign_annotation_in_tx(tx, AnnotationEntityKind::Album, &prev_id, &album.id)
            {
                warnings.push(warn_arm(
                    tx,
                    FolderWriteStage::ReassignAnnotation,
                    err,
                    album.id.clone(),
                    prev_id.clone(),
                )?);
            }

            // :495-502 — CopyAttributes(ctx, prevID, a.ID, "created_at"). A missing source
            // row is `model.ErrNotFound` and is SILENTLY ignored (`:497-500`): no warning.
            match copy_album_created_at_in_tx(tx, &prev_id, &album.id) {
                Ok(CopyAlbumCreatedAtOutcome::Copied)
                | Ok(CopyAlbumCreatedAtOutcome::SkippedPoisonedSource)
                | Ok(CopyAlbumCreatedAtOutcome::SourceNotFound) => {}
                Err(err) => warnings.push(warn_arm(
                    tx,
                    FolderWriteStage::CopyAttributes,
                    err,
                    album.id.clone(),
                    prev_id.clone(),
                )?),
            }

            // :504 — `delete(idMap, a.ID)`, on THIS attempt's copy only.
            album_id_map.remove(&album.id);
            consumed_transitions.push(AlbumTransition {
                new_id: album.id.clone(),
                prev_id,
            });
        }
        album_ids.push(album.id.clone());
    }

    // :428-434 — per track: mfRepo.Put(ctx, &entry.tracks[i]). Go builds the `Eq{path,
    // library_id}` filter from the record (`mediafile_repository.go:189`), but
    // `putByMatch` SHORT-CIRCUITS the probe when the record already has an id
    // (`sql_base_repository.go:578-580`), so the arm is chosen per record: `Id` for a
    // non-empty id (direct put, no probe — a same-path row of another id is NOT reused),
    // `Match` for an empty one (probe, reuse the found id, else Go's generated-id arm at
    // `:635-638`). Neither port arm alone reproduces Go for both cases, which is why this
    // chooses rather than always passing `Match`.
    let mut media_ids = Vec::with_capacity(batch.media.len());
    let mut inserted_media_ids = Vec::new();
    for record in &batch.media {
        let identity = if record.id.is_empty() {
            MediaIdentity::Match {
                path: record.path.clone(),
                library_id: record.library_id,
            }
        } else {
            MediaIdentity::Id(record.id.clone())
        };
        let written = put_mediafile_in_tx(
            tx,
            &identity,
            record,
            &batch.stamps.mediafile_put,
            batch.append_subtitle,
        )
        .map_err(|err| fatal(FolderWriteStage::MediaFilePut, err))?;
        media_ids.push(written.id.clone());
        if written.inserted {
            inserted_media_ids.push(written.id.clone());
        }
    }

    // :436-441 — a re-imported track returns to unresolved artwork. Warn-only, and the whole
    // arm is skipped when the folder has no tracks (`:437`).
    let mut artwork_rows_deleted = 0usize;
    if !batch.media.is_empty() {
        match delete_for_items_in_tx(tx, nd_id::ARTWORK_KIND_MEDIA_FILE_PREFIX, &media_ids) {
            Ok(deleted) => artwork_rows_deleted = deleted,
            Err(err) => warnings.push(warn_arm(
                tx,
                FolderWriteStage::ArtworkInvalidate,
                err,
                batch.folder.id.clone(),
                String::new(),
            )?),
        }
    }

    // :443-459 — MarkMissing, then Touch over the missing tracks' album ids. Both fatal.
    let mut missing_rows_marked = 0usize;
    let mut touched_album_ids: Vec<String> = Vec::new();
    if !batch.missing.is_empty() {
        let missing_ids: Vec<String> = batch.missing.iter().map(|m| m.id.clone()).collect();
        missing_rows_marked =
            mark_media_missing_in_tx(tx, true, &missing_ids, &batch.stamps.mark_missing)
                .map_err(sqlite_only)?;

        // :450-453 — the missing tracks' album ids, deduplicated.
        for entry in &batch.missing {
            if !touched_album_ids.contains(&entry.album_id) {
                touched_album_ids.push(entry.album_id.clone());
            }
        }
        touch_albums_in_tx(tx, &touched_album_ids, &batch.stamps.album_touch)
            .map_err(sqlite_only)?;
    }

    // :462-472 — enqueue the scanner's album/artist items. "Never fails the scan." An empty
    // item list runs no queue statement at all (`:464`).
    let queue_items = batch.queue_items();
    let mut enqueued_if_missing = false;
    if !queue_items.is_empty() {
        let result = if batch.full_scan_mode {
            enqueued_if_missing = true;
            enqueue_if_missing_in_tx(tx, &queue_items, &batch.stamps.artwork)
        } else {
            enqueue_in_tx(tx, &queue_items, EnqueueMode::Reset, &batch.stamps.artwork)
        };
        if let Err(err) = result {
            warnings.push(warn_arm(
                tx,
                FolderWriteStage::Enqueue,
                err,
                batch.folder.id.clone(),
                String::new(),
            )?);
        }
    }

    Ok(CommittedFolderOutcome {
        folder_id: folder.id,
        folder_inserted: folder.inserted,
        album_ids,
        artist_ids,
        media_ids,
        inserted_media_ids,
        touched_album_ids,
        missing_rows_marked,
        artwork_rows_deleted,
        queued_item_count: queue_items.len(),
        enqueued_if_missing,
        consumed_transitions,
        warnings,
    })
}

/// Fatal arm: keep the concrete driver error on the `Sqlite` arm so the retry loop's BUSY
/// classification sees the original extended code (`store.rs:79`, `db.go:137-140`). A DOMAIN
/// failure stays a domain failure — [`ScopeRejection::WriteRejected`] records its message
/// rather than forging a SQLite code (`D-ERROR`), and either way the folder transaction rolls
/// back.
fn fatal<E: std::error::Error>(
    stage: FolderWriteStage,
    err: DbOperationError<E>,
) -> FolderWriteFailure {
    match err {
        DbOperationError::Sqlite(source) => DbOperationError::Sqlite(source),
        DbOperationError::Domain(cause) => {
            DbOperationError::Domain(ScopeRejection::WriteRejected {
                stage,
                message: cause.to_string(),
            })
        }
    }
}

/// Coerce an `Infallible`-domain primitive into this lane's callback error type. The domain
/// arm is structurally uninhabited for these types (`tag_repo::TagWriteError`,
/// `library_repo::LibraryScanError`, `mediafile_repo::MediaMarkError`,
/// `album_repo::AlbumTouchError`, `sql_annotations::AnnotationWriteError`), so the
/// `unreachable!` names a documented impossibility rather than swallowing an error.
fn sqlite_only<E: std::error::Error>(err: DbOperationError<E>) -> FolderWriteFailure {
    DbOperationError::Sqlite(driver_error(err))
}

/// Extract the concrete driver error from an error whose domain arm cannot exist.
///
/// # Panics
/// Only if an `Infallible` domain type were somehow inhabited, which the type system rules
/// out for every caller here.
fn driver_error<E: std::error::Error>(err: DbOperationError<E>) -> rusqlite::Error {
    match err {
        DbOperationError::Sqlite(source) => source,
        DbOperationError::Domain(cause) => {
            unreachable!("this primitive has no domain error: {cause}")
        }
    }
}

/// A warn-only arm's failure, classified as a warning or as a fatal transaction loss.
///
/// `D-BATCH` makes "warn and continue" legal only while the folder transaction is still
/// alive, and it is: SQLite's default `ABORT` error action (what `RAISE(ABORT)` and an
/// ordinary constraint violation use) undoes ONLY the failing statement, so the remaining
/// arms still run and the transaction still commits. `RAISE(ROLLBACK)` — and any other
/// event that ends the transaction (an explicit `ROLLBACK`, a hot-journal crash recovery) —
/// is different: the transaction is gone, so every later statement would run in
/// AUTOCOMMIT mode and become durable on its own, and the executor's later `COMMIT` would
/// fail with "no transaction is active" AFTER those leaked writes landed. That is a
/// transaction loss, and per `D-BATCH` a transaction-aborted failure is always fatal.
///
/// Detection is `rusqlite::Transaction::is_autocommit` (deref'd `Connection::is_autocommit`,
/// i.e. SQLite's own `sqlite3_get_autocommit()`), checked AFTER the failing primitive
/// returns and BEFORE anything is recorded as a warning. It cannot false-positive on a
/// normal statement failure: an `ABORT`-class error leaves the transaction open, so
/// `is_autocommit()` is still `false` and the arm degrades to a warning exactly as before.
/// It cannot false-positive on an unstarted transaction either, because every warn arm has
/// already executed at least one statement on this lease by the time it can fail — a
/// statement always starts the deferred transaction, so `true` here can only mean "it ended".
///
/// On loss the ORIGINAL typed driver error is returned as [`DbOperationError::Sqlite`] —
/// never wrapped as a warning and never replaced with a forged code, so the retry loop's
/// BUSY-only classification (`store::is_busy`, Go's `db/db.go:137-140`) still sees the real
/// extended code, and a non-busy loss fails on its first occurrence.
fn warn_arm<E: std::error::Error>(
    tx: &rusqlite::Transaction<'_>,
    stage: FolderWriteStage,
    err: DbOperationError<E>,
    item_id: String,
    related_id: String,
) -> Result<FolderWriteWarning, FolderWriteFailure> {
    let source = driver_error(err);
    if tx.is_autocommit() {
        return Err(DbOperationError::Sqlite(source));
    }
    Ok(FolderWriteWarning {
        stage,
        item_id,
        related_id,
        source,
    })
}
