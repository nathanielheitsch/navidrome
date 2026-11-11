//! Port of `model/mediafile.go:27-107` — the fields `ToMediaFile` fills in.
//!
//! Deliberate narrowing: this is the **mapping output** DTO, not the entity DTO
//! owned by #06 (`nd-model::entity::Album`/`Artist`). It carries only what
//! `model/metadata/map_mediafile.go` writes; repository hydration, annotations,
//! and the wire shape are #05/#06 concerns.
//!
//! Storage-derived fields (`Size`, `BirthTime`, `UpdatedAt`, Go
//! `map_mediafile.go:80-82`) are `Option`: the tag reader does not supply them
//! (`core/storage/local/local.go:90-109` fills Go's `Info.FileInfo`), so they
//! stay absent instead of persisting a fabricated 0/epoch.

use std::collections::BTreeMap;

use crate::participants::{Credit, RoleCredits};

/// Go `model.DefaultLibraryID` (`model/library.go:68`).
pub const DEFAULT_LIBRARY_ID: i32 = 1;

/// Cleaned tags kept on the entity (Go `model.Tags`, `model/tag.go:79`).
pub type Tags = BTreeMap<String, Vec<String>>;

/// One movement, derived from the movement tags
/// (`model/mediafile.go` `Movements()`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Movement {
    /// Movement name (`TAG_MOVEMENT_NAME`).
    pub name: String,
    /// Movement number, 0 when absent.
    pub number: i32,
    /// Movement total, 0 when absent.
    pub count: i32,
}

/// A mapped track (Go `model.MediaFile`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaFile {
    /// Library this entity belongs to (Go `LibraryID`, `map_mediafile.go:20`).
    pub library_id: i32,
    /// Parent folder id, supplied by the scanner (`map_mediafile.go:21`).
    pub folder_id: String,
    /// Persistent id (Go `PID`, `map_mediafile.go:90`).
    pub pid: String,
    /// Album id (Go `AlbumID`, `map_mediafile.go:91`).
    pub album_id: String,

    /// Display title (`map_mediafile.go:26`).
    pub title: String,
    /// Display album name (`map_mediafile.go:27`).
    pub album: String,
    /// Sort title (`map_mediafile.go:28`).
    pub sort_title: String,
    /// Sort album name (`map_mediafile.go:29`).
    pub sort_album_name: String,
    /// Sorting key of the title (`map_mediafile.go:30`).
    pub order_title: String,
    /// Sorting key of the album, articles removed (`map_mediafile.go:31`).
    pub order_album_name: String,
    /// Compilation flag (`map_mediafile.go:32`).
    pub compilation: bool,

    /// Track number; the `/total` form is parsed but only the number is stored
    /// (`map_mediafile.go:35`).
    pub track_number: i32,
    /// Disc number (`map_mediafile.go:36`).
    pub disc_number: i32,
    /// Disc subtitle (`map_mediafile.go:37`).
    pub disc_subtitle: String,
    /// Catalog number (`map_mediafile.go:38`).
    pub catalog_num: String,
    /// Comment (`map_mediafile.go:39`).
    pub comment: String,
    /// Rounded BPM; absent when the tag is missing, zero, negative,
    /// unparseable or out of int32 (`map_mediafile.go:40-44`).
    pub bpm: Option<i32>,
    /// Serialized `model.LyricList` (`map_mediafile.go:45`); always present,
    /// `[]` when there are no lyrics (`model/lyrics.go:68-73`).
    pub lyrics: String,
    /// iTunes advisory mapping (`map_mediafile.go:46`, `:192-201`).
    pub explicit_status: String,

    /// Primary date year (`map_mediafile.go:53`).
    pub year: i32,
    /// Primary date (`map_mediafile.go:53`).
    pub date: String,
    /// Original date year (`map_mediafile.go:51`).
    pub original_year: i32,
    /// Original date (`map_mediafile.go:51`).
    pub original_date: String,
    /// Release date year (`map_mediafile.go:52`).
    pub release_year: i32,
    /// Release date (`map_mediafile.go:52`).
    pub release_date: String,

    /// MusicBrainz recording id (`map_mediafile.go:56`).
    pub mbz_recording_id: String,
    /// MusicBrainz release-track id (`map_mediafile.go:57`).
    pub mbz_release_track_id: String,
    /// MusicBrainz album id (`map_mediafile.go:58`).
    pub mbz_album_id: String,
    /// MusicBrainz release-group id (`map_mediafile.go:59`).
    pub mbz_release_group_id: String,
    /// Release type (`map_mediafile.go:60`).
    pub mbz_album_type: String,

    /// ReplayGain album peak (`map_mediafile.go:63`).
    pub rg_album_peak: Option<f64>,
    /// ReplayGain/R128 album gain (`map_mediafile.go:64`).
    pub rg_album_gain: Option<f64>,
    /// ReplayGain track peak (`map_mediafile.go:65`).
    pub rg_track_peak: Option<f64>,
    /// ReplayGain/R128 track gain (`map_mediafile.go:66`).
    pub rg_track_gain: Option<f64>,

    /// Embedded picture present (`map_mediafile.go:69`).
    pub has_cover_art: bool,
    /// Duration in seconds, truncated to `float32` (`map_mediafile.go:70`).
    pub duration: f32,
    /// Bitrate in kbps (`map_mediafile.go:71`).
    pub bit_rate: i32,
    /// Sample rate in Hz (`map_mediafile.go:72`).
    pub sample_rate: i32,
    /// Bit depth; `None` when the backend reports 0
    /// (`map_mediafile.go:73-75`, `map_mediafile_test.go:186-190`).
    pub bit_depth: Option<i32>,
    /// Channel count (`map_mediafile.go:76`).
    pub channels: i32,
    /// Codec name (`map_mediafile.go:77`).
    pub codec: String,
    /// Path relative to the library root (`map_mediafile.go:78`).
    pub path: String,
    /// Lowercased extension (`map_mediafile.go:79`).
    pub suffix: String,
    /// File size; absent until storage supplies it (see module docs).
    pub size: Option<i64>,
    /// Birth time, RFC 3339; absent until storage supplies it.
    pub birth_time: Option<String>,
    /// Modification time, RFC 3339; absent until storage supplies it.
    pub updated_at: Option<String>,

    /// All credits, keyed by role (`map_mediafile.go:84`).
    pub participants: RoleCredits,
    /// Display artist (`map_mediafile.go:85`).
    pub artist: String,
    /// Display album artist (`map_mediafile.go:86`).
    pub album_artist: String,

    /// Deprecated single-artist fields, kept for the legacy API
    /// (`map_mediafile.go:95-96`).
    pub artist_id: String,
    /// Deprecated single-album-artist field (`map_mediafile.go:96`).
    pub album_artist_id: String,
    /// Deprecated sort/order artist fields (`map_mediafile.go:99-102`).
    pub order_artist_name: String,
    /// Deprecated order album artist name (`map_mediafile.go:100`).
    pub order_album_artist_name: String,
    /// Deprecated sort artist name (`map_mediafile.go:101`).
    pub sort_artist_name: String,
    /// Deprecated sort album artist name (`map_mediafile.go:102`).
    pub sort_album_artist_name: String,

    /// Cleaned tags, with non-album main tags pruned
    /// (`map_mediafile.go:22`, `:104-112`).
    pub tags: Tags,
}

impl MediaFile {
    /// `model.MediaFile.Movements()` (`model/mediafile.go`): zips the movement
    /// name/number/total tag lists positionally.
    #[must_use]
    pub fn movements(&self) -> Vec<Movement> {
        let names = self.tags.get(crate::tag_names::TAG_MOVEMENT_NAME.as_str());
        let numbers = self
            .tags
            .get(crate::tag_names::TAG_MOVEMENT_NUMBER.as_str());
        let totals = self.tags.get(crate::tag_names::TAG_MOVEMENT_TOTAL.as_str());
        let len = names.map(Vec::len).unwrap_or(0);
        (0..len)
            .map(|i| Movement {
                name: names.and_then(|v| v.get(i)).cloned().unwrap_or_default(),
                // `number.ParseInt[int32]` (`utils/number/number.go`) parses at
                // 64 bits and truncates — unlike `tagNumber`, which rejects
                // out-of-int32 values (`model/metadata/metadata.go:158-165`).
                number: numbers
                    .and_then(|v| v.get(i))
                    .map(|v| crate::metadata::parse_int32_lossy(v))
                    .unwrap_or(0),
                count: totals
                    .and_then(|v| v.get(i))
                    .map(|v| crate::metadata::parse_int32_lossy(v))
                    .unwrap_or(0),
            })
            .collect()
    }

    /// The first credit of a role, mirroring Go's `Participants.First`
    /// returning a zero-value artist when absent.
    #[must_use]
    pub fn first_credit(&self, role: nd_id::Role) -> Option<&Credit> {
        self.participants.first(role)
    }
}
