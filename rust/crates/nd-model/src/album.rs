//! Album DTO — port of `models/album.go:15` (`type Album struct`).
//!
//! Wire names follow the Go `json:` tags; Go `omitempty` becomes serde
//! `omit_if_default`. Go `time.Time` → ISO-8601 `String` (calendar math is the
//! DB layer's). Go `Discs map[int]string` → `BTreeMap<String, String>` keyed by
//! matches. Not modelled (separate #06 modules): `Tags`, `Participants`,
//! `Genres`, `ItemImage`, `FolderIDs`.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

/// Go's `Album` struct (`models/album.go:15`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    /// The entity id (22-char base62).
    #[serde(rename = "id")]
    pub id: String,
    /// The library the entity belongs to.
    #[serde(rename = "libraryId")]
    pub library_id: i32,
    /// Display name.
    #[serde(rename = "name")]
    pub name: String,
    /// Id of the album artist (deprecated: use participants).
    #[serde(rename = "albumArtistId")]
    pub album_artist_id: String,
    /// Display name of the album artist.
    #[serde(rename = "albumArtist")]
    pub album_artist: String,
    /// Newest year across the album's songs.
    #[serde(rename = "maxYear")]
    pub max_year: i32,
    /// Oldest year across the album's songs.
    #[serde(rename = "minYear")]
    pub min_year: i32,
    /// The album's primary date as authored.
    #[serde(rename = "date", skip_serializing_if = "String::is_empty")]
    pub date: String,
    /// Newest original release year.
    #[serde(rename = "maxOriginalYear")]
    pub max_original_year: i32,
    /// Oldest original release year.
    #[serde(rename = "minOriginalYear")]
    pub min_original_year: i32,
    /// The album's original release date.
    #[serde(rename = "originalDate", skip_serializing_if = "String::is_empty")]
    pub original_date: String,
    /// The album's release date.
    #[serde(rename = "releaseDate", skip_serializing_if = "String::is_empty")]
    pub release_date: String,
    /// Whether this is a compilation album.
    #[serde(rename = "compilation")]
    pub compilation: bool,
    /// Free-form comment from the tags.
    #[serde(rename = "comment", skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// Number of songs in the entity.
    #[serde(rename = "songCount")]
    pub song_count: i32,
    /// Total duration in seconds.
    #[serde(rename = "duration")]
    pub duration: f32,
    /// Total size in bytes.
    #[serde(rename = "size")]
    pub size: i64,
    /// Disc number to disc name, keyed by decimal disc number.
    #[serde(rename = "discs", skip_serializing_if = "BTreeMap::is_empty")]
    pub discs: BTreeMap<String, String>,
    /// Sortable album name.
    #[serde(rename = "sortAlbumName", skip_serializing_if = "String::is_empty")]
    pub sort_album_name: String,
    /// Sortable album artist name.
    #[serde(
        rename = "sortAlbumArtistName",
        skip_serializing_if = "String::is_empty"
    )]
    pub sort_album_artist_name: String,
    /// Name order used for sorting.
    #[serde(rename = "orderAlbumName")]
    pub order_album_name: String,
    /// Album-artist order used for sorting.
    #[serde(rename = "orderAlbumArtistName")]
    pub order_album_artist_name: String,
    /// The label catalogue number.
    #[serde(rename = "catalogNum", skip_serializing_if = "String::is_empty")]
    pub catalog_num: String,
    /// MusicBrainz album id.
    #[serde(rename = "mbzAlbumId", skip_serializing_if = "String::is_empty")]
    pub mbz_album_id: String,
    /// MusicBrainz release type.
    #[serde(rename = "mbzAlbumType", skip_serializing_if = "String::is_empty")]
    pub mbz_album_type: String,
    /// MusicBrainz release comment.
    #[serde(rename = "mbzAlbumComment", skip_serializing_if = "String::is_empty")]
    pub mbz_album_comment: String,
    /// MusicBrainz release group id.
    #[serde(rename = "mbzReleaseGroupId", skip_serializing_if = "String::is_empty")]
    pub mbz_release_group_id: String,
    /// Explicit lyrics status (empty, explicit, or clean).
    #[serde(rename = "explicitStatus")]
    pub explicit_status: String,
    /// ReplayGain album gain (dB).
    #[serde(rename = "rgAlbumGain")]
    pub rg_album_gain: Option<f64>,
    /// ReplayGain album peak.
    #[serde(rename = "rgAlbumPeak")]
    pub rg_album_peak: Option<f64>,
    /// External metadata description.
    #[serde(rename = "description", skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The most common genre.
    #[serde(rename = "genre")]
    pub genre: String,
    /// Whether every media file of the entity is missing.
    #[serde(rename = "missing")]
    pub missing: bool,
    /// Creation timestamp (ISO-8601 text; parsed by #05).
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// Update timestamp (ISO-8601 text; parsed by #05).
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// Default-constructed `Album` (Go's zero struct).
pub fn new_album(id: String) -> Album {
    Album {
        id,
        library_id: 0,
        name: String::from(""),
        album_artist_id: String::from(""),
        album_artist: String::from(""),
        max_year: 0,
        min_year: 0,
        date: String::from(""),
        max_original_year: 0,
        min_original_year: 0,
        original_date: String::from(""),
        release_date: String::from(""),
        compilation: false,
        comment: String::from(""),
        song_count: 0,
        duration: 0.0,
        size: 0,
        discs: BTreeMap::new(),
        sort_album_name: String::from(""),
        sort_album_artist_name: String::from(""),
        order_album_name: String::from(""),
        order_album_artist_name: String::from(""),
        catalog_num: String::from(""),
        mbz_album_id: String::from(""),
        mbz_album_type: String::from(""),
        mbz_album_comment: String::from(""),
        mbz_release_group_id: String::from(""),
        explicit_status: String::from(""),
        rg_album_gain: None,
        rg_album_peak: None,
        description: String::from(""),
        genre: String::from(""),
        missing: false,
        created_at: String::from(""),
        updated_at: String::from(""),
    }
}

/// Go `Album.FullName()` (`models/album.go:79`).
pub fn full_name(album: &Album) -> String {
    album.name.clone()
}

/// Go `Album.Equals(other)` (`models/album.go:87`).
pub fn equals(album: &Album, other: &Album) -> bool {
    album.id == other.id
}

#[cfg(test)]
mod album_test {
    use super::*;
    use serde_json::{from_str, to_string};

    #[test]
    fn album_uses_go_wire_names_and_omitempty() {
        // Arrange
        let mut album = new_album("al-1".to_string());
        album.name = "Low Living".to_string();
        album.date = "1974".to_string();
        album.discs.insert("2".to_string(), "Side B".to_string());
        album.rg_album_gain = Some(-1.5);
        // Act
        let wire = to_string(&album).unwrap_or("".to_string());
        let back = from_str(&wire).unwrap_or(new_album("".to_string()));
        // Assert
        assert!(
            wire.contains("\"albumArtistId\""),
            "Go wire key casing preserved"
        );
        assert!(
            !wire.contains("\"comment\""),
            "Go omitempty drops empty strings"
        );
        assert!(
            wire.contains("\"discs\":{\"2\":\"Side B\"}"),
            "Go map[int]string wire shape"
        );
        assert_eq!(back, album, "DTO JSON round-trips");
    }
}
