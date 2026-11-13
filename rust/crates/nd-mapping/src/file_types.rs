//! Port of `model/file_types.go` — audio/image/playlist classification, which
//! the scanner (#17) and the mapping consumer use to decide what to ingest.
//!
//! Go asks the stdlib `mime` package, whose table is seeded from
//! `resources/mime_types.yaml` by `conf/mime/mime_types.go:19-40`. This port
//! reads the same embedded YAML ([`crate::mappings::MIME_TYPES_YAML`]) and
//! layers it over Go's built-in extension table
//! (`go/src/mime/mediatype.go:30-86`), preserving Go's lookup order: a
//! user-added entry wins, then the program-registered entries, then the
//! built-ins.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// `excludeAudioType` (`model/file_types.go:10-14`).
const EXCLUDE_AUDIO_TYPES: &[&str] = &["audio/mpegurl", "audio/x-mpegurl", "audio/x-scpls"];

/// The effective extension → MIME table (built-ins + `mime_types.yaml`).
pub struct MimeTable {
    types: BTreeMap<String, String>,
    lossless: Vec<String>,
}

impl MimeTable {
    /// Built-ins first, then the YAML overlay — Go's `mime.TypeByExtension`
    /// checks `extensionMimeMap` (which the YAML populates) before falling back
    /// to `builtinExtensionMime`.
    #[must_use]
    pub fn build(yaml: &str) -> Self {
        let mut types: BTreeMap<String, String> = BTreeMap::new();
        for (ext, mime) in BUILTIN_EXTENSION_MIME {
            types.insert((*ext).to_string(), (*mime).to_string());
        }
        let mut lossless: Vec<String> = Vec::new();
        if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            if let Some(map) = doc.get("types").and_then(|v| v.as_mapping()) {
                for (k, v) in map {
                    if let (Some(ext), Some(mime)) = (k.as_str(), v.as_str()) {
                        types.insert(ext.to_string(), mime.trim().to_string());
                    }
                }
            }
            if let Some(list) = doc.get("lossless").and_then(|v| v.as_sequence()) {
                lossless = list
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|ext| ext.trim_start_matches('.').to_string())
                    .collect();
            }
        }
        Self { types, lossless }
    }

    /// `mime.TypeByExtension` (`go/src/mime/mediatype.go:127-159`): lowercase
    /// the extension, require the leading dot, and return the exact match with
    /// no parameters.
    #[must_use]
    pub fn type_by_extension(&self, extension: &str) -> Option<&str> {
        let ext = normalize_extension(extension);
        if ext.is_empty() {
            return None;
        }
        self.types.get(&ext).map(String::as_str)
    }

    /// `mime.LosslessFormats` (`conf/mime/mime_types.go:38-40`).
    #[must_use]
    pub fn lossless_formats(&self) -> &[String] {
        &self.lossless
    }
}

/// Normalizes an extension the way Go does: `lowercaseExt` requires a leading
/// dot and rejects anything containing a slash.
fn normalize_extension(extension: &str) -> String {
    if !extension.starts_with('.') {
        return String::new();
    }
    if extension.contains('/') {
        return String::new();
    }
    extension.to_lowercase()
}

/// `model.IsAudioFile` (`model/file_types.go:16-21`): an `audio/*` type that is
/// not one of the excluded playlist types.
#[must_use]
pub fn is_audio_file(file_path: &str) -> bool {
    let Some(mime_type) = table().type_by_extension(extension(file_path)) else {
        return false;
    };
    if EXCLUDE_AUDIO_TYPES.contains(&mime_type) {
        return false;
    }
    mime_type.starts_with("audio/")
}

/// `model.IsImageFile` (`model/file_types.go:23-26`).
#[must_use]
pub fn is_image_file(file_path: &str) -> bool {
    table()
        .type_by_extension(extension(file_path))
        .is_some_and(|m| m.starts_with("image/"))
}

/// `model.IsValidPlaylist` (`model/file_types.go:28-31`): lowercased extension
/// in `.m3u`, `.m3u8`, `.nsp`.
#[must_use]
pub fn is_valid_playlist(file_path: &str) -> bool {
    matches!(
        extension(file_path).to_lowercase().as_str(),
        ".m3u" | ".m3u8" | ".nsp"
    )
}

/// `filepath.Ext` (`path/filepath`): the suffix from the final dot in the final
/// path element.
pub(crate) fn extension(file_path: &str) -> &str {
    let name = file_path.rsplit('/').next().unwrap_or(file_path);
    match name.rfind('.') {
        Some(i) => &name[i..],
        None => "",
    }
}

/// The process-wide table, built once from the embedded YAML.
#[must_use]
pub fn table() -> &'static MimeTable {
    static ONCE: OnceLock<MimeTable> = OnceLock::new();
    ONCE.get_or_init(|| MimeTable::build(crate::mappings::MIME_TYPES_YAML))
}

/// Go's built-in `mime` extension table (`go/src/mime/mediatype.go`), the
/// fallback under `mime_types.yaml`. Only the entries relevant to Navidrome's
/// audio/image/playlist classification are reproduced; the YAML file covers the
/// rest of the audio and image extensions.
const BUILTIN_EXTENSION_MIME: &[(&str, &str)] = &[
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".jfif", "image/jpeg"),
    (".png", "image/png"),
    (".webp", "image/webp"),
    (".gif", "image/gif"),
    (".bmp", "image/bmp"),
    (".tif", "image/tiff"),
    (".tiff", "image/tiff"),
    (".heic", "image/heic"),
    (".heif", "image/heif"),
    (".mp3", "audio/mpeg"),
    (".ogg", "audio/ogg"),
    (".oga", "audio/ogg"),
    (".opus", "audio/ogg"),
    (".flac", "audio/flac"),
    (".wav", "audio/wav"),
    (".m4a", "audio/mp4"),
    (".m4b", "audio/mp4"),
    (".mp4", "video/mp4"),
    (".aac", "audio/aac"),
    (".m3u", "audio/x-mpegurl"),
    (".m3u8", "application/x-mpegURL"),
    (".pls", "audio/x-scpls"),
    (".webm", "video/webm"),
    (".txt", "text/plain; charset=utf-8"),
    (".json", "application/json"),
    (".css", "text/css; charset=utf-8"),
    (".htm", "text/html; charset=utf-8"),
    (".html", "text/html; charset=utf-8"),
    (".js", "text/javascript; charset=utf-8"),
];
