//! SC-03 — the library-aware `nd-mapping::MediaFile` → `nd-db::MediaFileRecord` bridge.
//!
//! This is the one seam the mapping layer deliberately leaves open: `nd-mapping`
//! produces the Go *mapping* DTO (`model.MediaFile` as written by
//! `model/metadata/map_mediafile.go`) but the folder transaction consumes a
//! [`nd_db::mediafile_repo::MediaFileRecord`], and Go fills that gap with data the
//! mapper cannot see:
//!
//! - **storage facts.** Go's `Info.FileInfo` is filled by the storage layer
//!   (`core/storage/local/local.go:90-109`) before `metadata.New`, and
//!   `map_mediafile.go:80-82` reads `Size`/`BirthTime`/`UpdatedAt` from it. The Rust
//!   tag reader produces no stat data, so `nd-mapping` leaves those fields `Option`
//!   and `map_file` (`nd-mapping/src/lib.rs:78-86`) passes
//!   [`nd_mapping::metadata::FileStat::default()`] — this bridge supplies them from
//!   the SC-02 [`crate::storage::FileStat`] instead.
//! - **record-only and hydration-only fields.** `id`, `missing`, `created_at` are the
//!   DB's (SC-05/DB-14 stamp them), and Go's `structs:"-" fields
//!   (`library_path`/`library_name`/annotation/bookmark/`genres`/`image`/`album_image`)
//!   are read-path state that no write map contains. The projection below states each
//!   one explicitly rather than inheriting a `Default`.
//! - **the birth-time substitute.** Go resolves a file's birth time inside its storage
//!   wrapper — native birth time, then `statx`, then `time.Now()`
//!   (`core/storage/local/local.go:117-128`) — and `map_mediafile.go:81` reads that
//!   already-resolved value through `md.BirthTime()` (`model/metadata/metadata.go:90`).
//!   `Put` stamps `CreatedAt`, never `BirthTime` (`persistence/mediafile_repository.go:184-186`).
//!   The port keeps the *storage fact* honest ([`TrackStat`] carries `None` for a
//!   platform miss) and performs Go's substitution at the **record** layer, where a
//!   `DateTime<Utc>` is required. See the birth-time section below.

//!   DB's (SC-05/DB-14 stamp them), and Go's `structs:"-" fields
//!   (`library_path`/`library_name`/annotation/bookmark/`genres`/`image`/`album_image`)
//!   are read-path state that no write map contains. The projection below states each
//!   one explicitly rather than inheriting a `Default`.
//! - **the checked narrowings.** Go's `int` conversions are the record's `i32`
//!   columns; [`as_i32_checked`] refuses an out-of-range value instead of truncating
//!   (see the module section below).
//! - **role translation.** Go's `model.Role` and the `participants` column's role key
//!   are the same string, so Go needs no conversion; the port must go through the
//!   existing checked [`nd_db::sql_participations::role_from_id`].
//!
//! ## Deliberate non-derivation (parent pins, all Go-cited)
//!
//! - `genre` stays empty. Go never derives it on the scan side — `grep -c Genre
//!   scanner/*.go model/metadata/*.go` is 0 — it is a **read-side** computation:
//!   `dbMediaFile.PostScan` calls `Tags.ToGenres()` and overwrites the column
//!   (`persistence/mediafile_repository.go:64`, `model/tag.go:138-148`). Counts belong
//!   to DB-17/DB-20. Persisting a scan-side genre here would double-derive it.
//! - `image`/`album_image` stay zero, which means **UNRESOLVED**, not known-absent
//!   (`nd_db::artwork_hydration::ItemImage` docs; the DB-11 declared deviation).
//!   `has_cover_art` is the *mapping* fact (`map_mediafile.go:69`, `Info.HasPicture`)
//!   and is the only artwork field this bridge sets; the artwork queue leg is DB-14's.
//! - `append_subtitle` is **not** wired here. It is `conf.Server.Subsonic.AppendSubtitle`,
//!   applied at marshal time to `media_file.full_text`
//!   (`nd_db::mediafile_repo::full_title`, `mediafile_repo.rs:1345-1364`; the folder
//!   batch carries it at `nd_db::scanner_writes::FolderWriteBatch::append_subtitle`,
//!   `scanner_writes.rs:387`). It is not a mapping fact and not a record column.
//! - Go has **no** `EnableFingerprinting` and **no** `SubtitleSuffix` anywhere in this
//!   tree (grep-verified 0 hits); nothing is invented for them.
//!
//! ## A5: checked i32 narrowing, error instead of default
//!
//! `map_mediafile.rs:281-283` (`as_i32`) is Go's `int(props.X)` — an unchecked
//! narrowing. On the Go side those values come from TagLib's `int`, so they cannot
//! exceed 32 bits; in Rust they arrive as `i64` from Lofty, so a value *can*. The
//! record's `bit_rate`/`sample_rate`/`channels`/`bit_depth` columns are `i32`, and the
//! subplan's rule is "malformed is error, not default"
//! (`docs/rust-port/subplans/scanner.md` SC-03 §4), so [`as_i32_checked`] returns
//! [`BridgeError::Overflow`] rather than wrapping or yielding 0.
//!
//! The tag-derived numbers keep Go's own coercion, which is *not* an error: BPM only
//! lands when it rounds into `0 < r <= MaxInt32` (`map_mediafile.go:40-44`,
//! `map_mediafile_test.go:136-154`) and track/disc numbers go through `tagNumber`,
//! which rejects negatives and out-of-int32 values as 0 (`metadata.go:158-165`). Those
//! are already decided inside `nd-mapping`; re-checking them here would contradict the
//! oracle.
//!
//! ## Birth-time resolution and the injected-clock contract (SC-03 P1 ruling)
//!
//! `media_file.birth_time` is NOT NULL in the DDL, so an absent storage birth time
//! cannot be persisted as-is. Go never faces that problem because its wrapper already
//! substituted `time.Now()` (`local.go:117-128`). [`effective_birth_time`] reproduces
//! exactly that arm, with the clock **injected by the caller**:
//!
//! - storage birth time available → that value, and the injected `now` is ignored
//!   (Go's substitution only fires when the platform reported nothing);
//! - `BirthTimeUnavailable` (`nd-scanner/src/storage.rs:123,155`) → the injected `now`;
//! - `BirthTimeUnavailable` with **no** injected substitute →
//!   [`BridgeError::MissingBirthSubstitute`], because inventing an instant inside the
//!   bridge would break the port's D-TIME rule (no clock reads below the scanner).
//!
//! SC-05 supplies the clock. Go calls `time.Now()` per file at mapping time, so either
//! a single scan-start stamp or a per-file stamp is parity-equivalent for the record;
//! the bridge itself never reads the clock. The album draft resolves through the same
//! helper, so the track's `birth_time` and the album's `created_at` cannot drift
//! (`model/mediafile.go:390`, `older` at `:466-480`).
//!
//! `TimestampOutOfRange` is **not** a Go range rule — Go has none: `localFileInfo.BirthTime`
//! returns whatever the OS reports (`local.go:116-128`) and the mapping never validates a
//! range (`map_mediafile.go:81`). It only marks the two places where the Rust conversion
//! cannot represent the OS value at all: `SystemTime` → chrono `DateTime` in
//! [`TrackStat::from_storage_stat`], and a `u64` size past `i64::MAX`. Inventing a
//! before-year-one / after-now rule would be a divergence, so none exists here.
//!
use std::collections::BTreeMap;
use std::time::SystemTime;

use chrono::{DateTime, TimeZone, Utc};

use nd_db::album_repo::{participant_names, NamedParticipant};
use nd_db::mediafile_repo::MediaFileRecord;
use nd_db::sql_participations::{role_from_id, Participant, Participants, RoleBridgeError};
use nd_db::sql_tags::DbTags;
use nd_mapping::mdfile::MediaFile;
use nd_mapping::metadata::FileStat as MappingFileStat;
use nd_mapping::metadata::Metadata;
use nd_mapping::participants::RoleCredits;

use crate::storage::{BirthTimeUnavailable, FileStat, StatUnavailable};

/// Field name used in errors for the record's `size` column (Go `Size`,
/// `model/mediafile.go:65`, DB `INTEGER`).
const FIELD_SIZE: &str = "size";
/// Field name used in errors for the four backend `i64`→`i32` narrowings.
const FIELD_BIT_RATE: &str = "bit_rate";
const FIELD_SAMPLE_RATE: &str = "sample_rate";
const FIELD_CHANNELS: &str = "channels";
const FIELD_BIT_DEPTH: &str = "bit_depth";
/// Field name used in errors for the storage-derived timestamp columns.
const FIELD_BIRTH_TIME: &str = "birth_time";
const FIELD_UPDATED_AT: &str = "updated_at";

/// Errors from this bridge. Every variant is a real data defect, never a defaulted
/// substitute: the whole point of the lane is that a missing or unrepresentable fact
/// stops the record instead of becoming a plausible zero.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// A backend audio property does not fit the record's `i32` column. Go's `int`
    /// conversion cannot produce this from TagLib; Lofty's `i64` can.
    #[error("{field} value {value} does not fit i32 (Go stores it in an int column)")]
    Overflow {
        /// Record column name.
        field: &'static str,
        /// The offending value.
        value: i64,
    },
    /// A file size that does not fit the record's `i64` column. Unreachable on a
    /// 64-bit host; kept so the narrowing is checked rather than unchecked.
    #[error("file size {value} does not fit i64")]
    SizeOverflow {
        /// The raw `u64` from the OS.
        value: u64,
    },
    /// A timestamp the Rust conversion cannot place in the record's `datetime`
    /// domain (a `SystemTime` chrono cannot render, or a `u64` size past `i64::MAX`).
    /// Go has **no** range rule here — `localFileInfo.BirthTime` returns whatever the OS
    /// reports (`core/storage/local/local.go:117-128`) — so this is not a parity arm:
    /// it marks a conversion limit, and nothing is clamped or fabricated across it.
    #[error(
        "{field} value is outside the representable UTC range (refusing to fabricate a value)"
    )]
    TimestampOutOfRange {
        /// Record column name.
        field: &'static str,
    },
    /// No storage birth time and no caller-supplied substitute. Go's wrapper substitutes
    /// `time.Now()` inside the storage layer (`local.go:117-128`); the port injects the
    /// clock instead (D-TIME), so a caller that supplies neither gets an error rather
    /// than a bridge that reads the clock on its own.
    #[error("{field} is unavailable from storage and no substitute instant was supplied")]
    MissingBirthSubstitute {
        /// Record column name.
        field: &'static str,
    },
    /// A role key that is not in Go's `AllRoles`. Go's `Role.UnmarshalText` rejects it
    /// (`model/participants.go:62-69`), so the credit is a defect, not a droppable entry.
    #[error("participant role is not a valid Go role: {source}")]
    InvalidRole {
        /// The checked role bridge's error.
        #[source]
        source: RoleBridgeError,
    },
    /// A mapping-side timestamp string that is not RFC 3339.
    #[error("{field} timestamp {value:?} is not RFC 3339")]
    MalformedTimestamp {
        /// Record column name.
        field: &'static str,
        /// The value as carried by `nd-mapping`.
        value: String,
    },
    /// The mapping DTO and the storage snapshot disagree about the same file. That can
    /// only come from a caller that read tags and stats from different places.
    #[error(
        "{field} disagrees between the mapping ({mapping}) and the storage snapshot ({storage})"
    )]
    StorageFactMismatch {
        /// Record column name.
        field: &'static str,
        /// What `MediaFile` carries.
        mapping: String,
        /// What the [`FileStat`] carries.
        storage: String,
    },
}

/// Storage facts for one track, in the record's own types.
///
/// This is the SC-02 → record projection Go performs implicitly when
/// `localFS.ReadTags` attaches a `localFileInfo` to every `Info`
/// (`core/storage/local/local.go:90-109`). `None` means "this platform/filesystem
/// reported nothing", which stays `None` at this layer: an unavailable birth time is
/// never turned into an epoch or a scan timestamp here. Go's `time.Now()` substitute is
/// the **record** layer's decision, performed by [`effective_birth_time`] with the
/// caller's injected clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackStat {
    /// `fs.FileInfo.Size()` (`local.go:96` feeds `map_mediafile.go:80`).
    pub size: i64,
    /// `fs.FileInfo.ModTime()` → `media_file.updated_at` (`map_mediafile.go:82`).
    /// The file's own mtime, never a scan timestamp.
    pub modified: Option<DateTime<Utc>>,
    /// Birth time → `media_file.birth_time` (`map_mediafile.go:81`).
    pub birth_time: Option<DateTime<Utc>>,
}

impl TrackStat {
    /// Project an SC-02 [`FileStat`] into the record's types.
    ///
    /// [`BirthTimeUnavailable`] maps to `birth_time: None`, **not** to
    /// `SystemTime::UNIX_EPOCH` and not to `Utc::now()`. This is the storage-fact
    /// layer's honesty (`nd-scanner/src/storage.rs:120-124`); Go's
    /// `localFileInfo.BirthTime` substitutes `time.Now()` one layer up
    /// (`core/storage/local/local.go:117-128`), which this port reproduces at the
    /// record layer via [`effective_birth_time`] and its injected clock.
    ///
    /// # Errors
    ///
    /// [`BridgeError::SizeOverflow`] for a size past `i64::MAX`, and
    /// [`BridgeError::TimestampOutOfRange`] for an instant chrono cannot place in UTC
    /// (a `SystemTime` far before the epoch).
    pub fn from_storage_stat(stat: &FileStat) -> Result<Self, BridgeError> {
        let size =
            i64::try_from(stat.size).map_err(|_| BridgeError::SizeOverflow { value: stat.size })?;
        Ok(Self {
            size,
            modified: system_time_to_utc(stat.modified, "updated_at")?,
            birth_time: system_time_to_utc_created(stat.created, "birth_time")?,
        })
    }

    /// The same facts in `nd-mapping`'s [`MappingFileStat`], for `Metadata::new`.
    ///
    /// `Metadata::new` takes the stat as RFC 3339 strings (`metadata.rs:28-37`) because
    /// Go's `FileInfo` returns `time.Time`; rendering them here keeps one storage
    /// snapshot as the single source of truth for both sides of the bridge.
    #[must_use]
    pub fn to_mapping_file_stat(&self) -> MappingFileStat {
        MappingFileStat {
            size: Some(self.size),
            mod_time: self.modified.map(|t| t.to_rfc3339()),
            birth_time: self.birth_time.map(|t| t.to_rfc3339()),
        }
    }
}

/// `fs.FileInfo.ModTime()`: Go's `ModTime` cannot fail, so an unavailable modified time
/// is a Rust-side absence and is carried as `None`.
fn system_time_to_utc(
    modified: Result<SystemTime, StatUnavailable>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, BridgeError> {
    match modified {
        Ok(value) => utc_of(value, field).map(Some),
        Err(_) => Ok(None),
    }
}

/// `times.Get(..).HasBirthTime()`: the unavailable case is the honest one, so it
/// becomes `None` rather than a substituted instant.
fn system_time_to_utc_created(
    created: Result<SystemTime, BirthTimeUnavailable>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, BridgeError> {
    match created {
        Ok(value) => utc_of(value, field).map(Some),
        Err(_) => Ok(None),
    }
}

/// A `SystemTime` → `DateTime<Utc>`, refusing rather than clamping.
fn utc_of(value: SystemTime, field: &'static str) -> Result<DateTime<Utc>, BridgeError> {
    // Exact whole-nanosecond offset from the epoch, floor-divided into
    // (seconds, non-negative subsecond) — the pair chrono's `timestamp_opt` accepts.
    let nanos: i128 = match value.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i128::from(d.as_secs()) * 1_000_000_000 + i128::from(d.subsec_nanos()),
        Err(e) => {
            // Before the epoch. Go's time.Time represents these fine; the DB datetime
            // spelling does too, so convert exactly rather than clamping.
            let d = e.duration();
            -(i128::from(d.as_secs()) * 1_000_000_000 + i128::from(d.subsec_nanos()))
        }
    };
    let secs = nanos.div_euclid(1_000_000_000);
    let subnanos = nanos.rem_euclid(1_000_000_000) as u32;
    let secs = i64::try_from(secs).map_err(|_| BridgeError::TimestampOutOfRange { field })?;
    Utc.timestamp_opt(secs, subnanos)
        .single()
        .ok_or(BridgeError::TimestampOutOfRange { field })
}

/// Go's `int(props.X)` narrowing, checked.
///
/// # Errors
///
/// [`BridgeError::Overflow`] when `value` is outside `i32`.
pub fn as_i32_checked(field: &'static str, value: i64) -> Result<i32, BridgeError> {
    i32::try_from(value).map_err(|_| BridgeError::Overflow { field, value })
}

/// Project a mapped track plus its storage facts into the record the folder
/// transaction writes.
///
/// `md` supplies the values Go reads off `Metadata` for fields the mapping DTO does
/// not carry (`map_mediafile.go` uses `md` directly for participants and PIDs);
/// `mf` is the mapping output; `stat` is the storage snapshot.
///
/// `now` is the caller's clock, used only as the birth-time substitute Go's storage
/// wrapper would have applied (`local.go:117-128`): `Some` is required when the
/// snapshot reports no birth time, and is otherwise ignored. Go stamps it per file at
/// mapping time, so an SC-05 scan may inject one scan-start stamp for the whole scan or
/// a per-file instant — both are parity. The bridge never reads a clock itself (D-TIME);
/// pass the same value to [`crate::projection::album_draft_from_track`]'s `birth` after
/// resolving it with [`effective_birth_time`].
///
/// Every field of [`MediaFileRecord`] appears below exactly once, which is the point:
/// `MediaFileRecord` has no `Default`, so the projection table is forced to be
/// explicit about which fields are mapping facts, which are storage facts, and which
/// are somebody else's zero.
///
/// # Errors
///
/// Any [`BridgeError`] variant: an unrepresentable narrowing, an invalid role, an
/// unrepresentable timestamp, or a mapping/storage disagreement.
pub fn bridge_media_file(
    md: &Metadata,
    mf: &MediaFile,
    stat: &TrackStat,
    now: Option<DateTime<Utc>>,
) -> Result<MediaFileRecord, BridgeError> {
    // Every PRESENT mapping timestamp is parsed before storage availability is
    // considered (P2): an unparseable value is a defect in the caller's data and must
    // be reported as such, not masked by whatever the snapshot happens to say.
    let mapping_birth = mf
        .birth_time
        .as_deref()
        .map(|value| parse_rfc3339(FIELD_BIRTH_TIME, value))
        .transpose()?;

    // Go's storage wrapper already resolved the birth time (`local.go:117-128`); the
    // port performs the same substitution here, with the caller's clock. Resolved
    // before the field table so the mapping cross-check below compares against it.
    let birth_time =
        effective_birth_time(stat, now).ok_or(BridgeError::MissingBirthSubstitute {
            field: FIELD_BIRTH_TIME,
        })?;

    // Go reads these five off the backend properties as `int`
    // (`map_mediafile.go:71-76`); the port must narrow them checked.
    let bit_rate = as_i32_checked(FIELD_BIT_RATE, md.audio_properties().bit_rate)?;
    let sample_rate = as_i32_checked(FIELD_SAMPLE_RATE, md.audio_properties().sample_rate)?;
    let channels = as_i32_checked(FIELD_CHANNELS, md.audio_properties().channels)?;
    let bit_depth = if md.audio_properties().bit_depth > 0 {
        Some(as_i32_checked(
            FIELD_BIT_DEPTH,
            md.audio_properties().bit_depth,
        )?)
    } else {
        // Go only sets BitDepth when > 0 (`map_mediafile.go:73-75`), so a lossy codec
        // stays NULL rather than storing 0 (`map_mediafile_test.go:181,186`).
        None
    };

    let participants = participants_from_credits(&mf.participants)?;
    let named = named_participants(&mf.participants);

    Ok(MediaFileRecord {
        // ---- DB-owned identity and lifecycle (SC-05/DB-14) --------------------
        // Go's MediaFile.ID is empty until the repo's Put resolves it by match
        // (`mediafile_repository.go:160-181`); the batch's id comes from that.
        id: String::new(),
        missing: false, // Go's fresh import is never missing (`phase_1_folders.go:262-266`)
        // `None` (Go zero time) binds NULL, and `put_sql` stamps it to the batch's
        // stamp only when unset (`mediafile_repo.rs` docs, Go `:184-186`).
        created_at: None,

        // ---- mapping facts (map_mediafile.go:22-60) ---------------------------
        album: mf.album.clone(),
        album_artist: mf.album_artist.clone(),
        album_artist_id: mf.album_artist_id.clone(),
        album_id: mf.album_id.clone(),
        artist: mf.artist.clone(),
        artist_id: mf.artist_id.clone(),
        bpm: mf.bpm,
        catalog_num: mf.catalog_num.clone(),
        codec: mf.codec.clone(),
        comment: mf.comment.clone(),
        compilation: mf.compilation,
        date: mf.date.clone(),
        disc_number: mf.disc_number,
        disc_subtitle: mf.disc_subtitle.clone(),
        // Go `float32`; the record column is `real`, so f64 is the column affinity
        // (`mediafile_repo.rs:904-907`).
        duration: f64::from(mf.duration),
        explicit_status: mf.explicit_status.clone(),
        genre: String::new(), // never derived on the scan side — see module docs
        lyrics: mf.lyrics.clone(),
        mbz_recording_id: mf.mbz_recording_id.clone(),
        mbz_release_track_id: mf.mbz_release_track_id.clone(),
        mbz_album_id: mf.mbz_album_id.clone(),
        mbz_release_group_id: mf.mbz_release_group_id.clone(),
        mbz_album_type: mf.mbz_album_type.clone(),
        // Deprecated fields the mapping never fills (`model/metadata/*.go` sets no
        // MbzArtistID/MbzAlbumArtistID/MbzAlbumComment on a MediaFile), so Go's zero
        // is the honest value; the external provider fills them later
        // (`core/external/provider.go:258-262`).
        mbz_artist_id: String::new(),
        mbz_album_artist_id: String::new(),
        mbz_album_comment: String::new(),
        order_album_artist_name: mf.order_album_artist_name.clone(),
        order_album_name: mf.order_album_name.clone(),
        order_artist_name: mf.order_artist_name.clone(),
        order_title: mf.order_title.clone(),
        original_date: mf.original_date.clone(),
        original_year: mf.original_year,
        path: mf.path.clone(),
        pid: mf.pid.clone(),
        // Only `core/stream/decider.go:448-450` writes probe data; a scan writes "".
        probe_data: String::new(),
        release_date: mf.release_date.clone(),
        release_year: mf.release_year,
        rg_album_gain: mf.rg_album_gain,
        rg_album_peak: mf.rg_album_peak,
        rg_track_gain: mf.rg_track_gain,
        rg_track_peak: mf.rg_track_peak,
        sort_album_artist_name: mf.sort_album_artist_name.clone(),
        sort_album_name: mf.sort_album_name.clone(),
        sort_artist_name: mf.sort_artist_name.clone(),
        sort_title: mf.sort_title.clone(),
        suffix: mf.suffix.clone(),
        title: mf.title.clone(),
        track_number: mf.track_number,
        year: mf.year,

        // ---- backend facts, narrowed checked ----------------------------------
        bit_depth,
        bit_rate,
        channels,
        sample_rate,

        // ---- storage facts (Go Info.FileInfo, local.go:90-109) ----------------
        // S-C03: the mapping DTO's Option versions must not disagree with the
        // snapshot the scanner actually statted.
        size: checked_storage_size(mf, stat)?,
        // Resolved above (storage birth, else the injected substitute); the mapping
        // DTO's own value must agree with that resolution.
        birth_time: checked_birth_time(mapping_birth, birth_time)?,
        updated_at: storage_updated_at(mf, stat)?,

        // ---- configured scope --------------------------------------------------
        // `map_mediafile.go:20-21` takes `lib.ID` and the folder id; `map_file`'s
        // `DEFAULT_LIBRARY_ID`/empty folder must never reach a record
        // (decisions.md S4).
        library_id: i64::from(mf.library_id),
        folder_id: mf.folder_id.clone(),

        // ---- participants (map_mediafile.go:84, AllNames for FTS) -------------
        participants,
        participant_names: named,

        // ---- tags (map_mediafile.go:22, :104-112) -----------------------------
        // `DbTags` is the same `name → ordered values` shape Go's `model.Tags` has
        // (`sql_tags.rs:50`), so this is a free conversion, not a re-derivation.
        tags: tags_from_mapping(&mf.tags),

        // ---- hydration-only state: zero means unread, not absent -------------
        library_path: String::new(), // join-only (`model/mediafile.go:40`)
        library_name: String::new(), // join-only
        annotation: Default::default(), // structs:"-", inferred from the field type
        bookmark_position: 0,        // derived, only with a logged-in user
        genres: Vec::new(),          // PostScan derivation (`mediafile_repository.go:64`)
        // Zero == UNRESOLVED (still requestable), NOT known-absent. The only artwork
        // fact set above is `has_cover_art`.
        image: nd_db::artwork_hydration::ItemImage::default(),
        album_image: nd_db::artwork_hydration::ItemImage::default(),

        // The mapping's `Info.HasPicture` (`map_mediafile.go:69`) is the ONLY artwork
        // fact this bridge sets; `image`/`album_image` above stay zero = unresolved.
        has_cover_art: mf.has_cover_art,
    })
}

/// Go `model.Tags` → the column DTO. Both are `map[name][]value`; the port's
/// `BTreeMap` gives deterministic name order (`go_oracle.rs:442-455` documents Go's
/// map-order nondeterminism, which the port pins rather than reproducing).
#[must_use]
pub fn tags_from_mapping(tags: &nd_mapping::mdfile::Tags) -> DbTags {
    tags.clone()
}

/// Translate mapping credits into the `participants` column shape, through the
/// existing checked role bridge.
///
/// Go writes `{id, name, subRole}` only (`persistence/sql_participations.go:13-17`),
/// which is exactly [`Participant`]; sort/order names and MBIDs are not column data
/// (Go re-reads them from the `artist` table in `getParticipants`, `:127-146`).
///
/// An unknown role id is an error, mirroring Go's `Role.UnmarshalText` rejection
/// (`model/participants.go:62-69`): `role_from_id` already refuses
/// `nd_id::ROLE_INVALID`, and silently dropping a credit would lose a name that Go
/// would have persisted.
///
/// # Errors
///
/// [`BridgeError::InvalidRole`] for a role key outside Go's `AllRoles`.
pub fn participants_from_credits(credits: &RoleCredits) -> Result<Participants, BridgeError> {
    let mut out: Participants = BTreeMap::new();
    for (role, list) in credits.iter() {
        let db_role = role_from_id(role).map_err(|source| BridgeError::InvalidRole { source })?;
        let entries = out.entry(db_role).or_default();
        for credit in list {
            entries.push(Participant {
                id: credit.id.clone(),
                name: credit.name.clone(),
                sub_role: credit.sub_role.clone(),
            });
        }
    }
    Ok(out)
}

/// The FTS participant-name input, built from the SAME credits as the JSON
/// (`Participants.AllNames()`, `model/participants.go:175-186`): every participant's
/// name, plus its sort name when non-empty, `sub_role` never a name source, dedup in
/// first-seen order.
///
/// Role iteration is `RoleCredits`' deterministic (role-sorted) order, which stands in
/// for Go's unspecified map-iteration order — the FTS consumers are order-insensitive
/// (`nd_db::album_repo::participant_names` docs).
#[must_use]
pub fn named_participants(credits: &RoleCredits) -> Vec<NamedParticipant> {
    let mut out: Vec<NamedParticipant> = Vec::new();
    for (_role, list) in credits.iter() {
        for credit in list {
            out.push(NamedParticipant {
                id: credit.id.clone(),
                name: credit.name.clone(),
                sort_name: credit.sort_artist_name.clone(),
                sub_role: credit.sub_role.clone(),
            });
        }
    }
    out
}

/// `Participants.AllNames()` over credits — the consumer-side form of
/// [`named_participants`], delegated to the existing `nd-db` port rather than
/// reimplemented here.
#[must_use]
pub fn all_names(credits: &RoleCredits) -> Vec<String> {
    participant_names(&named_participants(credits))
}

/// Go's birth-time resolution, with the clock injected by the caller.
///
/// `localFileInfo.BirthTime` (`core/storage/local/local.go:117-128`) returns the
/// platform's birth time, else a `statx` birth time, else `time.Now()`;
/// `map_mediafile.go:81` then reads the result through `md.BirthTime()`
/// (`model/metadata/metadata.go:90`). The port keeps the storage fact honest
/// ([`TrackStat`] reports `None`) and performs the last-resort arm here:
///
/// - `stat.birth_time` available → that instant; `now` is **ignored** (Go only
///   substitutes when the platform reported nothing);
/// - `stat.birth_time` `None` (i.e. `BirthTimeUnavailable`) → the injected `now`;
/// - both absent → `None`, and the caller must surface
///   [`BridgeError::MissingBirthSubstitute`] rather than read a clock here (D-TIME).
///
/// `media_file.birth_time` is NOT NULL in the DDL and Go's `Put` stamps `CreatedAt`,
/// never `BirthTime` (`persistence/mediafile_repository.go:184-186`), so the record's
/// birth time is purely this resolved value.
#[must_use]
pub fn effective_birth_time(stat: &TrackStat, now: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    stat.birth_time.or(now)
}

/// `size`: the storage snapshot is authoritative; the mapping DTO's `Option` must
/// agree when it is present.
fn checked_storage_size(mf: &MediaFile, stat: &TrackStat) -> Result<i64, BridgeError> {
    // Edition 2021: no let-chain, so match on the Option and compare inside.
    if let Some(mapping) = mf.size {
        if mapping != stat.size {
            return Err(BridgeError::StorageFactMismatch {
                field: FIELD_SIZE,
                mapping: mapping.to_string(),
                storage: stat.size.to_string(),
            });
        }
    }
    Ok(stat.size)
}

/// Cross-check the mapping DTO's birth time against the **resolved** value.
///
/// `resolved` comes from [`effective_birth_time`], so the substitution arm is already
/// applied; the mapping DTO only ever carries a real platform birth time (it is
/// rendered from [`TrackStat::to_mapping_file_stat`]), and a disagreement means the two
/// sides read different files.
fn checked_birth_time(
    mapping: Option<DateTime<Utc>>,
    resolved: DateTime<Utc>,
) -> Result<DateTime<Utc>, BridgeError> {
    // The mapping DTO's birth time is not a second source of truth: `to_media_file`
    // carries it from the same stat the caller statted, so a disagreement means the two
    // were built from different reads. `mapping` is already parsed by the caller, so a
    // malformed string errors before availability is considered (P2).
    if let Some(parsed) = mapping {
        if parsed != resolved {
            return Err(BridgeError::StorageFactMismatch {
                field: FIELD_BIRTH_TIME,
                mapping: parsed.to_rfc3339(),
                storage: resolved.to_rfc3339(),
            });
        }
    }
    Ok(resolved)
}

/// `updated_at`: nullable in the DDL, so an unavailable mtime binds NULL.
///
/// A **present** mapping timestamp is validated first, whatever the storage side says:
/// an unparseable value is a defect in the caller's data, not something to drop because
/// the snapshot happens to report no mtime. Unlike `birth_time`, there is no
/// substitution arm here — Go's wrapper never substitutes an mtime — so a parseable
/// mapping mtime with no storage counterpart is a storage bug and errors
/// (`StorageFactMismatch`), not a permitted omission.
fn storage_updated_at(
    mf: &MediaFile,
    stat: &TrackStat,
) -> Result<Option<DateTime<Utc>>, BridgeError> {
    let mapping = mf
        .updated_at
        .as_deref()
        .map(|value| parse_rfc3339(FIELD_UPDATED_AT, value))
        .transpose()?;
    match (mapping, stat.modified) {
        (Some(parsed), Some(storage)) => {
            if parsed != storage {
                return Err(BridgeError::StorageFactMismatch {
                    field: FIELD_UPDATED_AT,
                    mapping: parsed.to_rfc3339(),
                    storage: storage.to_rfc3339(),
                });
            }
            Ok(Some(storage))
        }
        (Some(parsed), None) => Err(BridgeError::StorageFactMismatch {
            field: FIELD_UPDATED_AT,
            mapping: parsed.to_rfc3339(),
            storage: "unavailable".to_string(),
        }),
        (None, storage) => Ok(storage),
    }
}

/// Parse the RFC 3339 spelling [`TrackStat::to_mapping_file_stat`] produces.
fn parse_rfc3339(field: &'static str, value: &str) -> Result<DateTime<Utc>, BridgeError> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| BridgeError::MalformedTimestamp {
            field,
            value: value.to_string(),
        })
}
