//! SC-04 — pure multi-track album aggregation: the port of `model.MediaFiles.ToAlbum`
//! (`model/mediafile.go:320-409`) and its helper arms.
//!
//! This module is the generalization of SC-03's singleton arms in
//! [`crate::projection`]: every rule below is the real loop rather than the
//! one-element degeneration. It is **pure** — no I/O, no rusqlite, no clock, no
//! dependency edges beyond the ones [`crate::mapping_bridge`] and
//! [`crate::projection`] already take. Go's own contract is the authority; each arm
//! cites the Go line it ports, and the rules marked *executed* below were measured
//! against the real Go code (a scratch `package main` harness plus the tracked Go
//! suites), captured in `docs/rust-port/subplans/SC-04-go-truth-brief.md` §2 and
//! re-measured in this lane.
//!
//! ## Field-by-field Go truth (executed, not inferred)
//!
//! * The group is sorted by **`Path` only** (`:327`). Go's sort **mutates the caller's
//!   slice** — executed `before=[z a m] after=[a m z]`. This port sorts a COPY
//!   (`sort_by_path_copy`), so the caller's order survives: that is a deliberate,
//!   required deviation (`subplans/scanner.md` SC-04 §4), not a fidelity bug.
//! * The "we assume these attributes are all the same" block (`:346-358`) is a plain
//!   assignment per track, so the **last track in path order wins** — including an
//!   empty value overwriting a non-empty one (executed: `{Name:"FromA"} + {Name:"FromB"}`
//!   → `"FromB"`, and a last-track empty `AlbumArtist`/`CatalogNum` clears the first
//!   track's values).
//! * `Duration` accumulates as **`f32` in path order** (`:362`) and `Size` sums as
//!   `i64` (`:363`). Executed: `100.2 + 200.2 + 150.6` → `451` in float32
//!   (`model/mediafile_test.go:101-110`), size `4072`.
//! * `Discs` is seeded `{1: ""}` (`:324`); `DiscNumber > 0` adds `(number, subtitle)`
//!   (`:374-376`), so the **last path-sorted subtitle wins** for a repeated disc and
//!   `DiscNumber <= 0` adds nothing (executed: `{1:"First"},{1:"Second"}` →
//!   `{1:"Second"}`; `{0:"ignored"},{-1}` → `{1:""}`).
//! * `Participants.Merge` (`:378`, `model/participants.go:129-147`) dedups on
//!   `ID + SubRole` with the **first occurrence winning**; the same id under a
//!   different sub-role is a separate entry (executed).
//! * Artwork uses `firstArtPath` (`:381`, `:502-520`): lowest positive disc wins, a
//!   tie on disc breaks by lower `Path`, and a `DiscNumber == 0` art track **resets the
//!   state to disc 0**, so every later art track wins unconditionally (executed:
//!   three disc-0 art tracks → the last by path; `{disc1 no art},{disc0 art},
//!   {disc2 art}` → the disc-2 path).
//! * `ExplicitStatus`: `"e"` beats `"c"` in either order, `"c"` only when the current
//!   value is not `"e"`, and an empty value is a no-op (`:383-387`, executed).
//! * `UpdatedAt` is the **newest** `UpdatedAt` and `CreatedAt` the **oldest**
//!   `BirthTime`, each ignoring zeros (`:389-390`, `:469-480`); an all-zero birth group
//!   leaves `CreatedAt` zero (executed, `model/mediafile_test.go:123-134` and the
//!   three-member `:109` expectation).
//! * `Missing` is the AND of every member, starting `true` (`:341`, `:391`).
//! * `FolderIDs` is first-seen-unique over the **post-sort** order (`:396`,
//!   `utils/slice/slice.go:120-129`; executed `[F1 F2]` from a `[z a m]` group).
//! * `Date`/`OriginalDate`/`ReleaseDate`/`Comment` are `allOrNothing` (`:397-402`,
//!   `:412-421`): zero items → `""`, more than one distinct value → `""`, otherwise the
//!   single value **verbatim, malformed strings included** (executed:
//!   `["not-a-date","not-a-date"]` → `"not-a-date"`; `["", "1985"]` → `""`).
//! * `minMax` years (`:423-433`) seeds **both** bounds from `items[0]`, so an
//!   all-negative group keeps its negative min AND a positive max (executed:
//!   `{-5,1999}` → `-5/1999`; `{0,0,1999}` → `1999/1999`; `{2001,0,1999}` → `1999/2001`;
//!   `{0,0}` → `0/0`). The SC-03 arm seeded `mx` at `0`, which yielded `(-5, 0)` —
//!   a real divergence, fixed here.
//! * `MbzAlbumID`/`MbzReleaseGroupID` use `slice.MostFrequent` (`:403-404`,
//!   `utils/slice/slice.go:83-105`): the zero value is skipped and a tie goes to the
//!   value that first reaches the running maximum (executed `{id2,id1}` → `"id2"`,
//!   `{z,"",z}` → `"z"`, all empty → `""`).
//! * `RGAlbumGain`/`RGAlbumPeak` use `mostFrequentPtr` (`:405-406`, `:439-461`): nils
//!   are skipped, counting is by **dereferenced value**, a genuine `0.0` is a real
//!   candidate, and an all-nil group yields nil (executed: `-8×2` beats `-5×1`;
//!   `0.0×2` beats `1.0×1`; all nil → nil).
//! * `fixAlbumArtist` (`:407`, `:484-498`): a non-compilation album with an empty
//!   `AlbumArtistID` takes `Participants.First(RoleArtist)` (Go's zero `Artist{}` when
//!   the role is absent); a compilation with more than one distinct album-artist id
//!   becomes `Various Artists` / `63sqASlAfjbGMuLP4JhnZU` (executed both arms).
//!
//! ## Tag aggregation
//!
//! `SetTags` (`model/album.go:115-122`) is `TagList.GroupByFrequency()`
//! (`model/tag.go:26-57`) followed by the `AlbumLevelTags()` filter
//! (`model/album.go:104-113`, i.e. `resources/mappings.yaml`'s `album: true` set).
//! Frequency is keyed on the **tag ID** — `id.NewTagID(lower(name), lower(value))`,
//! ported as [`nd_id::new_tag_id`] — so values differing only in case collapse into one
//! ID and their frequencies COMBINE, while the display value is the **last**
//! occurrence's (`model/tag.go:36`). Value order per name is frequency descending, then
//! first-seen index (`model/tag.go:48-53`). Executed: `{genre:[synthwave]}` +
//! `{genre:[Synthwave]}` → `[Synthwave]`; `{Punk}` + `{Rock}` + `{Alternative,Rock}` →
//! `[Rock, Punk, Alternative]`.
//!
//! `Genre`/`Genres` stay **read-side derived** (Go `dbAlbum.PostScan` over
//! `Tags.ToGenres`, `persistence/album_repository.go:52-57`, ported as
//! [`nd_db::sql_tags::tags_to_genres`]). A draft carries Go's zero for both, exactly as
//! SC-03 did, and [`crate::compare::album_equals`] ignores them, so a genre-only tag
//! edit cannot make phase 3 churn an album row.
//!
//! ## Declared deviations
//!
//! 1. **Sort of a copy** — Go mutates its input; this port does not (required by the
//!    subplan, and asserted by `tests/aggregate.rs`).
//! 2. **`MixedGroup` refusal** — Go has no mixed-album/mixed-library check. The subplan
//!    (`SC-04 §4`) asks for a programmer/input error rather than silently combining
//!    groups, so a group whose members disagree on `album_id` or `library_id` is
//!    refused.
//! 3. **`song_count` narrowing** — Go's `SongCount` is a Go `int` (64-bit) while
//!    [`AlbumRecord::song_count`] is `i32`; a group that cannot fit is refused rather
//!    than truncated. `Size` is likewise `checked_add`ed: Go's `int64` sum wraps, this
//!    port refuses.
//! 4. **Deterministic map order** — Go's `Tags`/`Participants` maps iterate in
//!    unspecified order (`nd-mapping/tests/go_oracle.rs:442-455`); the port's
//!    `BTreeMap` pins name-sorted/role-sorted order, which is a stability improvement
//!    the port documents rather than a parity claim.
//! 5. **Stable path sort** — Go's `slices.SortFunc` is unstable, so two members with the
//!    identical path have an arbitrary relative order. Rust's sort is stable, which pins
//!    tag-`firstSeen` tie-breaks for a case Go leaves nondeterministic. Two members
//!    cannot legitimately share a path inside one folder, so this is unreachable in
//!    practice.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use nd_db::album_repo::{AlbumRecord, NamedParticipant};
use nd_db::sql_participations::Participants;
use nd_db::sql_tags::DbTags;
use nd_id::{new_tag_id, ROLE_ALBUM_ARTIST, ROLE_ARTIST, VARIOUS_ARTISTS, VARIOUS_ARTISTS_ID};
use nd_mapping::mappings::Mappings;
use nd_mapping::mdfile::MediaFile;
use nd_mapping::participants::RoleCredits;
use nd_utils::slice::unique;

use crate::mapping_bridge::{
    named_participants, participants_from_credits, BridgeError, TrackStat,
};
use crate::projection::merge_credits;

/// Go `Discs{1: ""}` — the album's disc map is seeded with disc 1 before the per-track
/// loop (`model/mediafile.go:324`).
const DEFAULT_DISC: i64 = 1;

/// One group member's caller-resolved facts: the storage snapshot plus the birth time
/// that member's record got.
///
/// `stat` supplies `Size` and `UpdatedAt` (`a.Size += m.Size` `:363`,
/// `a.UpdatedAt = newer(a.UpdatedAt, m.UpdatedAt)` `:389`); `birth` supplies `CreatedAt`
/// (`a.CreatedAt = older(a.CreatedAt, m.BirthTime)` `:390`). It must be the SAME value
/// the track record received —
/// [`crate::mapping_bridge::effective_birth_time`] resolved once per file from the
/// storage snapshot plus the caller's injected substitute (Go's wrapper substitutes
/// `time.Now()` there, `core/storage/local/local.go:117-128`) — which is what keeps the
/// media row and the album row from drifting.
///
/// Resolution is **per member**, not per album: Go loops the whole group for both time
/// arms, so `CreatedAt` is the oldest non-zero birth across all members and `UpdatedAt`
/// the newest mtime across all members. A single birth per album cannot express that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackFacts {
    /// Storage facts for this member (size, mtime, platform birth).
    pub stat: TrackStat,
    /// The member's resolved birth (`None` = Go's zero `time.Time`).
    pub birth: Option<DateTime<Utc>>,
}

/// Why an aggregation refused, or could not complete, its input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AggregateError {
    /// The members of one group disagree about `album_id` or `library_id`. Go has no
    /// such check (it trusts its caller, `slice.Group` by `AlbumID`); the subplan
    /// (`subplans/scanner.md` SC-04 §4) requires refusing a mixed group as a
    /// programmer/input error instead of silently combining it.
    #[error(
        "album group {album_id} is mixed: track {path} carries album {other_album_id} in library {other_library_id}"
    )]
    MixedGroup {
        /// The album id the group was keyed by.
        album_id: String,
        /// The member that disagrees with it.
        path: String,
        /// That member's own album id.
        other_album_id: String,
        /// That member's own library id.
        other_library_id: i64,
    },
    /// Go's `SongCount` is a 64-bit `int`; the record column is `i32`. A group that does
    /// not fit is refused rather than truncated.
    #[error("group of {tracks} tracks does not fit the record's i32 song_count")]
    SongCountOverflow {
        /// How many tracks the group holds.
        tracks: usize,
    },
    /// Go's `a.Size += m.Size` wraps on `int64` overflow; the port refuses instead of
    /// storing a negative size.
    #[error("size sum of {tracks} tracks overflows i64")]
    SizeOverflow {
        /// How many tracks were summed.
        tracks: usize,
    },
    /// The caller could not supply [`TrackFacts`] for a member. Only the caller's
    /// lookup can produce this: it means a track reached the aggregator without the
    /// storage facts its record was built from.
    #[error("no storage facts supplied for track {path}")]
    MissingTrackFacts {
        /// Logical path of the member with no facts.
        path: String,
    },
    /// The record-side failure the group's participants hit through the checked role
    /// bridge ([`BridgeError::InvalidRole`]). Forwarded rather than reworded, so the
    /// cause stays the bridge's own typed error.
    #[error("participant projection: {0}")]
    Bridge(#[from] BridgeError),
}

/// Go's zero `Album` (`model/mediafile.go:321-323`), which `ToAlbum` returns for an
/// empty group. Executed: `MediaFiles{}.ToAlbum().Equals(Album{}) == true`, with a nil
/// `Discs`, nil `FolderIDs`, nil `Tags` and nil `Participants`.
///
/// `imported_at` is the one field Go leaves zero that the record cannot: it is NOT NULL
/// in the DDL and [`nd_db::album_repo::put_sql`] overwrites it with the batch's own stamp
/// (`nd-db/src/album_repo.rs:1064-1067`), so the caller's stamp is carried here and
/// [`crate::compare::album_equals`] ignores the field anyway, exactly as Go's hash does.
#[must_use]
pub fn zero_album(imported_at: DateTime<Utc>) -> AlbumRecord {
    AlbumRecord {
        id: String::new(),
        library_id: 0,
        name: String::new(),
        embed_art_path: String::new(),
        album_artist: String::new(),
        album_artist_id: String::new(),
        min_year: 0,
        max_year: 0,
        date: String::new(),
        min_original_year: 0,
        max_original_year: 0,
        original_date: String::new(),
        release_date: String::new(),
        compilation: false,
        comment: String::new(),
        song_count: 0,
        duration: 0.0,
        size: 0,
        discs: None,
        sort_album_name: String::new(),
        sort_album_artist_name: String::new(),
        order_album_name: String::new(),
        order_album_artist_name: String::new(),
        catalog_num: String::new(),
        mbz_album_id: String::new(),
        mbz_album_artist_id: String::new(),
        mbz_album_type: String::new(),
        mbz_album_comment: String::new(),
        mbz_release_group_id: String::new(),
        folder_ids: None,
        explicit_status: String::new(),
        rg_album_gain: None,
        rg_album_peak: None,
        description: String::new(),
        small_image_url: String::new(),
        medium_image_url: String::new(),
        large_image_url: String::new(),
        external_url: String::new(),
        external_info_updated_at: None,
        genre: String::new(),
        tags: DbTags::new(),
        participants: Participants::new(),
        participant_names: Vec::new(),
        missing: false,
        imported_at,
        created_at: None,
        updated_at: None,
        library_path: String::new(),
        library_name: String::new(),
        annotation: Default::default(),
        genres: Vec::new(),
        image: nd_db::artwork_hydration::ItemImage::default(),
    }
}

/// `MediaFiles.ToAlbum()` (`model/mediafile.go:320-409`) for one album group.
///
/// `facts_for` resolves the storage snapshot and birth time **per member**; Go reads
/// them off each `MediaFile`'s `Info.FileInfo` (`core/storage/local/local.go:90-109`),
/// so the port takes the same facts once per member rather than once per album.
/// `mappings` supplies Go's `AlbumLevelTags()` filter (`model/album.go:104-122`).
///
/// An empty group is Go's zero [`AlbumRecord`] (nil `Discs`/`FolderIDs`), not an error.
/// The members are sorted by path **on a copy** — the caller's slice keeps its order.
///
/// # Errors
///
/// [`AggregateError::MixedGroup`] for members that disagree on album or library,
/// [`AggregateError::SongCountOverflow`] / [`AggregateError::SizeOverflow`] when a sum
/// does not fit the record's column, [`AggregateError::MissingTrackFacts`] when
/// `facts_for` has no facts for a member, and [`AggregateError::Bridge`] for a credit
/// carrying a role outside Go's `AllRoles`.
pub fn album_draft(
    group: &[&MediaFile],
    facts_for: impl Fn(&MediaFile) -> Result<TrackFacts, AggregateError>,
    mappings: &Mappings,
    imported_at: DateTime<Utc>,
) -> Result<AlbumRecord, AggregateError> {
    // `if len(mfs) == 0 { return Album{} }` (`:321-323`).
    if group.is_empty() {
        return Ok(zero_album(imported_at));
    }

    // Go has no mixed-group guard (`slice.Group` by `AlbumID`,
    // `scanner/phase_1_folders.go:329-333`); refusing is the subplan's requirement.
    let album_id = group[0].album_id.clone();
    let library_id = group[0].library_id;
    for track in group {
        if track.album_id != album_id || track.library_id != library_id {
            return Err(AggregateError::MixedGroup {
                album_id: album_id.clone(),
                path: track.path.clone(),
                other_album_id: track.album_id.clone(),
                other_library_id: i64::from(track.library_id),
            });
        }
    }

    let order = sort_by_path_copy(group);

    // `a := Album{SongCount: len(mfs), Tags: make(Tags), Participants: make(Participants),
    // Discs: Discs{1: ""}}` (`:324`).
    let song_count = i32::try_from(order.len()).map_err(|_| AggregateError::SongCountOverflow {
        tracks: order.len(),
    })?;
    let mut discs: BTreeMap<i64, String> = BTreeMap::new();
    discs.insert(DEFAULT_DISC, String::new());

    // The collectors Go appends per track (`:330-340`).
    let mut years: Vec<i32> = Vec::with_capacity(order.len());
    let mut dates: Vec<String> = Vec::with_capacity(order.len());
    let mut original_years: Vec<i32> = Vec::with_capacity(order.len());
    let mut original_dates: Vec<String> = Vec::with_capacity(order.len());
    let mut release_dates: Vec<String> = Vec::with_capacity(order.len());
    let mut comments: Vec<String> = Vec::with_capacity(order.len());
    let mut mbz_album_ids: Vec<String> = Vec::with_capacity(order.len());
    let mut mbz_release_group_ids: Vec<String> = Vec::with_capacity(order.len());
    let mut rg_album_gains: Vec<Option<f64>> = Vec::with_capacity(order.len());
    let mut rg_album_peaks: Vec<Option<f64>> = Vec::with_capacity(order.len());
    let mut tag_list: Vec<(String, String)> = Vec::new();

    // "We assume these attributes are all the same for all songs in an album" (`:346-358`):
    // plain assignment per track, so the last member in path order wins.
    let mut name = String::new();
    let mut album_artist = String::new();
    let mut album_artist_id = String::new();
    let mut sort_album_name = String::new();
    let mut sort_album_artist_name = String::new();
    let mut order_album_name = String::new();
    let mut order_album_artist_name = String::new();
    let mut catalog_num = String::new();
    let mut mbz_album_type = String::new();

    // `a.Compilation = a.Compilation || m.Compilation` (`:359`) — OR over the group.
    let mut compilation = false;
    // `a.Duration += m.Duration` (`:362`): Go's `Duration` is `float32`
    // (`model/mediafile.go:64`), so the accumulation is float32 in path order and is
    // widened afterwards to the `real` column's `f64`. Accumulating in `f64` here would
    // not reproduce Go's float32 rounding.
    let mut duration: f32 = 0.0;
    // `a.Size += m.Size` (`:363`), checked instead of wrapping.
    let mut size: i64 = 0;
    // `a.Missing = true` then `a.Missing = a.Missing && m.Missing` (`:341`, `:391`).
    let mut missing = true;
    // `embedArtPath, embedArtDisc = firstArtPath(embedArtPath, embedArtDisc, m)` (`:381`).
    let mut embed_art_path = String::new();
    let mut embed_art_disc: i32 = 0;
    // `a.ExplicitStatus` precedence (`:383-387`).
    let mut explicit_status = String::new();
    // `a.UpdatedAt = newer(...)` newest, `a.CreatedAt = older(...)` oldest ignoring
    // zeros (`:389-390`): both start from Go's zero time, spelled `None`.
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut created_at: Option<DateTime<Utc>> = None;

    for track in &order {
        let facts = facts_for(track)?;

        name = track.album.clone(); // `a.Name = m.Album` (`:348`)
        album_artist = track.album_artist.clone();
        album_artist_id = track.album_artist_id.clone();
        sort_album_name = track.sort_album_name.clone();
        sort_album_artist_name = track.sort_album_artist_name.clone();
        order_album_name = track.order_album_name.clone();
        order_album_artist_name = track.order_album_artist_name.clone();
        catalog_num = track.catalog_num.clone();
        mbz_album_type = track.mbz_album_type.clone();

        compilation |= track.compilation;
        duration += track.duration;
        size = size
            .checked_add(facts.stat.size)
            .ok_or(AggregateError::SizeOverflow {
                tracks: order.len(),
            })?;

        years.push(track.year);
        dates.push(track.date.clone());
        original_years.push(track.original_year);
        original_dates.push(track.original_date.clone());
        release_dates.push(track.release_date.clone());
        comments.push(track.comment.clone());
        mbz_album_ids.push(track.mbz_album_id.clone());
        mbz_release_group_ids.push(track.mbz_release_group_id.clone());
        rg_album_gains.push(track.rg_album_gain);
        rg_album_peaks.push(track.rg_album_peak);

        // `if m.DiscNumber > 0 { a.Discs.Add(m.DiscNumber, m.DiscSubtitle) }` (`:374-376`):
        // `Discs.Add` is a plain assignment (`model/album.go:126-128`), so the last
        // member in path order owns that disc's subtitle.
        if track.disc_number > 0 {
            discs.insert(i64::from(track.disc_number), track.disc_subtitle.clone());
        }

        // `tags = append(tags, m.Tags.FlattenAll()...)` (`:377`, `model/tag.go:111-119`).
        for (tag_name, values) in &track.tags {
            let lower = nd_utils::str::to_lower(tag_name);
            for value in values {
                tag_list.push((lower.clone(), value.clone()));
            }
        }

        // `a.EmbedArtPath`, `:381` — lowest positive disc, then lowest path, with the
        // disc-0 state reset.
        (embed_art_path, embed_art_disc) = first_art_path(&embed_art_path, embed_art_disc, track);

        if track.explicit_status == "c" && explicit_status != "e" {
            explicit_status = "c".to_string();
        } else if track.explicit_status == "e" {
            explicit_status = "e".to_string();
        }

        updated_at = newer(updated_at, facts.stat.modified);
        created_at = older(created_at, facts.birth);
        missing = missing && track_is_missing(track);
    }
    // The post-loop arms Go runs once the member loop is done (`:397-406`). Every
    // collector is already in path order, which is what makes their tie-breaks
    // deterministic.
    let (min_year, max_year) = min_max_year(&years);
    let (min_original_year, max_original_year) = min_max_year(&original_years);
    // `a.Date, a.OriginalDate, a.ReleaseDate, a.Comment = allOrNothing(...)` (`:397-402`).
    let date = all_or_nothing(&dates);
    let original_date = all_or_nothing(&original_dates);
    let release_date = all_or_nothing(&release_dates);
    let comment = all_or_nothing(&comments);
    // `a.MbzAlbumID, a.MbzReleaseGroupID = slice.MostFrequent(...)` (`:403-404`).
    let mbz_album_id = most_frequent(&mbz_album_ids);
    let mbz_release_group_id = most_frequent(&mbz_release_group_ids);
    // `a.RGAlbumGain, a.RGAlbumPeak = mostFrequentPtr(...)` (`:405-406`).
    let rg_album_gain = most_frequent_ptr(&rg_album_gains);
    let rg_album_peak = most_frequent_ptr(&rg_album_peaks);

    // `a.Participants.Merge(m.Participants)` (`:378`): the merge is the credits merge
    // SC-03 already ports (`model/participants.go:129-147`, dedup by `id+subRole`,
    // first occurrence wins), applied over the whole path-sorted group.
    let merged = merge_credits(&order);
    let participants = participants_from_credits(&merged).map_err(AggregateError::Bridge)?;
    let participant_names: Vec<NamedParticipant> = named_participants(&merged);

    // `a.SetTags(tags)` (`:395`, `model/album.go:115-122`).
    let tags = album_tags(&tag_list, mappings);

    // `fixAlbumArtist(&a)` (`:407`, `:484-498`).
    let (album_artist, album_artist_id) =
        fix_album_artist(compilation, album_artist, album_artist_id, &merged);

    Ok(AlbumRecord {
        id: album_id,                      // `a.ID = m.AlbumID` (`:347`)
        library_id: i64::from(library_id), // `a.LibraryID = m.LibraryID` (`:348`)
        name,
        embed_art_path,
        album_artist,
        album_artist_id,
        min_year,
        max_year,
        date,
        min_original_year,
        max_original_year,
        original_date,
        release_date,
        compilation,
        comment,
        song_count,
        duration: f64::from(duration),
        size,
        discs: Some(discs),
        sort_album_name,
        sort_album_artist_name,
        order_album_name,
        order_album_artist_name,
        catalog_num,
        mbz_album_id,
        // `a.MbzAlbumArtistID = m.MbzAlbumArtistID` / `a.MbzAlbumComment =
        // m.MbzAlbumComment` (`:355-357`) are Go's LAST-WINS legs, but this aggregate's
        // input is the mapping DTO (`nd_mapping::MediaFile`), which has no artist-id or
        // comment field at all, and Go's mapping side never fills them either
        // (`model/metadata/*.go` has no assignment; the only non-test writer is
        // `plugins/host_matcher.go:125`). So Go's zero is what phase 1 writes here.
        // NOT a general parity waiver: phase 3 aggregates hydrated rows, where
        // `nd-db/src/hydrate.rs:2734,2736` supplies real values and Go propagates them —
        // see `decisions.md` D-SC04-MBZ, which makes last-wins an SC-08 obligation.
        mbz_album_artist_id: String::new(),
        mbz_album_type,
        mbz_album_comment: String::new(),
        mbz_release_group_id,
        // `a.FolderIDs = slice.Unique(slice.Map(mfs, …FolderID))` (`:396`): first-seen
        // unique over the POST-SORT order, and `Some` for a non-empty group (Go's
        // non-nil slice; the nil spelling belongs to the empty group only).
        folder_ids: Some(unique(
            &order
                .iter()
                .map(|track| track.folder_id.clone())
                .collect::<Vec<String>>(),
        )),
        explicit_status,
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
        // Read-side derivation only: `dbAlbum.PostScan` overwrites these from the tags
        // shim (`album_repository.go:52-57`, `Tags.ToGenres` `model/tag.go:138-148`).
        genre: String::new(),
        tags,
        participants,
        participant_names,
        missing,
        // Caller-stamped placeholder: `put_sql` replaces it with the batch's own stamp
        // (`nd-db/src/album_repo.rs:1064-1067`).
        imported_at,
        created_at,
        updated_at,
        // Join-only (`structs:"-"`, `model/album.go:21-22`).
        library_path: String::new(),
        library_name: String::new(),
        annotation: Default::default(), // structs:"-" embed
        genres: Vec::new(),             // PostScan-derived
        // Zero == UNRESOLVED, not known-absent (`artwork_hydration::ItemImage` docs).
        image: nd_db::artwork_hydration::ItemImage::default(),
    })
}

/// `slices.SortFunc(mfs, cmp.Compare(a.Path, b.Path))` (`model/mediafile.go:327`) applied
/// to a COPY of the group's references.
///
/// Go sorts its receiver in place — executed: `before=[z a m]`, `after=[a m z]` — and the
/// subplan requires the caller's slice survive (`subplans/scanner.md` SC-04 §4). Only the
/// order is copied: the `&MediaFile` references are shared, no track is cloned.
///
/// Sorting is what makes the order-dependent arms (`firstArtPath`'s path tie-break,
/// `allOrNothing`'s first-seen value, `MostFrequent`'s tie-break, the disc-subtitle
/// overwrite and the "assume all the same" last-wins block) deterministic.
/// [`std::vec::Vec::sort_by`] is stable; Go's `SortFunc` is not, which is deviation 5 in
/// the module docs.
#[must_use]
fn sort_by_path_copy<'a>(group: &[&'a MediaFile]) -> Vec<&'a MediaFile> {
    let mut order: Vec<&MediaFile> = group.to_vec();
    // `cmp.Compare` on Go strings is a byte-wise comparison; `str::cmp` is byte-wise too.
    order.sort_by(|a, b| a.path.cmp(&b.path));
    order
}

/// `allOrNothing` (`model/mediafile.go:412-421`): the value only when every member
/// agrees. Zero items → `""`; more than one distinct value → `""`; otherwise the single
/// value **verbatim**, malformed dates and comments included (executed:
/// `["not-a-date","not-a-date"]` → `"not-a-date"`, `["", "1985"]` → `""`).
#[must_use]
fn all_or_nothing(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let distinct = unique(items);
    if distinct.len() != 1 {
        return String::new();
    }
    // `distinct.len() == 1`, so the head exists.
    distinct.into_iter().next().unwrap_or_default()
}

/// `minMax` (`model/mediafile.go:423-433`): **both** bounds start at `items[0]`, the max
/// is the plain maximum, and the min ignores zeros unless every value is zero.
///
/// Executed: `{0,0,1999}` → `1999/1999`, `{2001,0,1999}` → `1999/2001`, `{-5,1999}` →
/// `-5/1999`, `{0,0}` → `0/0`. Seeding `mx` at `0` — the SC-03 arm — yields `(-5, 0)` for
/// a negative group, which is why the seeding is Go's, not `0`.
#[must_use]
pub(crate) fn min_max_year(years: &[i32]) -> (i32, i32) {
    let seed = years.first().copied().unwrap_or_default();
    let mut mn = seed;
    let mut mx = seed;
    for &value in years {
        mx = mx.max(value);
        if mn == 0 {
            mn = value;
        } else if value > 0 {
            mn = mn.min(value);
        }
    }
    (mn, mx)
}

/// `slice.MostFrequent` (`utils/slice/slice.go:83-105`): the zero value is skipped, so an
/// empty id never wins, and a tie goes to the value that first reaches the running
/// maximum (executed `{id2,id1}` → `"id2"`; `{z,"",z}` → `"z"`; all empty → `""`).
#[must_use]
pub(crate) fn most_frequent(values: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut best: Option<&str> = None;
    let mut best_count = 0usize;
    for value in values {
        if value.is_empty() {
            continue;
        }
        let n = counts.entry(value.as_str()).or_insert(0);
        *n += 1;
        if *n > best_count {
            best_count = *n;
            best = Some(value.as_str());
        }
    }
    best.unwrap_or_default().to_string()
}

/// `mostFrequentPtr` (`model/mediafile.go:439-461`): most common non-`None` value,
/// counted by **dereferenced value** so a genuine `0.0` is a real candidate —
/// `most_frequent` skips the zero value and would drop it. All-`None` → `None`.
/// Executed: `-8×2` beats `-5×1`; `0.0×2` beats `1.0×1`; all nil → nil.
///
/// A `Vec` of `(value, count)` pairs replaces Go's `map[float64]int`; the scan compares
/// with `==`, so a `NaN` would never match itself and would count as a fresh entry.
/// ReplayGain values reach this only through the tag parser, which cannot produce a NaN,
/// so the divergence is unreachable rather than papered over.
#[must_use]
pub(crate) fn most_frequent_ptr(values: &[Option<f64>]) -> Option<f64> {
    let mut counts: Vec<(f64, usize)> = Vec::new();
    let mut best: Option<f64> = None;
    let mut best_count = 0usize;
    for value in values.iter().flatten() {
        let n = match counts.iter_mut().find(|(seen, _)| seen == value) {
            Some(entry) => {
                entry.1 += 1;
                entry.1
            }
            None => {
                counts.push((*value, 1));
                1
            }
        };
        if n > best_count {
            best_count = n;
            best = Some(*value);
        }
    }
    // `if bestCount == 0 { return nil }` (`:458-460`).
    if best_count == 0 {
        return None;
    }
    best
}

/// `newer` (`model/mediafile.go:463-468`) over Go's zero-time arms: `newer(zero, x)` is
/// `x`, `newer(x, zero)` is `x`, otherwise the later instant. `None` is Go's zero time.
#[must_use]
fn newer(current: Option<DateTime<Utc>>, next: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (current, next) {
        (None, value) => value,
        (Some(current), None) => Some(current),
        (Some(current), Some(value)) => Some(current.max(value)),
    }
}

/// `older` (`model/mediafile.go:469-480`): a zero first argument takes the second, a zero
/// second argument is ignored, otherwise the earlier instant. This is why an all-zero
/// birth group leaves `CreatedAt` zero (`None`) rather than the epoch.
#[must_use]
fn older(current: Option<DateTime<Utc>>, next: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (current, next) {
        (None, value) => value,
        (Some(current), None) => Some(current),
        (Some(current), Some(value)) => Some(current.min(value)),
    }
}

/// `firstArtPath` (`model/mediafile.go:502-520`): the member with embedded art and the
/// lowest positive disc number, ties broken by lower `Path`.
///
/// The state reset is the executed quirk: a member with `DiscNumber == 0` returns
/// `(path, 0)`, so `currentDisc` becomes `0` and the `currentDisc == 0` arm then lets
/// **every later** art-bearing member win outright. Executed: three disc-0 art members →
/// the last by path; `{disc1 no art}, {disc0 art}, {disc2 art}` → the disc-2 path.
#[must_use]
fn first_art_path(current_path: &str, current_disc: i32, track: &MediaFile) -> (String, i32) {
    if !track.has_cover_art {
        return (current_path.to_string(), current_disc);
    }
    if current_disc == 0 || (track.disc_number < current_disc && track.disc_number > 0) {
        return (track.path.clone(), track.disc_number);
    }
    if track.disc_number == current_disc
        && (track.path.as_str() < current_path || current_path.is_empty())
    {
        return (track.path.clone(), track.disc_number);
    }
    (current_path.to_string(), current_disc)
}

/// `fixAlbumArtist` (`model/mediafile.go:484-498`): a non-compilation album with no
/// album-artist id falls back to the first artist credit; a compilation whose album
/// artists carry more than one distinct id becomes `Various Artists`.
///
/// Executed both arms, including `{X1, X2}` → `"Various Artists"` /
/// [`nd_id::VARIOUS_ARTISTS_ID`].
#[must_use]
pub fn fix_album_artist(
    compilation: bool,
    album_artist: String,
    album_artist_id: String,
    merged: &RoleCredits,
) -> (String, String) {
    if !compilation {
        if album_artist_id.is_empty() {
            // `artist := a.Participants.First(RoleArtist)` — Go's zero `Artist{}` when
            // the role is absent, i.e. an empty name and id.
            let artist = merged.first(ROLE_ARTIST).cloned().unwrap_or_default();
            return (artist.name, artist.id);
        }
        return (album_artist, album_artist_id);
    }
    // `slice.Unique(slice.Map(a.Participants[RoleAlbumArtist], …ID))` (`:494-496`).
    let ids: Vec<&str> = merged
        .get(ROLE_ALBUM_ARTIST)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|credit| credit.id.as_str())
        .collect();
    if unique(&ids).len() > 1 {
        (VARIOUS_ARTISTS.to_string(), VARIOUS_ARTISTS_ID.to_string())
    } else {
        (album_artist, album_artist_id)
    }
}

/// Go's `Tags.FlattenAll()` (`model/tag.go:111-119`) over a tag map: one `(lowercased
/// name, verbatim value)` per value, in stored order per name.
///
/// Name order: Go's map iteration is nondeterministic; the port's `BTreeMap` pins
/// name-sorted order (`nd-mapping/tests/go_oracle.rs:442-455` records that rule). Only
/// the per-name value order is semantically meaningful downstream, because
/// [`group_by_frequency`] sorts each name's ids by frequency and first-seen index.
#[must_use]
pub fn flatten_all(tags: &nd_mapping::mdfile::Tags) -> Vec<(String, String)> {
    let mut flat: Vec<(String, String)> = Vec::new();
    for (name, values) in tags {
        let lower = nd_utils::str::to_lower(name);
        for value in values {
            flat.push((lower.clone(), value.clone()));
        }
    }
    flat
}

/// `TagList.GroupByFrequency()` (`model/tag.go:26-57`), keyed on the **tag ID**.
///
/// The frequency key is `id.NewTagID(lower(name), lower(value))` = [`nd_id::new_tag_id`]
/// (`model/tag.go:63-75`, `model/id/id.go:46-48`), so two values differing only in case
/// share one ID and their frequencies COMBINE, while the display value is the LAST
/// occurrence's for that ID (`model/tag.go:36`). Value order within a name is frequency
/// descending, then first-seen index (`model/tag.go:48-53`); a tie therefore keeps
/// first-seen order, and each id's `firstSeen` is unique, so the comparator is a total
/// order and Go's unstable `SortFunc` cannot disagree with the port.
///
/// Executed: `{Punk}` + `{Rock}` + `{Alternative, Rock}` → `[Rock, Punk, Alternative]`;
/// `{synthwave}` + `{Synthwave}` → `[Synthwave]`; a same-track duplicate collapses.
#[must_use]
pub fn group_by_frequency(flat: &[(String, String)]) -> DbTags {
    // `grouped[name][id] = frequency`, plus the first-seen index and the last display
    // value of each id.
    let mut grouped: BTreeMap<&str, Vec<(String, usize)>> = BTreeMap::new();
    let mut first_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for (index, (name, value)) in flat.iter().enumerate() {
        let id = new_tag_id(name, value);
        let counts = grouped.entry(name.as_str()).or_default();
        match counts.iter_mut().find(|(seen, _)| seen == &id) {
            Some((_, count)) => *count += 1,
            None => counts.push((id.clone(), 1)),
        }
        // `if _, ok := firstSeen[t.ID]; !ok { firstSeen[t.ID] = i }` (`:37-39`).
        first_seen.entry(id.clone()).or_insert(index);
        // `values[t.ID] = t.TagValue` (`:36`) — the last occurrence owns the display value.
        values.insert(id, value.clone());
    }

    let mut out: DbTags = BTreeMap::new();
    for (name, mut counts) in grouped {
        // `cmp.Or(cmp.Compare(counts[b], counts[a]), cmp.Compare(firstSeen[a], firstSeen[b]))`
        // (`model/tag.go:48-53`): frequency descending, then first-seen ascending.
        counts.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| first_seen[a.0.as_str()].cmp(&first_seen[b.0.as_str()]))
        });
        out.insert(
            name.to_string(),
            counts
                .iter()
                .map(|(id, _)| values[id.as_str()].clone())
                .collect(),
        );
    }
    out
}

/// `Album.SetTags` (`model/album.go:115-122`): [`group_by_frequency`] then drop every
/// name Go's `AlbumLevelTags()` does not contain (`model/album.go:104-113`), i.e. every
/// mapping without `album: true` in `resources/mappings.yaml`.
///
/// `mappings.all()` is the merged table Go builds `AlbumLevelTags` over, and its keys are
/// already lowercased (`nd-mapping/src/mappings.rs:462-478`), which is the same key space
/// [`flatten_all`] produces.
#[must_use]
pub fn album_tags(flat: &[(String, String)], mappings: &Mappings) -> DbTags {
    let mut out: DbTags = BTreeMap::new();
    for (name, values) in group_by_frequency(flat) {
        // `AlbumLevelTags()` is built over all mappings (`model/album.go:104-111`).
        let Some(conf) = mappings.all().get(&name) else {
            continue;
        };
        if !conf.album {
            continue;
        }
        out.insert(name, values);
    }
    out
}

/// Go's `MediaFile.Missing` for a freshly mapped import.
///
/// `Missing` is only ever set by the missing-state pass
/// (`scanner/phase_1_folders.go:446-456`), so a track read from disk is not missing.
/// Named as a predicate because SC-06's missing leg is where this becomes data-dependent,
/// and because Go's album `Missing` is the AND over the group (`:391`) — one missing
/// member makes the album not-missing only when every member is missing.
#[must_use]
fn track_is_missing(_track: &MediaFile) -> bool {
    false
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// Non-vacuity of [`sort_by_path_copy`]: the sort actually reorders the aggregate
    /// order while the caller's slice keeps its own order. Executed Go truth: Go's sort
    /// mutates its receiver (`before=[z a m] after=[a m z]`).
    #[test]
    fn path_sort_reorders_the_copy_and_not_the_input() {
        let mk = |path: &str| MediaFile {
            path: path.to_string(),
            ..MediaFile::default()
        };
        let (a, b, c) = (mk("z"), mk("a"), mk("m"));
        let input: Vec<&MediaFile> = vec![&a, &b, &c];

        let order = sort_by_path_copy(&input);
        assert_eq!(
            order.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(),
            ["a", "m", "z"],
            "sorted by Path, matching Go's comparator"
        );
        assert_eq!(
            input.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(),
            ["z", "a", "m"],
            "the caller's slice is NOT reordered (Go mutates; this port must not)"
        );
    }

    /// `minMax` seeding, executed against `model/mediafile.go:423-433`.
    #[test]
    fn min_max_year_reproduces_go_seeding() {
        assert_eq!(min_max_year(&[0, 0, 1999]), (1999, 1999));
        assert_eq!(min_max_year(&[2001, 0, 1999]), (1999, 2001));
        assert_eq!(min_max_year(&[-5, 1999]), (-5, 1999));
        assert_eq!(min_max_year(&[0, 0]), (0, 0));
        // The SC-03 arm's bug: a seeded `mx = 0` yielded `(-5, 0)` here.
        assert_ne!(min_max_year(&[-5, 1999]), (-5, 0));
        assert_eq!(
            min_max_year(&[]),
            (0, 0),
            "Go would panic; the port cannot be called"
        );
    }

    /// `allOrNothing` (`:412-421`): malformed-but-equal passes through verbatim, and any
    /// disagreement — including an empty value among non-empty ones — yields `""`.
    #[test]
    fn all_or_nothing_is_verbatim_or_empty() {
        assert_eq!(all_or_nothing(&[]), "");
        assert_eq!(
            all_or_nothing(&["not-a-date".to_string(), "not-a-date".to_string()]),
            "not-a-date"
        );
        assert_eq!(all_or_nothing(&["".to_string(), "1985".to_string()]), "");
        assert_eq!(all_or_nothing(&["".to_string(), "".to_string()]), "");
        assert_eq!(all_or_nothing(&["1985".to_string()]), "1985");
    }

    /// `mostFrequentPtr` (`:439-461`): a genuine `0.0` beats a single `1.0`, `-8×2` beats
    /// `-5×1`, and all-`None` is `None`.
    #[test]
    fn most_frequent_ptr_counts_values_not_nullness() {
        assert_eq!(
            most_frequent_ptr(&[Some(-8.0), Some(-8.0), Some(-5.0)]),
            Some(-8.0)
        );
        assert_eq!(most_frequent_ptr(&[Some(0.0), Some(0.0)]), Some(0.0));
        assert_eq!(
            most_frequent_ptr(&[Some(1.0), Some(0.0), Some(0.0)]),
            Some(0.0)
        );
        assert_eq!(most_frequent_ptr(&[None, None]), None);
        assert_eq!(most_frequent_ptr(&[]), None);
    }

    /// `slice.MostFrequent` (`utils/slice/slice.go:83-105`): zero value skipped, tie goes
    /// to the first value reaching the running max.
    #[test]
    fn most_frequent_skips_zero_and_keeps_first_max() {
        assert_eq!(
            most_frequent(&["id2".to_string(), "id1".to_string()]),
            "id2"
        );
        assert_eq!(
            most_frequent(&["z".to_string(), String::new(), "z".to_string()]),
            "z"
        );
        assert_eq!(most_frequent(&[String::new(), String::new()]), "");
    }

    /// The `firstArtPath` state reset (`:502-520`), asserted directly rather than only
    /// through a whole aggregate.
    #[test]
    fn first_art_path_resets_on_disc_zero() {
        let art = |path: &str, disc: i32| MediaFile {
            path: path.to_string(),
            disc_number: disc,
            has_cover_art: true,
            ..MediaFile::default()
        };
        let mut path = String::new();
        let mut disc = 0;
        for m in [art("a", 0), art("b", 0), art("c", 0)] {
            (path, disc) = first_art_path(&path, disc, &m);
        }
        assert_eq!(
            path, "c",
            "a disc-0 member resets the state, so the last wins"
        );
        assert_eq!(disc, 0);

        let no_art = MediaFile {
            path: "a".to_string(),
            disc_number: 1,
            ..MediaFile::default()
        };
        let mut path = String::new();
        let mut disc = 0;
        for m in [&no_art, &art("b", 0), &art("c", 2)] {
            (path, disc) = first_art_path(&path, disc, m);
        }
        assert_eq!(
            path, "c",
            "executed Go vector: the disc-2 path wins after a reset"
        );
    }
}
