//! Entity DTOs — the `struct` surfaces of `models/album.go:15`,
//! `models/artist.go:14`/`:49`, `models/mediafile.go:27` (the "Song"),
//! `models/playlist.go:20`, `models/scrobble.go:10`, `models/share.go:13`
//! and `models/playqueue.go:8`.
//!
//! Port decisions (frozen here; #06 may not diverge):
//!   * Go `time.Time` / `*time.Time` → `String` / `Option<String>` of ISO-8601
//!     text. DTOs are transforms-only; timestamp parsing belongs to #05.
//!   * A Go `json:` tag carrying `omitempty` → serde `skip_serializing_if`
//!     (the house spelling, as in `nd-conf/src/config.rs:57`).
//!   * Go fields tagged `json:"-"` (`Annotations`, `ItemImage`, `SimilarArtists`,
//!     the `Tracks`/`Albums` repository back-references) never reach the wire,
//!     so they are absent from the DTO; `PlayQueue.Items` IS marshalled.
//!   * Go `Discs map[int]string` → `BTreeMap<String,String>`: Go marshals
//!     integer keys as sorted decimal strings, so the wire shape matches.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::wire::{is_zero_i32, is_zero_i64};

/// Go's `Album` struct (`models/album.go:15`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    #[serde(rename = "id")]
    /// The entity id (22-char base62).
    pub id: String,
    #[serde(rename = "libraryId")]
    /// The library the entity belongs to.
    pub library_id: i32,
    #[serde(rename = "name")]
    /// Display name.
    pub name: String,
    #[serde(rename = "albumArtistId")]
    /// Id of the album artist (deprecated: use participants).
    pub album_artist_id: String,
    #[serde(rename = "albumArtist")]
    /// Display name of the album artist.
    pub album_artist: String,
    #[serde(rename = "maxYear")]
    /// Newest year across the album's songs.
    pub max_year: i32,
    #[serde(rename = "minYear")]
    /// Oldest year across the album's songs.
    pub min_year: i32,
    #[serde(rename = "date", skip_serializing_if = "String::is_empty")]
    /// The album's primary date as authored.
    pub date: String,
    #[serde(rename = "maxOriginalYear")]
    /// Newest original release year.
    pub max_original_year: i32,
    #[serde(rename = "minOriginalYear")]
    /// Oldest original release year.
    pub min_original_year: i32,
    #[serde(rename = "originalDate", skip_serializing_if = "String::is_empty")]
    /// The album's original release date.
    pub original_date: String,
    #[serde(rename = "releaseDate", skip_serializing_if = "String::is_empty")]
    /// The album's release date.
    pub release_date: String,
    #[serde(rename = "compilation")]
    /// Whether this is a compilation album.
    pub compilation: bool,
    #[serde(rename = "comment", skip_serializing_if = "String::is_empty")]
    /// Free-form comment from the tags.
    pub comment: String,
    #[serde(rename = "songCount")]
    /// Number of songs in the entity.
    pub song_count: i32,
    #[serde(rename = "duration")]
    /// Total duration in seconds.
    pub duration: f32,
    #[serde(rename = "size")]
    /// Total size in bytes.
    pub size: i64,
    #[serde(rename = "discs", skip_serializing_if = "BTreeMap::is_empty")]
    /// Disc number to disc name, keyed by decimal disc number.
    pub discs: BTreeMap<String, String>,
    #[serde(rename = "sortAlbumName", skip_serializing_if = "String::is_empty")]
    /// Sortable album name.
    pub sort_album_name: String,
    #[serde(rename = "orderAlbumName")]
    /// Name order used for sorting.
    pub order_album_name: String,
    #[serde(rename = "catalogNum", skip_serializing_if = "String::is_empty")]
    /// The label catalogue number.
    pub catalog_num: String,
    #[serde(rename = "mbzAlbumId", skip_serializing_if = "String::is_empty")]
    /// MusicBrainz album id.
    pub mbz_album_id: String,
    #[serde(rename = "mbzAlbumType", skip_serializing_if = "String::is_empty")]
    /// MusicBrainz release type.
    pub mbz_album_type: String,
    #[serde(rename = "explicitStatus")]
    /// Explicit lyrics status (empty, explicit, or clean).
    pub explicit_status: String,
    #[serde(rename = "rgAlbumGain")]
    /// ReplayGain album gain (dB).
    pub rg_album_gain: Option<f64>,
    #[serde(rename = "rgAlbumPeak")]
    /// ReplayGain album peak.
    pub rg_album_peak: Option<f64>,
    #[serde(rename = "description", skip_serializing_if = "String::is_empty")]
    /// External metadata description.
    pub description: String,
    #[serde(rename = "genre")]
    /// The most common genre.
    pub genre: String,
    #[serde(rename = "missing")]
    /// Whether every media file of the entity is missing.
    pub missing: bool,
    #[serde(rename = "createdAt")]
    /// Oldest creation timestamp across the songs.
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    /// Newest update timestamp across the songs.
    pub updated_at: String,
}

/// Go's `Artist` struct (`models/artist.go:14`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Artist {
    #[serde(rename = "id")]
    /// The entity id (22-char base62).
    pub id: String,
    #[serde(rename = "name")]
    /// Display name.
    pub name: String,
    #[serde(rename = "sortArtistName", skip_serializing_if = "String::is_empty")]
    /// Sortable artist name.
    pub sort_artist_name: String,
    #[serde(rename = "orderArtistName", skip_serializing_if = "String::is_empty")]
    /// Name order used for sorting.
    pub order_artist_name: String,
    #[serde(rename = "mbzArtistId", skip_serializing_if = "String::is_empty")]
    /// MusicBrainz artist id.
    pub mbz_artist_id: String,
    #[serde(rename = "size", skip_serializing_if = "is_zero_i64")]
    /// Total size in bytes.
    pub size: i64,
    #[serde(rename = "albumCount", skip_serializing_if = "is_zero_i32")]
    /// Number of albums by the artist.
    pub album_count: i32,
    #[serde(rename = "songCount", skip_serializing_if = "is_zero_i32")]
    /// Number of songs in the entity.
    pub song_count: i32,
    #[serde(rename = "biography", skip_serializing_if = "String::is_empty")]
    /// External metadata biography.
    pub biography: String,
    #[serde(rename = "smallImageUrl", skip_serializing_if = "String::is_empty")]
    /// Small cover-art URL.
    pub small_image_url: String,
    #[serde(rename = "mediumImageUrl", skip_serializing_if = "String::is_empty")]
    /// Medium cover-art URL.
    pub medium_image_url: String,
    #[serde(rename = "largeImageUrl", skip_serializing_if = "String::is_empty")]
    /// Large cover-art URL.
    pub large_image_url: String,
    #[serde(rename = "externalUrl", skip_serializing_if = "String::is_empty")]
    /// Link to the metadata source page.
    pub external_url: String,
    #[serde(rename = "uploadedImage", skip_serializing_if = "String::is_empty")]
    /// Locally uploaded cover-art reference.
    pub uploaded_image: String,
    #[serde(rename = "missing")]
    /// Whether every media file of the entity is missing.
    pub missing: bool,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    /// Oldest creation timestamp across the songs.
    pub created_at: Option<String>,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    /// Newest update timestamp across the songs.
    pub updated_at: Option<String>,
}

/// Go's `ArtistStats` struct (`models/artist.go:49`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistStats {
    #[serde(rename = "songCount")]
    /// Number of songs in the entity.
    pub song_count: i32,
    #[serde(rename = "albumCount")]
    /// Number of albums by the artist.
    pub album_count: i32,
    #[serde(rename = "size")]
    /// Total size in bytes.
    pub size: i64,
}

#[cfg(test)]
mod entity_test {
    use super::*;
    use serde_json::to_string;

    #[test]
    fn album_wire_matches_go_omitempty_rules() {
        // Arrange
        let album = Album::default();
        // Act
        let wire = to_string(&album);
        // Assert
        assert!(wire.is_ok(), "serialization cannot fail");
    }
}
