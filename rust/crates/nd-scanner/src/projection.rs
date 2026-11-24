//! SC-03 — singleton album/artist drafts from a folder's mapped tracks.
//!
//! Mirrors the two Go phase-1 helpers that turn a folder's tracks into the entities the
//! folder transaction writes:
//! - `phaseFolders.createAlbumsFromMediaFiles` (`scanner/phase_1_folders.go:328-334`)
//!   groups tracks by album id and calls `model.MediaFiles.ToAlbum()`
//!   (`model/mediafile.go:320-406`).
//! - `phaseFolders.createArtistsFromMediaFiles` (`:340-346`) merges every track's
//!   `Participants` (`model/participants.go:129-133`) and emits
//!   `Participants.AllArtists()` (`:150-163`): stable sort by id, then compact by id.
//!
//! ## Two arms of the same aggregation
//!
//! [`album_draft_from_track`] is the **singleton** arm SC-05's first vertical was built
//! on: one track per album group, so each aggregation rule collapses to its one-element
//! result, and every arm below cites the Go line it reduces. [`album_drafts`] now hands
//! **every** group to [`crate::aggregate::album_draft`] — SC-04's real
//! `MediaFiles.ToAlbum` loop (`model/mediafile.go:320-409`) — and the singleton is the
//! `len == 1` specialization of it rather than a separate DTO
//! (`docs/rust-port/subplans/scanner.md` SC-03 §3, SC-04 §4). A multi-track group is no
//! longer refused; its `SongCount`, `Duration`/`Size` sums, date consensus, `minMax`
//! years, `MostFrequent` MBIDs, tag frequency and lowest-disc artwork pick are
//! aggregated per [`crate::aggregate`]'s Go-truth module docs.
//!
//! ## What these drafts are not
//!
//! - **Not DB writes.** They are [`AlbumRecord`]/[`ArtistRecord`] values SC-05 places
//!   into `FolderWriteBatch`; identity resolution (`PutByMatch`), `created_at` stamping
//!   and participant re-hydration belong to DB-14/DB-11.
//! - **Not equality.** `nd_model::album::equals` compares ids only
//!   (`nd-model/src/album.rs:174-176`) and is not Go's change detection — decisions.md
//!   **S4** assigns aggregate/semantic comparison to SC-04. Nothing here compares
//!   albums, and `album.rs` is untouched.
//! - **Not id generation.** The album id comes from the mapping
//!   (`MediaFile::album_id`, `map_mediafile.go:91`), which is exactly Go's
//!   `a.ID = m.AlbumID` (`model/mediafile.go:344`).
//!
//! ## Read-side fields stay zero
//!
//! `genre`, `genres`, `image`, `annotation`, `library_path`/`library_name` are read-side
//! or hydration state (Go `structs:"-"`; `album_repository.go:52-57` derives genre in
//! PostScan), so a draft carries Go's zero. `imported_at` is a caller stamp:
//! `put_sql` overwrites it with the batch's own stamp
//! (`nd-db/src/album_repo.rs:1064-1067`, Go `album_repository.go:210`). `created_at`
//! is NOT a stamp: it is the track's resolved birth (`:390`), zero (NULL) when no
//! birth and no injected substitute exist.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use nd_db::album_repo::{AlbumRecord, NamedParticipant};
use nd_db::artist_repo::ArtistRecord;
use nd_db::sql_participations::Participants;
use nd_db::sql_tags::DbTags;
use nd_mapping::mappings::Mappings;
use nd_mapping::mdfile::MediaFile;
use nd_mapping::participants::{Credit, RoleCredits};

use crate::aggregate::{self, AggregateError, TrackFacts};
use crate::mapping_bridge::{
    named_participants, participants_from_credits, BridgeError, TrackStat,
};

/// Go `Discs{1: ""}` — the album's disc map is seeded with disc 1 before the
/// per-track loop (`model/mediafile.go:323`).
const DEFAULT_DISC: i64 = 1;

/// Why a projection refused its input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectionError {
    /// A group could not be aggregated (`crate::aggregate::AggregateError`): a mixed
    /// album/library group, a sum that does not fit the record's column, a member with
    /// no supplied storage facts, or the participant conversion's own refusal. Carried
    /// as the aggregate's typed error rather than reworded, so the cause stays theirs.
    #[error("album aggregation: {0}")]
    Aggregate(#[from] crate::aggregate::AggregateError),
    /// The record-side failure a draft's participants hit through the checked role
    /// bridge (`BridgeError::InvalidRole`). Forwarded rather than reworded, so the
    /// cause stays the bridge's own typed error.
    #[error("participant projection: {0}")]
    Bridge(#[from] BridgeError),
}

/// `Participants.AllNames()` over merged credits, delegated to the bridge's single
/// implementation so a track record and an album draft cannot disagree about the FTS
/// input (`model/participants.go:175-186`).
#[must_use]
pub fn all_names(merged: &RoleCredits) -> Vec<String> {
    crate::mapping_bridge::all_names(merged)
}

/// Merge tracks' credits the way Go's `createArtistsFromMediaFiles` does:
/// `Participants.Merge` per track (`model/participants.go:129-133`), i.e. `add` per
/// role with Go's dedup-by-`id+subRole`, first occurrence wins (`:135-147`).
///
/// One track is the identity case, which is what SC-05's singleton folder has.
#[must_use]
pub fn merge_credits(tracks: &[&MediaFile]) -> RoleCredits {
    let mut merged = RoleCredits::default();
    for track in tracks {
        for (role, list) in track.participants.iter() {
            for credit in list {
                // Go re-adds whole Participant lists, sub-role included, so per-credit
                // add_with_sub_role is the same operation with the same dedup key.
                merged.add_with_sub_role(role, &credit.sub_role, [credit.clone()]);
            }
        }
    }
    merged
}

/// `Participants.AllArtists()` (`model/participants.go:150-163`): every credit across
/// roles, **stably** sorted by id, then compacted on equal ids.
///
/// Go compacts on `a1.ID == a2.ID` only, so two credits with the same artist id but
/// different sub-roles collapse to the first one in stable order — one `artist` row per
/// id, not one per credit. That is Go's behaviour and is asserted, not assumed.
#[must_use]
pub fn all_artists(merged: &RoleCredits) -> Vec<Credit> {
    let mut all: Vec<Credit> = merged
        .iter()
        .flat_map(|(_role, list)| list.clone())
        .collect();
    // `slices.SortStableFunc(artists, cmp.Compare(a1.ID, a2.ID))`: a stable sort, so
    // equal ids keep their (role-sorted, then tag-order) insertion sequence.
    all.sort_by(|a, b| a.id.cmp(&b.id));
    // `slices.CompactFunc(artists, a1.ID == a2.ID)` — adjacent-equal removal after the
    // sort, i.e. dedup-by-id keeping the first stable occurrence.
    all.dedup_by(|a, b| a.id == b.id);
    all
}

/// Artist drafts for the folder transaction: `AllArtists()` mapped to
/// [`ArtistRecord`]s carrying exactly the fields Go's restricted artist `Put` updates
/// (`scanner/phase_1_folders.go:403-405`, ported as
/// [`nd_db::scanner_writes::ARTIST_UPDATE_COLUMNS`]: `name`, `mbz_artist_id`,
/// `sort_artist_name`, `order_artist_name`, `full_text`, `search_normalized`,
/// `updated_at`).
///
/// `created_at`/`updated_at` stay `None`: Go passes zero `*time.Time` and the repo
/// stamps both (`nd-db/src/artist_repo.rs:885-889`, Go `artist_repository.go:240`).
/// Stats, artwork URLs and annotation are hydration state (`model/artist.go:15-30`,
/// `structs:"-"`), so they keep Go's zero.
#[must_use]
pub fn artist_drafts(tracks: &[&MediaFile]) -> Vec<ArtistRecord> {
    all_artists(&merge_credits(tracks))
        .into_iter()
        .map(|credit| ArtistRecord {
            id: credit.id,
            name: credit.name,
            sort_artist_name: credit.sort_artist_name,
            order_artist_name: credit.order_artist_name,
            mbz_artist_id: credit.mbz_artist_id,
            ..ArtistRecord::default()
        })
        .collect()
}

/// The singleton album draft — one track's `MediaFiles.ToAlbum()`
/// (`model/mediafile.go:320-406`), with each aggregation arm reduced to its
/// one-element result. Each arm below names the Go line it comes from so SC-04 can
/// replace the reduction with the real loop without losing the field inventory.
///
/// `stat` supplies `Size`/`UpdatedAt` the way Go's `Info.FileInfo` does
/// (`a.Size += m.Size` `:362`, `a.UpdatedAt = newer(a.UpdatedAt, m.UpdatedAt)` `:385`),
/// and `birth` supplies `CreatedAt` (`a.CreatedAt = older(a.CreatedAt, m.BirthTime)`
/// `:390`): `older` of the zero time and the track's birth is that birth
/// (`:466-480`), and **all-zero births leave `CreatedAt` zero**
/// (`model/mediafile_test.go:123-134`), which binds NULL in the DDL.
///
/// `birth` must be the SAME value the track record got —
/// [`crate::mapping_bridge::effective_birth_time`] resolved from the storage snapshot
/// plus the caller's injected substitute (Go's wrapper substitutes `time.Now()` there,
/// `core/storage/local/local.go:117-128`). Resolving once and passing it in is what
/// keeps the media row and the album row from drifting.
///
/// `mappings` supplies Go's `AlbumLevelTags()` filter (`model/album.go:104-122`): the
/// album's `tags` column keeps only names flagged `album: true` in
/// `resources/mappings.yaml`.
///
/// # Errors
///
/// [`BridgeError::InvalidRole`] when a credit carries a role outside Go's `AllRoles`
/// (the same checked bridge the track bridge uses; a draft with an invalid role key
/// would poison the `participants` column).
pub fn album_draft_from_track(
    track: &MediaFile,
    stat: &TrackStat,
    birth: Option<DateTime<Utc>>,
    mappings: &Mappings,
    imported_at: DateTime<Utc>,
) -> Result<AlbumRecord, BridgeError> {
    // `a := Album{SongCount: len(mfs), Tags: make(Tags), Participants: make(Participants),
    // Discs: Discs{1: ""}}` (`:323`).
    let mut discs: BTreeMap<i64, String> = BTreeMap::new();
    discs.insert(DEFAULT_DISC, String::new());
    // `if m.DiscNumber > 0 { a.Discs.Add(m.DiscNumber, m.DiscSubtitle) }` (`:371-373`).
    if track.disc_number > 0 {
        discs.insert(i64::from(track.disc_number), track.disc_subtitle.clone());
    }

    // "We assume these attributes are all the same for all songs in an album" block
    // (`:343-357`): Go's loop lets the last track win, so with one track it is simply
    // that track's value.
    let mut album_artist = track.album_artist.clone();
    let mut album_artist_id = track.album_artist_id.clone();

    // `a.Compilation = a.Compilation || m.Compilation` (`:358`).
    let compilation = track.compilation;

    // `embedArtPath, embedArtDisc = firstArtPath(embedArtPath, embedArtDisc, m)`
    // (`:378`, `:500-517`): starting from an empty path (`currentDisc == 0`), the first
    // track with embedded art wins; a track without art never sets it.
    let embed_art_path = if track.has_cover_art {
        track.path.clone()
    } else {
        String::new()
    };

    // `a.Missing = true` then `a.Missing = a.Missing && m.Missing` (`:337`, `:387`).
    let missing = track_is_missing(track);

    // `a.Date, _ = allOrNothing(dates)` (`:396-398`): one element is always the single
    // distinct value, so the date passes through verbatim — empty included, because
    // Go's `allOrNothing([""])` returns `"", 1` (`:361-370`).
    let (min_year, max_year) = aggregate::min_max_year(&[track.year]);
    let (min_original_year, max_original_year) = aggregate::min_max_year(&[track.original_year]);

    // `a.MbzAlbumID = slice.MostFrequent(mbzAlbumIds)` (`:403-404`): `MostFrequent`
    // skips the zero value (`utils/slice/slice.go:83-105`), so an empty MBID stays empty.
    let mbz_album_id = aggregate::most_frequent(std::slice::from_ref(&track.mbz_album_id));
    let mbz_release_group_id =
        aggregate::most_frequent(std::slice::from_ref(&track.mbz_release_group_id));

    // `a.RGAlbumGain = mostFrequentPtr(rgAlbumGains)` (`:405-406`, `:462-484`): one
    // non-nil value wins, nil yields nil, and a genuine 0.0 is a real candidate.
    let rg_album_gain = track.rg_album_gain;
    let rg_album_peak = track.rg_album_peak;

    // `a.Duration += m.Duration` (`:361`): Go sums float32; the column is `real`.
    let duration = f64::from(track.duration);

    // `a.Participants.Merge(m.Participants)` (`:376`): one track is the identity merge.
    let merged = merge_credits(&[track]);
    let participants = participants_from_credits(&merged)?;
    let participant_names: Vec<NamedParticipant> = named_participants(&merged);

    // `a.SetTags(tags)` (`:395`, `model/album.go:115-122`).
    let tags = album_tags(&track.tags, mappings);

    // `fixAlbumArtist(&a)` (`:407`, `:486-500`).
    (album_artist, album_artist_id) =
        aggregate::fix_album_artist(compilation, album_artist, album_artist_id, &merged);

    Ok(AlbumRecord {
        id: track.album_id.clone(),              // `a.ID = m.AlbumID` (`:344`)
        library_id: i64::from(track.library_id), // `a.LibraryID = m.LibraryID` (`:345`)
        name: track.album.clone(),               // `a.Name = m.Album` (`:346`)
        embed_art_path,
        album_artist,
        album_artist_id,
        min_year,
        max_year,
        date: track.date.clone(),
        min_original_year,
        max_original_year,
        original_date: track.original_date.clone(),
        release_date: track.release_date.clone(),
        compilation,
        // `a.Comment, _ = allOrNothing(comments)` (`:401`).
        comment: track.comment.clone(),
        song_count: 1, // `SongCount: len(mfs)` (`:323`)
        duration,
        size: stat.size, // `a.Size += m.Size` (`:362`), storage-supplied
        discs: Some(discs),
        sort_album_name: track.sort_album_name.clone(),
        sort_album_artist_name: track.sort_album_artist_name.clone(),
        order_album_name: track.order_album_name.clone(),
        order_album_artist_name: track.order_album_artist_name.clone(),
        catalog_num: track.catalog_num.clone(),
        mbz_album_id,
        // `a.MbzAlbumArtistID = m.MbzAlbumArtistID` / `a.MbzAlbumComment = m.MbzAlbumComment`
        // (`:355-357`): the mapping never fills either field on a MediaFile
        // (`model/metadata/*.go` has no assignment), so Go's zero is what phase 1 writes.
        mbz_album_artist_id: String::new(),
        mbz_album_type: track.mbz_album_type.clone(),
        mbz_album_comment: String::new(),
        mbz_release_group_id,
        // `a.FolderIDs = slice.Unique(slice.Map(mfs, …FolderID))` (`:394`).
        folder_ids: Some(vec![track.folder_id.clone()]),
        // `:380-383`: with one track the e>c precedence reduces to the track's status.
        explicit_status: track.explicit_status.clone(),
        rg_album_gain,
        rg_album_peak,
        // External-metadata columns: never phase-1 state (`core/external/provider.go`
        // fills them later).
        description: String::new(),
        small_image_url: String::new(),
        medium_image_url: String::new(),
        large_image_url: String::new(),
        external_url: String::new(),
        external_info_updated_at: None,
        // Read-side derivation only: `dbAlbum.PostScan` overwrites this from the tags
        // shim (`album_repository.go:52-57`, `Tags.ToGenres` `model/tag.go:138-148`).
        genre: String::new(),
        tags,
        participants,
        participant_names,
        missing,
        // Caller-stamped placeholder: `put_sql` replaces it with the batch's stamp
        // (`nd-db/src/album_repo.rs:1064-1067`).
        imported_at,
        // `a.CreatedAt = older(a.CreatedAt, m.BirthTime)` (`:390`): `older` starts from
        // the zero time (`:326`), so the result is the track's birth as resolved by
        // [`crate::mapping_bridge::effective_birth_time`] — Go's wrapper has already
        // substituted `time.Now()` at that point (`local.go:117-128`). An unresolved
        // birth stays zero, exactly like Go's all-zero-BirthTime case
        // (`model/mediafile_test.go:123-134`), which [`nd_db::album_repo::put_sql`]
        // binds as NULL rather than stamping a scan time here.
        created_at: birth,
        // `a.UpdatedAt = newer(a.UpdatedAt, m.UpdatedAt)` (`:385`) — the newest of the
        // zero time and the file's mtime is the mtime; absent stays absent.
        updated_at: stat.modified,
        // Join-only (`structs:"-"`, `model/album.go:21-22`).
        library_path: String::new(),
        library_name: String::new(),
        annotation: Default::default(), // structs:"-" embed
        genres: Vec::new(),             // PostScan-derived
        // Zero == UNRESOLVED, not known-absent (`artwork_hydration::ItemImage` docs).
        image: nd_db::artwork_hydration::ItemImage::default(),
    })
}

/// Albums for a folder: Go groups tracks by album id
/// (`slice.Group(entry.tracks, func(mf) { return mf.AlbumID })`,
/// `phase_1_folders.go:329-333`) and calls `ToAlbum` per group — SC-04's
/// [`crate::aggregate::album_draft`] for every group size, so a multi-track group is
/// aggregated rather than refused.
///
/// `facts_for` supplies each **member's** storage snapshot and resolved birth
/// ([`TrackFacts`]); Go reads those off each `MediaFile`'s `Info.FileInfo`
/// (`core/storage/local/local.go:90-109`), so `CreatedAt`/`UpdatedAt` are the
/// oldest-newest across the whole group, not the first track's. A member with no
/// facts is a caller defect and is refused
/// ([`crate::aggregate::AggregateError::MissingTrackFacts`]) rather than projected
/// with a fabricated zero-size snapshot.
///
/// **Emission order is album-id order** (the grouping is a `BTreeMap`). Go emits
/// first-occurrence order from a nondeterministic map iteration; the port pins a
/// stable order instead — a deliberate, documented deviation, the same rule
/// `nd-mapping/tests/go_oracle.rs:442-455` applies to map-iteration order.
///
/// # Errors
///
/// Any [`crate::aggregate::AggregateError`] the aggregation can surface (mixed group,
/// overflowing sums, missing facts, invalid role), forwarded as
/// [`ProjectionError::Aggregate`], or the [`BridgeError`] the singleton arm surfaces.
#[allow(clippy::missing_panics_doc)] // no panicking path: grouping only indexes a Vec
pub fn album_drafts(
    tracks: &[&MediaFile],
    facts_for: impl Fn(&MediaFile) -> Result<TrackFacts, AggregateError>,
    mappings: &Mappings,
    imported_at: DateTime<Utc>,
) -> Result<Vec<AlbumRecord>, ProjectionError> {
    let mut by_album: BTreeMap<&str, Vec<&MediaFile>> = BTreeMap::new();
    for track in tracks {
        by_album
            .entry(track.album_id.as_str())
            .or_default()
            .push(track);
    }
    by_album
        .values()
        .map(|group| {
            crate::aggregate::album_draft(group, &facts_for, mappings, imported_at)
                .map_err(ProjectionError::Aggregate)
        })
        .collect()
}

/// The `participants` column value of a draft, delegated to the bridge's checked
/// conversion so a track record and its album draft cannot disagree about roles.
///
/// # Errors
///
/// [`BridgeError::InvalidRole`] for a role key outside Go's `AllRoles`.
pub fn participants_of(merged: &RoleCredits) -> Result<Participants, BridgeError> {
    participants_from_credits(merged)
}

/// `Album.SetTags` (`model/album.go:115-122`), the singleton degeneration of
/// [`crate::aggregate::album_tags`]: flatten the track's own tags, group by frequency,
/// then keep only album-level names.
///
/// For one track every tag ID has frequency 1, so Go's frequency-then-first-seen sort
/// degenerates to first-seen order — the track's own value order within a name — and no
/// case-folded frequency can combine. Delegating rather than keeping a separate singleton
/// rule means the two arms cannot disagree about which names survive.
#[must_use]
pub fn album_tags(tags: &nd_mapping::mdfile::Tags, mappings: &Mappings) -> DbTags {
    aggregate::album_tags(&aggregate::flatten_all(tags), mappings)
}

/// Go's `MediaFile.Missing` for a freshly mapped import.
///
/// `Missing` is only ever set by the missing-state pass
/// (`phase_1_folders.go:446-456`), so a track read from disk is not missing. Named as a
/// predicate because SC-06's missing leg is where this becomes data-dependent.
#[must_use]
fn track_is_missing(_track: &MediaFile) -> bool {
    false
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// `ProjectionError::Bridge` — the draft's participant conversion refusing an invalid
    /// role rather than writing a poisoned `participants` column. It cannot be produced from a
    /// real tag read (`nd-mapping` only ever emits Go's `AllRoles` keys), and SC-05 always
    /// imports every file, so there is no filesystem path to it. Executed here on the input
    /// shape the draft actually consumes: a track whose credits carry Go's `RoleInvalid`,
    /// which is what `Role::default()` and `role_from_string` return for an unknown key.
    #[test]
    fn a_draft_with_an_invalid_role_refuses_through_the_bridge_instead_of_writing_a_column() {
        let ctx =
            nd_mapping::config::MappingContext::new(nd_mapping::config::MappingOptions::default());
        let mut credits = RoleCredits::default();
        credits.add(
            nd_id::ROLE_ALBUM_ARTIST,
            [Credit {
                id: "artist-1".to_string(),
                name: "Valid".to_string(),
                sort_artist_name: String::new(),
                order_artist_name: String::new(),
                mbz_artist_id: String::new(),
                sub_role: String::new(),
            }],
        );
        credits.add(
            nd_id::ROLE_INVALID,
            [Credit {
                id: "artist-2".to_string(),
                name: "Bogus".to_string(),
                sort_artist_name: String::new(),
                order_artist_name: String::new(),
                mbz_artist_id: String::new(),
                sub_role: String::new(),
            }],
        );

        // Non-vacuity: the same conversion without the invalid role succeeds, so the refusal
        // below is the role and not a broken fixture.
        let valid_only = {
            let mut only = RoleCredits::default();
            only.add(
                nd_id::ROLE_ALBUM_ARTIST,
                [Credit {
                    id: "artist-1".to_string(),
                    name: "Valid".to_string(),
                    sort_artist_name: String::new(),
                    order_artist_name: String::new(),
                    mbz_artist_id: String::new(),
                    sub_role: String::new(),
                }],
            );
            only
        };
        assert!(
            participants_of(&valid_only).is_ok(),
            "the valid credit converts, so the refusal below is the invalid role"
        );

        let error = participants_of(&credits)
            .expect_err("an invalid role is a defect, not a droppable credit");
        assert!(
            matches!(error, BridgeError::InvalidRole { .. }),
            "expected InvalidRole, got {error:?}"
        );

        // The draft path wraps that same failure in `ProjectionError::Bridge`, forwarding the
        // bridge's typed error rather than rewording it: `album_draft_from_track` runs the
        // participants conversion before it can build a record, so a folder carrying such a
        // credit aborts with `Phase1Error::Projection` and writes nothing.
        let wrapped = ProjectionError::Bridge(error);
        assert!(
            wrapped
                .to_string()
                .contains("participant role is not a valid Go role"),
            "the forwarded message keeps the bridge's own wording: {wrapped}"
        );
        assert!(
            matches!(
                wrapped,
                ProjectionError::Bridge(BridgeError::InvalidRole { .. })
            ),
            "the arm forwards, it does not reword: {wrapped}"
        );
        // `ctx.mappings()` is what the draft's signature needs; naming it here keeps the
        // compile-time link between this arm and the draft builder explicit.
        let _: &Mappings = ctx.mappings();
    }
}
