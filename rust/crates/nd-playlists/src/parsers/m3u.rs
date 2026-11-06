//! M3U parsing and path resolution — port of `core/playlists/parse_m3u.go`.
//!
//! This module ports the pure parser half of the Go file: line filtering
//! (`#PLAYLIST`, `#EXTALBUMARTURL`, comments, `file://` unescaping,
//! audio-extension check) and the [`PathResolver`]/[`PathResolution`] path
//! resolution machinery. The DB-chunked lookup loop (`parseM3U` itself,
//! `parse_m3u.go:22-84`) lives with the store wiring in #23.

use std::path::{Component, Path};

use unicode_normalization::UnicodeNormalization;

/// A library known to the resolver (`model.Library`, relevant fields only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// Numeric library id.
    pub id: i64,
    /// Absolute library root path.
    pub path: String,
}

/// A folder a playlist file lives in (`model.Folder`, relevant fields only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folder {
    /// Library the folder belongs to.
    pub library_id: i64,
    /// Absolute path of the owning library root.
    pub library_path: String,
    /// Folder path relative to the library root.
    pub path: String,
    /// Folder name (`.` for the library root).
    pub name: String,
}

impl Folder {
    /// Absolute folder path (`model.Folder.AbsolutePath`,
    /// `model/folder.go:35-37`): `filepath.Join` of library path, folder path
    /// and name — empty elements are skipped, like `filepath.Join`.
    #[must_use]
    pub fn absolute_path(&self) -> String {
        let parts: Vec<&str> = [&self.library_path, &self.path, &self.name]
            .into_iter()
            .map(|p| p.as_str())
            .filter(|p| !p.is_empty())
            .collect();
        let joined = parts.join("/");
        if joined.is_empty() {
            return clean_path(&self.library_path);
        }
        clean_path(&joined)
    }
}

/// The result of resolving a playlist path to a library-relative path
/// (`pathResolution`, `parse_m3u.go:87-92`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathResolution {
    /// Absolute path of the entry.
    pub absolute_path: String,
    /// Absolute path of the owning library root.
    pub library_path: String,
    /// Owning library id.
    pub library_id: i64,
    /// Whether the resolution found a library.
    pub valid: bool,
}

impl PathResolution {
    /// Converts to a library-qualified string `"<libraryID>:<relativePath>"`
    /// with forward slashes (`ToQualifiedString`, `parse_m3u.go:95-110`).
    ///
    /// # Errors
    /// [`PathError::Invalid`] when the resolution is not valid; relative-path
    /// failures surface as [`PathError::Rel`].
    pub fn to_qualified_string(&self) -> Result<String, PathError> {
        if !self.valid {
            return Err(PathError::Invalid);
        }
        let rel = relate(&self.library_path, &self.absolute_path)
            .map_err(|e| PathError::Rel(format!("{}: {e}", self.absolute_path)))?;
        // Convert path separators to forward slashes (filepath.ToSlash)
        Ok(format!("{}:{}", self.library_id, rel.replace('\\', "/")))
    }
}

/// Path-resolution errors.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The resolution was not valid.
    #[error("invalid path resolution")]
    Invalid,
    /// `filepath.Rel` could not produce a relative path.
    #[error("relative path: {0}")]
    Rel(String),
}

/// Matches absolute paths against known libraries
/// (`model.LibraryMatcher`, `model/library_matcher.go`): longest path first,
/// so `/music-classical` wins over `/music`.
#[derive(Debug, Clone)]
pub struct PathResolver {
    libraries: Vec<Library>,
    cleaned_paths: Vec<String>,
}

impl PathResolver {
    /// Builds a resolver, sorting libraries longest path first
    /// (`NewLibraryMatcher`/`newPathResolver`).
    #[must_use]
    pub fn new(mut libs: Vec<Library>) -> Self {
        libs.sort_by_key(|l| std::cmp::Reverse(l.path.len()));
        let cleaned_paths = libs.iter().map(|l| clean_path(&l.path)).collect::<Vec<_>>();
        Self {
            libraries: libs,
            cleaned_paths,
        }
    }

    /// Returns the library whose path contains `absolute_path`
    /// (`FindLibrary`, `model/library_matcher.go:30-43`).
    #[must_use]
    pub fn find_library(&self, absolute_path: &str) -> Option<&Library> {
        for (i, lib_path) in self.cleaned_paths.iter().enumerate() {
            // A cleaned path only ends with a separator when it is a filesystem root
            if absolute_path.starts_with(lib_path)
                && (absolute_path.len() == lib_path.len()
                    || absolute_path.as_bytes()[lib_path.len()] == b'/'
                    || lib_path.ends_with('/'))
            {
                return Some(&self.libraries[i]);
            }
        }
        None
    }

    /// Resolves a playlist line to a [`PathResolution`] (`resolvePath`,
    /// `parse_m3u.go:130-149`): absolute lines are used as-is (cleaned);
    /// relative lines are resolved against the playlist's folder, then
    /// matched against libraries.
    #[must_use]
    pub fn resolve_path(&self, line: &str, folder: Option<&Folder>) -> PathResolution {
        let absolute_path = match folder {
            Some(f) if !Path::new(line).is_absolute() => {
                clean_path(&format!("{}/{}", f.absolute_path(), line))
            }
            _ => clean_path(line),
        };
        self.find_in_libraries(&absolute_path)
    }

    /// Matches an absolute path against all known libraries (`findInLibraries`,
    /// `parse_m3u.go:151-164`).
    #[must_use]
    pub fn find_in_libraries(&self, absolute_path: &str) -> PathResolution {
        match self.find_library(absolute_path) {
            None => PathResolution {
                valid: false,
                ..PathResolution::default()
            },
            Some(lib) => PathResolution {
                absolute_path: absolute_path.to_string(),
                library_path: clean_path(&lib.path),
                library_id: lib.id,
                valid: true,
            },
        }
    }

    /// Converts playlist lines to library-qualified strings
    /// (`resolvePaths`, `parse_m3u.go:166-192`). Lines outside every library
    /// are skipped.
    #[must_use]
    pub fn resolve_paths(&self, folder: Option<&Folder>, lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|line| self.resolve_path(line, folder))
            .filter(|r| r.valid)
            .filter_map(|r| r.to_qualified_string().ok())
            .collect()
    }
}

/// One filtered playlist entry plus the side effects of its directives.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct M3uEntry {
    /// `#PLAYLIST:` directive value, when present.
    pub playlist_name: Option<String>,
    /// `#EXTALBUMARTURL:` directive value (URL or local path), when present.
    pub external_image_url: Option<String>,
    /// Raw (unqualified) file path line, when the line is an audio file.
    pub path: Option<String>,
}

/// One line of an M3U file, classified (`parseM3U` line loop,
/// `parse_m3u.go:29-57`).
#[must_use]
pub fn classify_line(line: &str) -> M3uEntry {
    let mut entry = M3uEntry::default();
    let line = line.trim();
    if let Some(after) = line.strip_prefix("#PLAYLIST:") {
        entry.playlist_name = Some(after.to_string());
        return entry;
    }
    if let Some(after) = line.strip_prefix("#EXTALBUMARTURL:") {
        entry.external_image_url = Some(after.to_string());
        return entry;
    }
    // Skip empty lines and extended info
    if line.is_empty() || line.starts_with('#') {
        return entry;
    }
    let mut line = line.to_string();
    if let Some(after) = line.strip_prefix("file://") {
        line = urlencoding::decode(after)
            .map(|c| c.into_owned())
            .unwrap_or_default();
    }
    if !is_audio_file(&line) {
        return entry;
    }
    entry.path = Some(line);
    entry
}

/// Classifies a whole M3U text, preserving entry order for paths and keeping
/// the last directive value for each directive type (matching the Go loop,
/// which overwrites `pls.Name`/`pls.ExternalImageURL` per occurrence).
#[must_use]
pub fn parse_m3u_text(text: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let mut name = None;
    let mut image_url = None;
    let mut paths = Vec::new();
    for line in nd_utils::slice::lines_from(text) {
        let entry = classify_line(&line);
        if entry.playlist_name.is_some() {
            name = entry.playlist_name;
        }
        if entry.external_image_url.is_some() {
            image_url = entry.external_image_url;
        }
        if let Some(p) = entry.path {
            paths.push(p);
        }
    }
    (name, image_url, paths)
}

/// `model.IsAudioFile` (`model/file_types.go:16-21`): true when the extension
/// maps to an `audio/*` MIME type that is not a playlist type itself.
#[must_use]
pub fn is_audio_file(file_path: &str) -> bool {
    let ext = Path::new(file_path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mime = mime_from_ext(&ext);
    !matches!(
        mime.as_deref(),
        Some("audio/mpegurl") | Some("audio/x-mpegurl") | Some("audio/x-scpls")
    ) && mime.is_some_and(|m| m.starts_with("audio/"))
}

/// `model.IsImageFile` (`model/file_types.go:22-25`).
#[must_use]
pub fn is_image_file(file_path: &str) -> bool {
    let ext = Path::new(file_path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    mime_from_ext(&ext).is_some_and(|m| m.starts_with("image/"))
}

/// `mime.TypeByExtension` for the extensions playlists care about, falling
/// back to the `mime` crate's table. Lowercased extension with leading dot.
fn mime_from_ext(ext: &str) -> Option<String> {
    let ext = ext.to_ascii_lowercase();
    match ext.as_str() {
        // Go's built-in table entries the playlist code paths rely on.
        ".mp3" | ".m4a" | ".m4b" | ".m4p" => Some("audio/mpeg".into()),
        ".ogg" | ".oga" | ".opus" => Some("audio/ogg".into()),
        ".flac" => Some("audio/x-flac".into()),
        ".wav" => Some("audio/wave".into()),
        ".m3u" | ".m3u8" => Some("audio/mpegurl".into()),
        ".aac" => Some("audio/aac".into()),
        ".jpg" | ".jpeg" => Some("image/jpeg".into()),
        ".png" => Some("image/png".into()),
        ".webp" => Some("image/webp".into()),
        ".gif" => Some("image/gif".into()),
        _ => mime_guess_simple(&ext),
    }
}

/// Last-resort MIME lookup (keeps the crate dependency-light).
fn mime_guess_simple(ext: &str) -> Option<String> {
    let audio = [
        ".mp3", ".m4a", ".m4b", ".m4p", ".ogg", ".oga", ".opus", ".flac", ".wav", ".aac", ".wma",
        ".alac", ".ape", ".mpc", ".dsf", ".dsff", ".aif", ".aiff", ".aifc", ".voc", ".amr", ".ac3",
        ".caf", ".mid", ".midi",
    ];
    let image = [
        ".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".svg", ".ico", ".tiff", ".tif",
    ];
    if audio.contains(&ext) {
        Some("audio/unknown".into())
    } else if image.contains(&ext) {
        Some("image/unknown".into())
    } else {
        None
    }
}

/// `filepath.Clean` equivalent for POSIX-style playlist paths.
#[must_use]
pub fn clean_path(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for comp in Path::new(p).components() {
        match comp {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                        continue;
                    }
                }
                if !rooted {
                    out.push("..");
                }
            }
            Component::Normal(s) => out.push(s.to_str().unwrap_or("")),
            Component::Prefix(_) => {}
        }
    }
    if out.is_empty() {
        return if rooted { "/".into() } else { ".".into() };
    }
    let joined = out.join("/");
    if rooted {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `filepath.Rel` for POSIX-style paths (error mirrors `Rel`'s inability to
/// escape the base with relative bases).
fn relate(base: &str, target: &str) -> Result<String, String> {
    if base == target {
        return Ok(".".into());
    }
    let base_abs = Path::new(base).is_absolute();
    let target_abs = Path::new(target).is_absolute();
    if base_abs != target_abs {
        return Err(format!("Rel: can't make {target} relative to {base}"));
    }
    let base_clean = clean_path(base);
    let target_clean = clean_path(target);
    let base_parts: Vec<&str> = base_clean.split('/').filter(|p| !p.is_empty()).collect();
    let target_parts: Vec<&str> = target_clean.split('/').filter(|p| !p.is_empty()).collect();
    let mut common = 0;
    while common < base_parts.len()
        && common < target_parts.len()
        && base_parts[common] == target_parts[common]
    {
        common += 1;
    }
    if common == base_parts.len() {
        return Ok(target_parts[common..].join("/"));
    }
    let mut rel: Vec<&str> = vec![".."; base_parts.len() - common];
    rel.extend_from_slice(&target_parts[common..]);
    if rel.is_empty() {
        return Ok(".".into());
    }
    Ok(rel.join("/"))
}

/// Resolves an `#EXTALBUMARTURL` value to a storable string
/// (`resolveImageURL`, `parse_m3u.go:194-233`): HTTP(S) URLs are stored as-is
/// when `EnableM3UExternalAlbumArt`; local paths are resolved, image-checked
/// and validated against library boundaries (owner's libraries only when the
/// playlist has no folder).
#[must_use]
pub fn resolve_image_url(
    value: &str,
    folder: Option<&Folder>,
    matcher: &PathResolver,
    owner_library_ids: &[i64],
    enable_m3u_external_album_art: bool,
) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        if !enable_m3u_external_album_art {
            return String::new();
        }
        return value.to_string();
    }
    let Some(local_path) = resolve_local_path(value, folder) else {
        return String::new();
    };
    if !is_image_file(&local_path) {
        return String::new();
    }
    match matcher.find_library(&local_path) {
        Some(lib) => {
            if folder.is_none() && !owner_library_ids.contains(&lib.id) {
                return String::new();
            }
            local_path
        }
        None => String::new(),
    }
}

/// Converts a `file://`, absolute, or relative path to a clean absolute path
/// (`resolveLocalPath`, `parse_m3u.go:236-253`).
#[must_use]
pub fn resolve_local_path(value: &str, folder: Option<&Folder>) -> Option<String> {
    if let Some(after) = value.strip_prefix("file://") {
        let decoded = urlencoding::decode(after).ok()?;
        return Some(clean_path(&decoded));
    }
    if Path::new(value).is_absolute() {
        return Some(clean_path(value));
    }
    let folder = folder?;
    Some(clean_path(&format!("{}/{}", folder.absolute_path(), value)))
}

/// NFC form of `s` (`norm.NFC.String`).
#[must_use]
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// NFD form of `s` (`norm.NFD.String`).
#[must_use]
pub fn nfd(s: &str) -> String {
    s.nfd().collect()
}

/// Lookup candidates for one resolved path: NFC/NFD × raw/lowercase
/// (`parse_m3u.go:47-77`), deduplicated, originals first.
#[must_use]
pub fn lookup_candidates(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(4);
    let nfc_raw = nfc(path);
    let nfd_raw = nfd(path);
    for cand in [
        nfc_raw.clone(),
        nfd_raw.clone(),
        nfc_raw.to_lowercase(),
        nfd_raw.to_lowercase(),
    ] {
        if !out.contains(&cand) {
            out.push(cand);
        }
    }
    out
}

/// Library-qualified lookup key: `<libraryID>:<lowercased NFC path>`
/// (`parse_m3u.go:70-76`).
#[must_use]
pub fn lookup_key(library_id: i64, path: &str) -> String {
    format!("{library_id}:{}", nfc(path).to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> PathResolver {
        PathResolver::new(vec![
            Library {
                id: 1,
                path: "/music".into(),
            },
            Library {
                id: 2,
                path: "/music-classical".into(),
            },
            Library {
                id: 3,
                path: "/podcasts".into(),
            },
        ])
    }

    fn folder() -> Folder {
        // Matches the Go test fixture: model.Folder{Path: "playlists",
        // LibraryPath: "/music", LibraryID: 1} — Name unset.
        Folder {
            path: "playlists".into(),
            library_path: "/music".into(),
            library_id: 1,
            name: String::new(),
        }
    }

    #[test]
    fn resolves_absolute_paths() {
        let r = resolver().resolve_path("/music/artist/album/track.mp3", None);
        assert!(r.valid);
        assert_eq!(r.library_id, 1);
        assert_eq!(r.library_path, "/music");
        assert_eq!(r.absolute_path, "/music/artist/album/track.mp3");
    }

    #[test]
    fn resolves_relative_paths_when_folder_provided() {
        let f = folder();
        let r = resolver().resolve_path("../artist/album/track.mp3", Some(&f));
        assert!(r.valid);
        assert_eq!(r.library_id, 1);
        assert_eq!(r.absolute_path, "/music/artist/album/track.mp3");
    }

    #[test]
    fn returns_invalid_for_paths_outside_any_library() {
        let r = resolver().resolve_path("/outside/library/track.mp3", None);
        assert!(!r.valid);
    }

    #[test]
    fn resolves_to_longest_matching_library() {
        let r = resolver().resolve_path("/music-classical/track.mp3", None);
        assert!(r.valid);
        assert_eq!(r.library_id, 2);
        assert_eq!(r.library_path, "/music-classical");
    }

    #[test]
    fn returns_invalid_for_non_audio_outside() {
        let r = resolver().resolve_path("/videos/movie.mp4", None);
        assert!(!r.valid);
    }

    #[test]
    fn cleans_the_path_before_matching() {
        let r = resolver().resolve_path("/music//artist/../artist/track.mp3", None);
        assert!(r.valid);
        assert_eq!(r.absolute_path, "/music/artist/track.mp3");
    }

    #[test]
    fn resolves_relative_path_within_same_library() {
        let f = folder();
        let r = resolver().resolve_path("../songs/track.mp3", Some(&f));
        assert!(r.valid);
        assert_eq!(r.library_id, 1);
        assert_eq!(r.absolute_path, "/music/songs/track.mp3");
    }

    #[test]
    fn resolves_relative_path_to_different_library() {
        let f = folder();
        let r = resolver().resolve_path("../../podcasts/episode.mp3", Some(&f));
        assert!(r.valid);
        assert_eq!(r.library_id, 3);
        assert_eq!(r.library_path, "/podcasts");
    }

    #[test]
    fn uses_matcher_to_find_correct_library_for_resolved_path() {
        let f = folder();
        let r = resolver().resolve_path("../../music-classical/track.mp3", Some(&f));
        assert!(r.valid);
        assert_eq!(r.library_id, 2);
        assert_eq!(r.library_path, "/music-classical");
    }

    #[test]
    fn returns_invalid_for_relative_paths_escaping_all_libraries() {
        let f = folder();
        let r = resolver().resolve_path("../../../../etc/passwd", Some(&f));
        assert!(!r.valid);
    }

    #[test]
    fn handles_playlist_referencing_file_in_other_library() {
        let f = folder();
        let r = resolver().resolve_path("../../podcasts/show/episode.mp3", Some(&f));
        assert!(r.valid);
        assert_eq!(r.library_id, 3, "Should resolve to podcasts library");
        assert_eq!(r.library_path, "/podcasts");
    }

    #[test]
    fn prefers_longer_library_paths() {
        let r = resolver().resolve_path("/music-classical/baroque/track.mp3", None);
        assert!(r.valid);
        assert_eq!(r.library_id, 2, "Should match /music-classical, not /music");
    }

    #[test]
    fn to_qualified_string_converts_valid_resolution() {
        let r = PathResolution {
            absolute_path: "/music/artist/album/track.mp3".into(),
            library_path: "/music".into(),
            library_id: 1,
            valid: true,
        };
        assert_eq!(r.to_qualified_string().unwrap(), "1:artist/album/track.mp3");
    }

    #[test]
    fn to_qualified_string_uses_forward_slashes() {
        let r = PathResolution {
            absolute_path: "/music/artist\\album\\track.mp3".into(),
            library_path: "/music".into(),
            library_id: 2,
            valid: true,
        };
        let s = r.to_qualified_string().unwrap();
        assert!(s.starts_with("2:"));
        assert!(!s.contains('\\'));
    }

    #[test]
    fn to_qualified_string_errors_for_invalid() {
        let r = PathResolution {
            valid: false,
            ..PathResolution::default()
        };
        assert!(r.to_qualified_string().is_err());
    }

    #[test]
    fn classify_line_playlist_directive() {
        let e = classify_line("#PLAYLIST:My List");
        assert_eq!(e.playlist_name.as_deref(), Some("My List"));
        assert_eq!(e.path, None);
    }

    #[test]
    fn classify_line_art_url_directive() {
        // The directive value keeps its leading space here; the trim happens
        // in resolve_image_url, matching resolveImageURL.
        let e = classify_line("#EXTALBUMARTURL: https://example.com/cover.jpg");
        assert_eq!(
            e.external_image_url.as_deref(),
            Some(" https://example.com/cover.jpg")
        );
    }

    #[test]
    fn classify_line_skips_comments_and_empty() {
        assert_eq!(classify_line("").path, None);
        assert_eq!(classify_line("#EXTM3U").path, None);
        assert_eq!(classify_line("#EXTINF:123,Artist - Title").path, None);
    }

    #[test]
    fn classify_line_unescapes_file_url() {
        let e = classify_line("file:///music/s%C3%A9ng.mp3");
        assert_eq!(e.path.as_deref(), Some("/music/séng.mp3"));
    }

    #[test]
    fn classify_line_rejects_non_audio() {
        assert_eq!(classify_line("/music/track.txt").path, None);
        assert_eq!(classify_line("/music/playlist.m3u").path, None);
        assert!(classify_line("/music/track.mp3").path.is_some());
    }

    #[test]
    fn is_audio_and_image_file() {
        assert!(is_audio_file("/music/track.mp3"));
        assert!(!is_audio_file("/music/cover.jpg"));
        assert!(is_image_file("/music/cover.jpg"));
        assert!(!is_image_file("/music/track.mp3"));
    }

    #[test]
    fn parse_m3u_text_directives_and_paths() {
        let text =
            "#EXTM3U\r\n#PLAYLIST:Test\n\r\n#EXTALBUMARTURL:https://x/c.jpg\n../songs/a.mp3\n";
        let (name, art, paths) = parse_m3u_text(text);
        assert_eq!(name.as_deref(), Some("Test"));
        assert_eq!(art.as_deref(), Some("https://x/c.jpg"));
        assert_eq!(paths, vec!["../songs/a.mp3"]);
    }

    #[test]
    fn parse_m3u_text_cr_ending() {
        // Old Mac CR-only line endings split via lines_from.
        let (name, _, paths) = parse_m3u_text("#PLAYLIST:CR\r../a.mp3\r../b.mp3\r");
        assert_eq!(name.as_deref(), Some("CR"));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn resolve_image_url_http_requires_flag() {
        let m = resolver();
        assert_eq!(
            resolve_image_url(
                "https://example.com/cover.jpg",
                Some(&folder()),
                &m,
                &[],
                false
            ),
            ""
        );
        assert_eq!(
            resolve_image_url(
                "https://example.com/cover.jpg",
                Some(&folder()),
                &m,
                &[],
                true
            ),
            "https://example.com/cover.jpg"
        );
    }

    #[test]
    fn resolve_image_url_local_absolute_path() {
        let m = resolver();
        assert_eq!(
            resolve_image_url("/music/cover.jpg", Some(&folder()), &m, &[], false),
            "/music/cover.jpg"
        );
        // Outside every library -> rejected
        assert_eq!(
            resolve_image_url("/outside/cover.jpg", Some(&folder()), &m, &[], false),
            ""
        );
    }

    #[test]
    fn resolve_image_url_relative_path() {
        let m = resolver();
        assert_eq!(
            resolve_image_url("../cover.jpg", Some(&folder()), &m, &[], false),
            "/music/cover.jpg"
        );
        // Escaping all libraries -> rejected
        assert_eq!(
            resolve_image_url("../../outside/cover.jpg", Some(&folder()), &m, &[], false),
            ""
        );
    }

    #[test]
    fn resolve_image_url_file_url() {
        let m = resolver();
        assert_eq!(
            resolve_image_url("file:///music/c.jpg", Some(&folder()), &m, &[], false),
            "/music/c.jpg"
        );
        assert_eq!(
            resolve_image_url("file:///outside/c.jpg", Some(&folder()), &m, &[], false),
            ""
        );
    }

    #[test]
    fn resolve_image_url_rejects_non_image() {
        let m = resolver();
        assert_eq!(
            resolve_image_url("/music/track.mp3", Some(&folder()), &m, &[], false),
            ""
        );
    }

    #[test]
    fn resolve_image_url_restricted_to_owner_libraries_without_folder() {
        let m = resolver();
        // No folder: owner has library 1 only; library 3 path is rejected.
        assert_eq!(
            resolve_image_url("/podcasts/cover.jpg", None, &m, &[1], false),
            ""
        );
        assert_eq!(
            resolve_image_url("/music/cover.jpg", None, &m, &[1], false),
            "/music/cover.jpg"
        );
    }

    #[test]
    fn lookup_candidates_dedupe() {
        // "abc" is normalization-invariant and lowercase already: 1 candidate.
        assert_eq!(lookup_candidates("abc"), vec!["abc"]);
        let c = lookup_candidates("ABC");
        assert_eq!(c[0], "ABC");
        assert!(c.contains(&"abc".to_string()));
    }

    #[test]
    fn lookup_key_normalizes() {
        let nfc_form = "caf\u{e9}"; // é precomposed
        assert_eq!(lookup_key(1, nfc_form), "1:café");
        let nfd_form = "cafe\u{301}"; // e + combining acute
        assert_eq!(lookup_key(1, nfd_form), "1:café");
        assert_eq!(lookup_key(1, nfc_form), lookup_key(1, nfd_form));
    }

    #[test]
    fn clean_and_relate() {
        assert_eq!(clean_path("/music//a/../a/x.mp3"), "/music/a/x.mp3");
        assert_eq!(clean_path("a/b"), "a/b");
        assert_eq!(relate("/music", "/music/a/b").unwrap(), "a/b");
        assert_eq!(relate("/music/x", "/music").unwrap(), "..");
        assert!(relate("/music", "relative").is_err());
    }
}
