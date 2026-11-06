//! Filesystem helpers — port of `utils/files.go`. `TempFileName` uses
//! [`crate::nanoid`] with the Go `id.NewRandom()` alphabet instead of
//! depending on the `nd-id` crate (crate-boundary decision; Go used the same
//! 22-char base62 generator).

use std::path::Path;

/// Returns a temporary file name: OS temp dir + `prefix` + random id + `suffix`.
#[must_use]
pub fn temp_file_name(prefix: &str, suffix: &str) -> String {
    const ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let random = crate::nanoid::generate(ALPHABET, 22).unwrap_or_else(|_| "0".repeat(22));
    let dir = std::env::temp_dir();
    dir.join(format!("{prefix}{random}{suffix}"))
        .display()
        .to_string()
}

/// Returns the base name of `file_path` without its extension
/// (`utils.BaseName`).
#[must_use]
pub fn base_name(file_path: &str) -> String {
    let p = Path::new(file_path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name.rfind('.') {
        Some(_) => match name.rfind('.') {
            Some(i) => name[..i].to_string(),
            None => String::new(),
        },
        None => name.to_string(),
    }
}

/// Produces a filesystem-safe, human-readable version of a name
/// (`utils.CleanFileName`): lowercases, replaces spaces with underscores,
/// strips non-alphanumerics (except `_`/`-`), truncates to 50 characters, and
/// trims trailing `_-`.
#[must_use]
pub fn clean_file_name(name: &str) -> String {
    let s = name.trim().to_lowercase().replace(' ', "_");
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    let mut s = s;
    if s.len() > 50 {
        s = s[..50].to_string();
    }
    s.trim_end_matches(['_', '-']).to_string()
}

/// Checks if a file or directory exists (`utils.FileExists`).
#[must_use]
pub fn file_exists(path: &str) -> bool {
    let md = std::fs::metadata(path);
    match md {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_name_has_prefix_and_suffix() {
        let result = temp_file_name("test-", ".tmp");
        assert!(result.contains("test-"));
        assert!(result.ends_with(".tmp"));
        assert!(result.starts_with(
            std::env::temp_dir()
                .display()
                .to_string()
                .trim_end_matches('/')
        ));
    }

    #[test]
    fn temp_file_name_is_unique() {
        assert_ne!(
            temp_file_name("unique-", ".test"),
            temp_file_name("unique-", ".test")
        );
    }

    #[test]
    fn base_name_extracts_the_basename() {
        assert_eq!(base_name("test.mp3"), "test");
        assert_eq!(base_name("/path/to/file.txt"), "file");
        assert_eq!(base_name("/path/to/filename"), "filename");
        assert_eq!(base_name("archive.tar.gz"), "archive.tar");
        assert_eq!(base_name(".hidden"), "");
        assert_eq!(base_name(".config.json"), ".config");
        assert_eq!(base_name(""), "");
        assert_eq!(base_name("/path/to/dir/"), "dir");
        assert_eq!(base_name("/very/long/path/to/my/favorite/song.mp3"), "song");
    }

    #[test]
    fn clean_file_name_cases() {
        assert_eq!(clean_file_name("My Cool Playlist"), "my_cool_playlist");
        assert_eq!(clean_file_name("Rock & Roll! (2024)"), "rock__roll_2024");
        // Música loses the ú (non-ASCII stripped), matching Go's [^a-z0-9_-].
        assert_eq!(clean_file_name("Música Favorita"), "msica_favorita");
        assert_eq!(clean_file_name("lo-fi beats"), "lo-fi_beats");
        assert_eq!(clean_file_name(""), "");
        assert_eq!(clean_file_name("   "), "");
        assert_eq!(clean_file_name("!!!@@@###"), "");
        let long = "abcdefghij".repeat(10);
        assert_eq!(clean_file_name(&long).len(), 50);
        // 49 a's + space + "b": truncates to 49 a's + "_", then trims the "_".
        let name = format!("{} b", "a".repeat(49));
        assert_eq!(clean_file_name(&name), "a".repeat(49));
    }

    #[test]
    fn file_exists_cases() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        assert!(file_exists(file.path().to_str().expect("utf8 path")));
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(file_exists(dir.path().to_str().expect("utf8 path")));
        assert!(!file_exists(&format!(
            "{}/does-not-exist.txt",
            dir.path().display()
        )));
        assert!(!file_exists(""));
        assert!(!file_exists(
            "/this/path/definitely/does/not/exist/file.txt"
        ));
    }
}
