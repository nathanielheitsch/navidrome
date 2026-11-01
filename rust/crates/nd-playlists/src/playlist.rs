//! Playlist DTO — port of the `model.Playlist`/`model.PlaylistTrack` wire
//! shape (`model/playlist.go:21-44`, `:226-231`). The full repository
//! surface is #23; this module pins the JSON DTO the parsers produce and
//! the REST adapter serializes.

use serde::{Deserialize, Serialize};

/// A playlist record (`model.Playlist`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    /// Stable id.
    #[serde(default)]
    pub id: String,
    /// Playlist name.
    #[serde(default)]
    pub name: String,
    /// Free-form comment.
    #[serde(default)]
    pub comment: String,
    /// Total duration in seconds.
    #[serde(default)]
    pub duration: f32,
    /// Total size in bytes.
    #[serde(default)]
    pub size: i64,
    /// Number of tracks.
    #[serde(default, rename = "songCount")]
    pub song_count: i32,
    /// Owner display name (not stored).
    #[serde(default, rename = "ownerName")]
    pub owner_name: String,
    /// Owner id.
    #[serde(default, rename = "ownerId")]
    pub owner_id: String,
    /// Public visibility.
    #[serde(default)]
    pub public: bool,
    /// Serialized tracks (API responses only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<PlaylistTrack>,
    /// Absolute path of the source file, for synced playlists.
    #[serde(default)]
    pub path: String,
    /// Sync with the source file.
    #[serde(default)]
    pub sync: bool,
    /// Uploaded (manual) cover path.
    #[serde(default, rename = "uploadedImage")]
    pub uploaded_image: String,
    /// `#EXTALBUMARTURL` / album-art URL.
    #[serde(
        default,
        rename = "externalImageUrl",
        skip_serializing_if = "String::is_empty"
    )]
    pub external_image_url: String,
    /// Creation time.
    #[serde(default, rename = "createdAt")]
    pub created_at: String,
    /// Last update time.
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
    /// Smart-playlist rules (raw criteria JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<serde_json::Value>,
    /// Last rules evaluation time (RFC 3339).
    #[serde(
        default,
        rename = "evaluatedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub evaluated_at: Option<String>,
}

/// A playlist track join record (`model.PlaylistTrack`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaylistTrack {
    /// Join-record id.
    #[serde(default)]
    pub id: String,
    /// Referenced media file id.
    #[serde(default, rename = "mediaFileId")]
    pub media_file_id: String,
    /// Owning playlist id.
    #[serde(default, rename = "playlistId")]
    pub playlist_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Playlist {
        Playlist {
            id: "pl1".into(),
            name: "My Playlist".into(),
            comment: "A comment".into(),
            duration: 123.5,
            size: 4096,
            song_count: 2,
            owner_name: "admin".into(),
            owner_id: "u1".into(),
            public: true,
            tracks: vec![
                PlaylistTrack {
                    id: "t1".into(),
                    media_file_id: "mf1".into(),
                    playlist_id: "pl1".into(),
                },
                PlaylistTrack {
                    id: "t2".into(),
                    media_file_id: "mf2".into(),
                    playlist_id: "pl1".into(),
                },
            ],
            path: "/music/playlists/pls.m3u".into(),
            sync: true,
            uploaded_image: "uploaded.jpg".into(),
            external_image_url: "https://example.com/cover.jpg".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            rules: Some(serde_json::json!({"all": [{"is": {"loved": true}}]})),
            evaluated_at: Some("2026-01-02T00:00:00Z".into()),
        }
    }

    #[test]
    fn dto_json_round_trip() {
        // Same field-set contract as the Go structs tags: snake_case for
        // db-backed fields, camelCase for the API fields Go tags spell out
        // (songCount, ownerName, ownerId, uploadedImage, externalImageUrl,
        // evaluatedAt, mediaFileId, playlistId).
        let json = r#"{
            "id": "pl1",
            "name": "My Playlist",
            "comment": "A comment",
            "duration": 123.5,
            "size": 4096,
            "songCount": 2,
            "ownerName": "admin",
            "ownerId": "u1",
            "public": true,
            "tracks": [
                {"id": "t1", "mediaFileId": "mf1", "playlistId": "pl1"},
                {"id": "t2", "mediaFileId": "mf2", "playlistId": "pl1"}
            ],
            "path": "/music/playlists/pls.m3u",
            "sync": true,
            "uploadedImage": "uploaded.jpg",
            "externalImageUrl": "https://example.com/cover.jpg",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "rules": {"all": [{"is": {"loved": true}}]},
            "evaluatedAt": "2026-01-02T00:00:00Z"
        }"#;
        let pls: Playlist = serde_json::from_str(json).unwrap();
        assert_eq!(pls, sample());

        let re = serde_json::to_value(&pls).unwrap();
        assert_eq!(re["songCount"], 2);
        assert_eq!(re["ownerName"], "admin");
        assert_eq!(re["ownerId"], "u1");
        assert_eq!(re["uploadedImage"], "uploaded.jpg");
        assert_eq!(re["externalImageUrl"], "https://example.com/cover.jpg");
        assert_eq!(re["evaluatedAt"], "2026-01-02T00:00:00Z");
        assert_eq!(re["tracks"][0]["mediaFileId"], "mf1");
        assert_eq!(re["tracks"][0]["playlistId"], "pl1");
        assert!(re.get("imported_hash").is_none(), "no wire name for hash");
    }

    #[test]
    fn empty_extras_are_omitted_like_go() {
        let v = serde_json::to_value(Playlist::default()).unwrap();
        assert!(v.get("externalImageUrl").is_none());
        assert!(v.get("evaluatedAt").is_none());
        assert!(v.get("rules").is_none());
        assert!(v.get("tracks").is_none());
    }
}
