//! Import semantics — port of `core/playlists/import.go` (the parser half;
//! the repo/store wiring lands in #23). Extension dispatch (`.nsp` vs M3U),
//! synced-playlist creation, content fingerprints, and the
//! [`PlaylistsImportPending`](self::PLAYLISTS_IMPORT_PENDING_KEY) flag key.

use std::path::Path;
use std::time::SystemTime;

pub use super::nsp::NspFile;

/// Flag key marking that playlist import was deferred
/// (`consts.PlaylistsImportPendingFlagKey`, `consts/consts.go:19`).
pub const PLAYLISTS_IMPORT_PENDING_KEY: &str = "PlaylistsImportPending";

/// Playlist content fingerprint: 128-bit xxh3 over the bytes the parser
/// consumed, rendered through the id encoder (`fingerprint`,
/// `import.go:127-129`).
#[must_use]
pub fn fingerprint(data: &[u8]) -> String {
    let digest = xxhash_rust::xxh3::xxh3_128(data).to_le_bytes();
    nd_id::encode(&digest)
}

/// A synced playlist created from a file on disk (`newSyncedPlaylist`,
/// `parse_nsp.go:15-33`).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncedPlaylist {
    /// Name derived from the filename (extension stripped).
    pub name: String,
    /// `Auto-imported from '<filename>'`.
    pub comment: String,
    /// Synced playlists are private by default.
    pub public: bool,
    /// Absolute path of the playlist file.
    pub path: String,
    /// Always `true` for disk-imported playlists.
    pub sync: bool,
    /// File modification time.
    pub updated_at: SystemTime,
}

/// Errors building a [`SyncedPlaylist`].
#[derive(Debug, thiserror::Error)]
pub enum SyncedPlaylistError {
    /// `os.Stat` failed (typically: file does not exist).
    #[error("reading playlist file: {0}")]
    Stat(#[from] std::io::Error),
}

/// Builds a synced playlist record for `base_dir/playlist_file`
/// (`newSyncedPlaylist`, `parse_nsp.go:15-33`): name is the filename minus
/// extension, comment records the source file, public is false, sync true.
///
/// # Errors
/// [`SyncedPlaylistError::Stat`] when the file cannot be probed.
pub fn new_synced_playlist(
    base_dir: &str,
    playlist_file: &str,
) -> Result<SyncedPlaylist, SyncedPlaylistError> {
    let playlist_path = join(base_dir, playlist_file);
    let meta = std::fs::metadata(&playlist_path)?;
    let extension = extension_of(playlist_file);
    let name = playlist_file
        .strip_suffix(&extension)
        .unwrap_or(playlist_file)
        .to_string();
    Ok(SyncedPlaylist {
        name,
        comment: format!("Auto-imported from '{playlist_file}'"),
        public: false,
        path: playlist_path,
        sync: true,
        updated_at: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    })
}

/// `filepath.Join` + `filepath.Clean` for POSIX-style paths.
#[must_use]
pub fn join(base: &str, file: &str) -> String {
    let joined = if base.is_empty() {
        file.to_string()
    } else if file.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{file}")
    };
    super::m3u::clean_path(&joined)
}

/// `filepath.Ext`: the suffix from the final dot of the final element,
/// including the dot.
#[must_use]
pub fn extension_of(file: &str) -> String {
    let name = Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => name[i..].to_string(),
    }
}

/// Playlist kind selected by file extension (`parsePlaylist`,
/// `import.go:105-125`): lowercase extension `.nsp` selects the smart
/// playlist; anything else parses as M3U.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistKind {
    /// Smart playlist JSON (`.nsp`).
    Nsp,
    /// Extended M3U (`m3u`/`m3u8` and unknown extensions).
    M3u,
}

/// Selects the parser for a playlist filename (`parsePlaylist` switch,
/// `import.go:116-120`).
#[must_use]
pub fn kind_for(playlist_file: &str) -> PlaylistKind {
    if "nsp".eq_ignore_ascii_case(extension_of(playlist_file).trim_start_matches('.')) {
        PlaylistKind::Nsp
    } else {
        PlaylistKind::M3u
    }
}

/// Parsed result of a playlist file on disk, per its extension.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedPlaylist {
    /// Smart playlist: file metadata plus parsed rules.
    Nsp(SyncedPlaylist, NspFile),
    /// M3U: file metadata plus parsed entries.
    M3u(SyncedPlaylist, M3uParsed),
}

/// The parsed half of an M3U import (DB-backed track materialization is #23).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct M3uParsed {
    /// Last `#PLAYLIST:` value.
    pub name: Option<String>,
    /// Last `#EXTALBUMARTURL:` value (storable resolution is #23's job).
    pub external_image_url: Option<String>,
    /// Raw audio-file lines, in playlist order.
    pub paths: Vec<String>,
    /// Fingerprint of the (BOM-normalized) file bytes.
    pub imported_hash: String,
}

/// Reads and parses a playlist file from `base_dir`, dispatching on the
/// extension and fingerprinting the exact bytes the parser consumed
/// (`parsePlaylist`, `import.go:105-125`; the `TeeReader` hashing becomes a
/// plain post-read hash here since the BOM normalization happens up front).
///
/// # Errors
/// IO errors reading the file, or parse errors ([`SyncedPlaylistError`] /
/// NSP JSON errors) wrapped as strings.
pub fn parse_playlist_file(base_dir: &str, playlist_file: &str) -> Result<ParsedPlaylist, String> {
    let pls = new_synced_playlist(base_dir, playlist_file).map_err(|e| e.to_string())?;
    let raw = std::fs::read(&pls.path).map_err(|e| e.to_string())?;
    let bytes = nd_utils::ioutils::normalize_bom(raw);
    match kind_for(playlist_file) {
        PlaylistKind::Nsp => {
            let nsp = NspFile::parse(&bytes).map_err(|e| e.to_string())?;
            Ok(ParsedPlaylist::Nsp(pls, nsp))
        }
        PlaylistKind::M3u => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let (name, external_image_url, paths) = super::m3u::parse_m3u_text(&text);
            Ok(ParsedPlaylist::M3u(
                pls,
                M3uParsed {
                    name,
                    external_image_url,
                    paths,
                    imported_hash: fingerprint(&bytes),
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_synced_playlist_attributes() {
        let dir = std::env::temp_dir().join("nd-playlists-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.m3u");
        std::fs::write(&file, b"content").unwrap();
        let dir_s = dir.to_str().unwrap().to_string();

        let pls = new_synced_playlist(&dir_s, "test.m3u").unwrap();
        assert_eq!(pls.name, "test");
        assert_eq!(pls.comment, "Auto-imported from 'test.m3u'");
        assert!(!pls.public);
        assert_eq!(pls.path, join(&dir_s, "test.m3u"));
        assert!(pls.sync);
        assert_ne!(pls.updated_at, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn new_synced_playlist_strips_extension_for_name() {
        let dir = std::env::temp_dir().join("nd-playlists-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("My Favorites.nsp");
        std::fs::write(&file, b"{}").unwrap();
        let dir_s = dir.to_str().unwrap().to_string();
        let pls = new_synced_playlist(&dir_s, "My Favorites.nsp").unwrap();
        assert_eq!(pls.name, "My Favorites");
    }

    #[test]
    fn new_synced_playlist_errors_for_missing_file() {
        let dir = std::env::temp_dir().join("nd-playlists-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_str().unwrap().to_string();
        assert!(new_synced_playlist(&dir_s, "nonexistent.m3u").is_err());
    }

    #[test]
    fn extension_of_matches_go() {
        assert_eq!(extension_of("test.m3u"), ".m3u");
        assert_eq!(extension_of("test"), "");
        assert_eq!(extension_of(".hidden"), "");
        assert_eq!(extension_of("a.tar.gz"), ".gz");
        assert_eq!(extension_of("/x/y/my.favorites.nsp"), ".nsp");
    }

    #[test]
    fn kind_dispatch() {
        assert_eq!(kind_for("a.nsp"), PlaylistKind::Nsp);
        assert_eq!(kind_for("a.NSP"), PlaylistKind::Nsp);
        assert_eq!(kind_for("a.m3u"), PlaylistKind::M3u);
        assert_eq!(kind_for("a.m3u8"), PlaylistKind::M3u);
    }

    #[test]
    fn import_pending_key_matches_go_const() {
        assert_eq!(PLAYLISTS_IMPORT_PENDING_KEY, "PlaylistsImportPending");
    }

    #[test]
    fn fingerprint_is_stable() {
        // Deterministic content fingerprint: same bytes -> same id-shaped string.
        let a = fingerprint(b"hello playlist");
        let b = fingerprint(b"hello playlist");
        assert_eq!(a, b);
        assert_ne!(a, fingerprint(b"hello playlist2"));
        assert_eq!(a.len(), nd_id::ID_LENGTH);
    }

    #[test]
    fn parse_playlist_file_m3u_with_bom() {
        let dir = std::env::temp_dir().join("nd-playlists-tests/bom");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "\u{feff}#EXTM3U\n#PLAYLIST:Test Playlist\n../songs/test.mp3\n";
        std::fs::write(dir.join("bom-test.m3u"), content).unwrap();
        let dir_s = dir.to_str().unwrap().to_string();
        let ParsedPlaylist::M3u(pls, parsed) = parse_playlist_file(&dir_s, "bom-test.m3u").unwrap()
        else {
            panic!("expected m3u");
        };
        assert_eq!(parsed.name.as_deref(), Some("Test Playlist"));
        assert_eq!(parsed.paths, vec!["../songs/test.mp3"]);
        assert!(!parsed.imported_hash.is_empty());
        assert_eq!(pls.name, "bom-test");
    }

    #[test]
    fn parse_playlist_file_nsp() {
        let dir = std::env::temp_dir().join("nd-playlists-tests/nsp");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("smart.nsp"),
            b"{\"name\": \"My Smart\", \"all\": [{\"is\": {\"loved\": true}}]}",
        )
        .unwrap();
        let dir_s = dir.to_str().unwrap().to_string();
        let ParsedPlaylist::Nsp(pls, nsp) = parse_playlist_file(&dir_s, "smart.nsp").unwrap()
        else {
            panic!("expected nsp");
        };
        assert_eq!(pls.name, "smart");
        assert_eq!(nsp.name, "My Smart");
        assert_eq!(nsp.public, None);
    }

    #[test]
    fn parse_playlist_file_missing_fails() {
        let dir = std::env::temp_dir().join("nd-playlists-tests/missing");
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_str().unwrap().to_string();
        assert!(parse_playlist_file(&dir_s, "nope.m3u").is_err());
    }
}
