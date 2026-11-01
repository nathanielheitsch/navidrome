// nd-playlists: owned by #10 / #23 (docs/rust-port/issues.md Part B)

//! #10 — Playlist parsers (this stage): M3U line classification and path
//! resolution, NSP SmartPlaylist JSON parsing, and import semantics
//! (extension dispatch, synced-playlist creation, content fingerprints,
//! the `PlaylistsImportPending` flag key). The `model.Playlist` wire DTO the
//! parsers and the `rest_adapter.go` shape produce lives in [`playlist`].
//! Store wiring and the REST adapter land in #23 (`docs/rust-port/issues.md`
//! Part B `#23`).

pub mod parsers;
pub mod playlist;

pub use parsers::import::{
    extension_of, fingerprint, join, kind_for, new_synced_playlist, parse_playlist_file, M3uParsed,
    ParsedPlaylist, PlaylistKind, SyncedPlaylist, SyncedPlaylistError,
    PLAYLISTS_IMPORT_PENDING_KEY,
};
pub use parsers::m3u::{
    classify_line, clean_path, is_audio_file, is_image_file, lookup_candidates, lookup_key, nfc,
    nfd, parse_m3u_text, resolve_image_url, resolve_local_path, Folder, Library, M3uEntry,
    PathError, PathResolution, PathResolver,
};
pub use parsers::nsp::{position_from_offset, NspError, NspFile, NspRules, NSP_MAX_BYTES};
pub use playlist::{Playlist, PlaylistTrack};

/// Scaffold marker kept from #S1: the owning issue for this crate.
pub const OWNED_BY_ISSUE: u8 = 10;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_playlists_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 10;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-playlists instantiates without pulling in the world"
        );
    }
}
