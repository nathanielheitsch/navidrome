//! SC-04 — semantic equality: the port of `model.Album.Equals`
//! (`model/album.go:87-100`) and `model.MediaFile.Hash`/`Equals`/`IsEquivalent`
//! (`model/mediafile.go:230-253`).
//!
//! Pure structural comparison: no database, no clock, and — deliberately — **no hash
//! crate**. Go compares 64-bit `hashstructure` hashes with `IgnoreZeroValue:true` and
//! `ZeroNil:true` plus an xxh3 over the tag and participant ID sequences; this port
//! compares the same field inventory directly. What that proves is stated below and is
//! the contract SC-05/SC-06 rely on: **boolean equivalence on the used field domain, not
//! Go hash bytes.** Two records that Go's `Equals` calls equal are equal here, and two it
//! calls unequal are unequal here, for every field Go includes or ignores (the executed
//! verdicts are cited per arm); it does not claim the hashes are the same number, and
//! nothing anywhere stores or compares a hash.
//!
//! ## What `hashstructure` actually includes (executed, `model/album.go:14-72`,
//! `model/mediafile.go:28-106`)
//!
//! `hashstructure` reflects over the Go struct's fields and honours only the `hash` tag —
//! the `structs:"-"` tags are the `structs` library's concern, not this one. That is why
//! `MediaFile.Genres` (`structs:"-"`, no `hash` tag) IS included (executed `M2` media
//! genres nil vs set → **unequal`) while `Album.Genres` carries `hash:"ignore"` and is
//! NOT (executed `C7` → equal). The asymmetry is real and is reproduced here.
//!
//! ### Album equality
//!
//! * **Included**: `ID`, `LibraryID`, `Name`, `EmbedArtPath`, `AlbumArtist`,
//!   `AlbumArtistID`, `MinYear`, `MaxYear`, `Date`, `MinOriginalYear`, `MaxOriginalYear`,
//!   `OriginalDate`, `ReleaseDate`, `Compilation`, `Comment`, `SongCount`, `Duration`
//!   (floored), `Size`, `Discs`, `SortAlbumName`, `SortAlbumArtistName`, `OrderAlbumName`,
//!   `OrderAlbumArtistName`, `CatalogNum`, `MbzAlbumID`, `MbzAlbumArtistID`,
//!   `MbzAlbumType`, `MbzAlbumComment`, `MbzReleaseGroupID`, `FolderIDs` (as a set),
//!   `ExplicitStatus`, `RGAlbumGain`, `RGAlbumPeak`, `Missing`, `CreatedAt`, `UpdatedAt`.
//!   Executed `C8`-`C12`, `C4`-`C5`, `C10`, `C11`, `C18`: each mutation is reported
//!   changed.
//! * **Ignored** (`hash:"ignore"`): `Genre`, `Genres`, `Tags`, `Participants`,
//!   the five external-metadata fields, `ExternalInfoUpdatedAt`, `ImportedAt`,
//!   `LibraryPath`, `LibraryName`, `Annotations`, `ItemImage`. Executed `C13`-`C16`,
//!   `C19`-`C21` → equal. Because a draft's genre/genres are PostScan-derived
//!   (`album_repository.go:52-57`) and its tags/participants are excluded, a genre-only
//!   tag edit cannot make phase 3 refresh an album row.
//! * `Duration` is floored on BOTH sides before the hash (`album.go:88-89`), so `100.9`
//!   and `100.1` are equal and `100.0` and `99.9` are not (executed `C17`, `C18`; and
//!   `D3`, where `0.4` equals Go's zero).
//! * `FolderIDs` carries `hash:"set"` (`album.go:51`): order- and duplicate-insensitive
//!   (executed `C6`: `[F1,F2] ↔ [F2,F1,F2]` equal), but a nil slice is NOT an empty one
//!   (executed `C7`, `A1`: nil vs `[]` unequal; `A2`: `[] ↔ []` equal).
//! * `Discs` keeps Go's nil-vs-empty asymmetry (`A3`: nil vs `{}` unequal; `C4`: nil vs
//!   `{1:""}` unequal; `C5`: a subtitle change unequal).
//! * `RGAlbumGain`/`RGAlbumPeak` are `*float64`: `IgnoreZeroValue` skips a nil pointer but
//!   hashes `Some(0.0)`, so `nil != Some(0.0)` **in both directions** (executed `C1`, `C2`;
//!   `C3`: `0.0 ↔ 0.0` equal). Plain `Option` equality is exactly that.
//!
//! ### Media equality
//!
//! The **opposite** on tags and participants, because Go hashes them separately
//! (`mediafile.go:236-241`) after excluding the struct fields themselves:
//!
//! * **Included**: every non-ignored scalar of `model.MediaFile` — title, album, artist
//!   and album-artist name/id, `HasCoverArt`, track/disc numbers and disc subtitle, year,
//!   date, original year/date, release year/date, size, suffix, duration, bit rate,
//!   sample rate, bit depth, channels, codec, `Genre`, `Genres`, the sort/order names,
//!   compilation, comment, lyrics, BPM, `ExplicitStatus`, catalog num, the six MBZ ids,
//!   and the four ReplayGain pointers. Executed `M10`, `M11`, `M16`-`M18`, `M2`-`M6`.
//!   `bit_depth`/`bpm`/`rg_*` keep the `None != Some(0)` asymmetry (executed `M4`, `M5`,
//!   `M6`).
//! * **Excluded**: `ID`, `PID`, `LibraryID`, `LibraryPath`, `LibraryName`, `FolderID`,
//!   `Path`, `AlbumID`, `ProbeData`, `Missing`, `BirthTime`, `CreatedAt`, `UpdatedAt`,
//!   `Annotations`, `Bookmarkable`, `ItemImage`, `AlbumImage` (executed `M1`, `M7`-`M9`,
//!   `M12`). `participant_names` and `bookmark_position` are Rust-side derived fields with
//!   no Go hash counterpart (`Participants.AllNames` is an FTS input, not a hashed field),
//!   so they are excluded too.
//! * **Duration is NOT floored** for media (`M19`: `100.9` vs `100.1` → unequal; `M20`
//!   likewise) — only the album floors. It IS quantized to Go's `float32` field type, so
//!   `100.2` and `100.2000001` compare equal while `100.2` and `100.2001` do not (executed
//!   `D1`, `D2`); comparing the `f64` columns bit-for-bit would report the first pair as
//!   changed.
//! * `Tags` enter as Go's `Tags.Hash` (`tag.go:127-136`): an EMPTY map contributes
//!   nothing (`len(t) == 0` → nil), otherwise the **sorted list of tag IDs**, IDs being
//!   `id.NewTagID(lower(name), lower(value))` — one per value, duplicates kept. Executed:
//!   `M14` `{}` vs nil equal; `M15` `{genre:[]}` vs nil **unequal**; `T5` case-folded
//!   names/values equal; `T1` reordering within a name **equal** (the IDs are sorted);
//!   `T3`/`T4` a duplicate value added or collapsed **unequal**; `T2` splitting one name
//!   across two names unequal; `T8`/`T10` a name with no values is indistinguishable from
//!   a different name with no values, and an extra empty name is ignored.
//! * `Participants` enter as Go's `Participants.Hash` (`participants.go:188-199`): per
//!   role, the sorted `subRole:id` list joined with `/`, prefixed by the role, then the
//!   per-role strings sorted and joined. So **only `subRole` + `id` are significant —
//!   names are not** (executed `P3` names swapped → equal), role assignment IS (`P6`),
//!   insertion order is NOT (`P2`), duplicates ARE (`P4`), and nil equals empty (`P5`,
//!   `M13`).
//!
//! ### Declared deviations
//!
//! 1. **No hash, boolean comparison.** Structural equality of the included inventory
//!    stands in for `hashstructure` + xxh3. It is exact for the executed verdicts; it does
//!    not reproduce hash bytes, and nothing in the port stores or compares them.
//! 2. **Tag ID sequence is sorted globally.** `docs/rust-port/subplans/SC-04-go-truth-brief.md`
//!    §D records "tags value reordering within a name → unequal". Re-executing
//!    `Tags.Hash` in this lane says otherwise — `slices.Sort(ids)` (`tag.go:131-132`)
//!    sorts across names, so `{genre:[Rock,Pop]}` and `{genre:[Pop,Rock]}` hash
//!    identically (executed `T1` → equal), while a duplicate value still differs
//!    (`T3`/`T4` → unequal). The port follows the measured code, not the brief line.
//! 3. **Float quantization.** Go hashes `float32` fields (`mediafile.go:64`,
//!    `album.go:38`); the record columns are `real`/`f64`. Media duration is narrowed to
//!    `f32` on both sides (deviation-free reproduction of `D1`/`D2`), and album duration is
//!    narrowed and floored, matching `album.go:88-89`.
//! 4. **`None` == Go's zero.** Every optional column spells Go's zero value; `NULL` and
//!    `0.0` stay distinct exactly where Go keeps nil-pointer and pointer-to-zero distinct.

use std::collections::BTreeMap;

use nd_db::album_repo::AlbumRecord;
use nd_db::mediafile_repo::MediaFileRecord;
use nd_db::sql_participations::{Participants, Role};
use nd_db::sql_tags::DbTags;
use nd_id::new_tag_id;
use nd_utils::files::base_name;

/// Go's `float32` quantization of a `real` column value (`model/mediafile.go:64`), so a
/// stored `f64` compares the way Go's `float32` field does.
#[must_use]
fn as_f32(value: f64) -> f64 {
    f64::from(value as f32)
}

/// `Album.Equals`'s normalization (`model/album.go:88-89`):
/// `float32(math.Floor(float64(a.Duration)))` on both sides.
#[must_use]
fn floored_duration(value: f64) -> f64 {
    f64::from((value as f32).floor())
}

/// `Album.Equals` (`model/album.go:87-100`): the included inventory of
/// `model/album.go:19-72`, after flooring both durations.
///
/// `tags`, `participants`, `participant_names`, `genre`, `genres`, the external-metadata
/// fields, `imported_at`, `library_path`/`library_name`, `annotation` and `image` are all
/// `hash:"ignore"` in Go and are NOT compared here — see the module docs for the executed
/// per-field verdicts. `folder_ids` is compared as a deduplicated set (Go `hash:"set"`),
/// and `discs` / the `rg_*` pointers keep Go's nil-versus-empty asymmetry.
#[must_use]
pub fn album_equals(a: &AlbumRecord, b: &AlbumRecord) -> bool {
    // `hash:"set"` (`album.go:51`): order- and duplicate-insensitive, but a nil slice is
    // not an empty one (executed `C6`, `C7`, `A1`, `A2`).
    let folder_ids_equal = match (a.folder_ids.as_ref(), b.folder_ids.as_ref()) {
        (None, None) => true,
        (Some(left), Some(right)) => set_of(left) == set_of(right),
        _ => false,
    };

    a.id == b.id
        && a.library_id == b.library_id
        && a.name == b.name
        && a.embed_art_path == b.embed_art_path
        && a.album_artist == b.album_artist
        && a.album_artist_id == b.album_artist_id
        && a.min_year == b.min_year
        && a.max_year == b.max_year
        && a.date == b.date
        && a.min_original_year == b.min_original_year
        && a.max_original_year == b.max_original_year
        && a.original_date == b.original_date
        && a.release_date == b.release_date
        && a.compilation == b.compilation
        && a.comment == b.comment
        && a.song_count == b.song_count
        // Floored on both sides (`album.go:88-89`): 100.9 ↔ 100.1 equal, 100.0 ↔ 99.9 not.
        && floored_duration(a.duration) == floored_duration(b.duration)
        && a.size == b.size
        // `Discs` is a plain map field: nil != empty != {1: ""} (executed `A3`, `C4`, `C5`).
        && a.discs == b.discs
        && a.sort_album_name == b.sort_album_name
        && a.sort_album_artist_name == b.sort_album_artist_name
        && a.order_album_name == b.order_album_name
        && a.order_album_artist_name == b.order_album_artist_name
        && a.catalog_num == b.catalog_num
        && a.mbz_album_id == b.mbz_album_id
        && a.mbz_album_artist_id == b.mbz_album_artist_id
        && a.mbz_album_type == b.mbz_album_type
        && a.mbz_album_comment == b.mbz_album_comment
        && a.mbz_release_group_id == b.mbz_release_group_id
        && folder_ids_equal
        && a.explicit_status == b.explicit_status
        // `Option::eq` is exactly Go's nil-pointer-vs-pointer-to-zero behaviour
        // (executed `C1`-`C3`).
        && a.rg_album_gain == b.rg_album_gain
        && a.rg_album_peak == b.rg_album_peak
        && a.missing == b.missing
        && a.created_at == b.created_at
        && a.updated_at == b.updated_at
}

/// `MediaFile.Equals` (`model/mediafile.go:246-248`): `Hash` compared as a value, spelled
/// here as the field inventory plus the two ID sequences Go folds into the hash
/// (`:236-241`).
///
/// The identifier and time fields Go marks `hash:"ignore"` are not compared; tags and
/// participants ARE, through the tag-ID and participant-ID sequences below rather
/// than as structures. Both sides must be DB-read rows: `genre`/`genres` are PostScan-derived
/// (`mediafile_repository.go:49-66`), so a freshly mapped DTO and a stored row are not the
/// same kind of value.
#[must_use]
pub fn media_equals(a: &MediaFileRecord, b: &MediaFileRecord) -> bool {
    a.title == b.title
        && a.album == b.album
        && a.artist == b.artist
        && a.artist_id == b.artist_id
        && a.album_artist == b.album_artist
        && a.album_artist_id == b.album_artist_id
        && a.has_cover_art == b.has_cover_art
        && a.track_number == b.track_number
        && a.disc_number == b.disc_number
        && a.disc_subtitle == b.disc_subtitle
        && a.year == b.year
        && a.date == b.date
        && a.original_year == b.original_year
        && a.original_date == b.original_date
        && a.release_year == b.release_year
        && a.release_date == b.release_date
        && a.size == b.size
        && a.suffix == b.suffix
        // No floor for media (executed `M19`), but Go's field is float32 (executed `D1`).
        && as_f32(a.duration) == as_f32(b.duration)
        && a.bit_rate == b.bit_rate
        && a.sample_rate == b.sample_rate
        && a.bit_depth == b.bit_depth
        && a.channels == b.channels
        && a.codec == b.codec
        && a.genre == b.genre
        // `Genres` is `structs:"-"` but has no `hash:"ignore"` (executed `M2`).
        && a.genres == b.genres
        && a.sort_title == b.sort_title
        && a.sort_album_name == b.sort_album_name
        && a.sort_artist_name == b.sort_artist_name
        && a.sort_album_artist_name == b.sort_album_artist_name
        && a.order_title == b.order_title
        && a.order_album_name == b.order_album_name
        && a.order_artist_name == b.order_artist_name
        && a.order_album_artist_name == b.order_album_artist_name
        && a.compilation == b.compilation
        && a.comment == b.comment
        && a.lyrics == b.lyrics
        && a.bpm == b.bpm
        && a.explicit_status == b.explicit_status
        && a.catalog_num == b.catalog_num
        && a.mbz_recording_id == b.mbz_recording_id
        && a.mbz_release_track_id == b.mbz_release_track_id
        && a.mbz_album_id == b.mbz_album_id
        && a.mbz_release_group_id == b.mbz_release_group_id
        && a.mbz_artist_id == b.mbz_artist_id
        && a.mbz_album_artist_id == b.mbz_album_artist_id
        && a.mbz_album_type == b.mbz_album_type
        && a.mbz_album_comment == b.mbz_album_comment
        && a.rg_album_gain == b.rg_album_gain
        && a.rg_album_peak == b.rg_album_peak
        && a.rg_track_gain == b.rg_track_gain
        && a.rg_track_peak == b.rg_track_peak
        && tag_ids(&a.tags) == tag_ids(&b.tags)
        && participant_ids(&a.participants) == participant_ids(&b.participants)
}

/// `MediaFile.IsEquivalent` (`model/mediafile.go:250-253`): the path's base name without
/// its extension, via [`nd_utils::files::base_name`] (Go `utils.BaseName`). Used for
/// matching missing tracks, so directory and extension are irrelevant (executed:
/// `a/b.mp3 ↔ x/y/b.flac` true, `a/b.mp3 ↔ a/b.mp4` true, `b ↔ b.mp3` true,
/// `a/b/ ↔ a/b` true, `a/b.mp3 ↔ a/b-extra.mp3` false, `"" ↔ "x"` false).
#[must_use]
pub fn is_equivalent(a: &MediaFileRecord, b: &MediaFileRecord) -> bool {
    base_name(&a.path) == base_name(&b.path)
}

/// `Tags.Hash` (`model/tag.go:127-136`) as a value instead of a digest: `None` for the
/// empty map (Go returns nil, contributing nothing), else the **sorted** tag IDs — one per
/// value, duplicates kept, each `id.NewTagID(lower(name), lower(value))`.
///
/// Sorting the IDs is what makes the sequence order-insensitive across and within names
/// (executed `T1`, `T5`) while still distinguishing an added duplicate (`T3`, `T4`) and a
/// cross-name split (`T2`). A name whose value list is empty yields `Some(vec![])`, which
/// is not `None` — exactly Go's `{"genre": []}` vs nil verdict (`M15`, `T7`), and two
/// different empty names both give `Some(vec![])`, as executed (`T8`, `T10`).
#[must_use]
fn tag_ids(tags: &DbTags) -> Option<Vec<String>> {
    if tags.is_empty() {
        return None;
    }
    let mut ids: Vec<String> = Vec::new();
    for (name, values) in tags {
        for value in values {
            ids.push(new_tag_id(name, value));
        }
    }
    // `slices.Sort(ids)` (`:131`): byte-wise, matching Go's string ordering.
    ids.sort();
    Some(ids)
}

/// `Participants.Hash` (`model/participants.go:188-199`) as a value instead of a digest:
/// per role, the sorted `subRole:id` entries; the map's role keys play the part of Go's
/// sorted `"<role>:<ids>"` strings.
///
/// Only `sub_role` and `id` appear, so an artist's name, sort name and MBZ id cannot make
/// two rows differ (executed `P3`); a role change does (`P6`); duplicates do (`P4`);
/// insertion order does not (`P2`); an empty map equals Go's nil map (`P5`, `M13`) because
/// Rust spells both as an empty `BTreeMap`.
#[must_use]
fn participant_ids(participants: &Participants) -> BTreeMap<String, Vec<String>> {
    participants
        .iter()
        .map(|(role, list): (&Role, _)| {
            let mut entries: Vec<String> = list
                .iter()
                .map(|participant| format!("{}:{}", participant.sub_role, participant.id))
                .collect();
            entries.sort();
            (role.as_str().to_string(), entries)
        })
        .collect()
}

/// Deduplicated, ordered spelling of Go's `hash:"set"` slice semantics.
#[must_use]
fn set_of(values: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = values.iter().map(String::as_str).collect();
    out.sort_unstable();
    out.dedup();
    out
}
