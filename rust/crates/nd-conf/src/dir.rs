//! Port of `conf/dir.go`: a lazy directory path value type.
//!
//! `Dir` wraps a directory path and creates the directory on demand. It is a
//! plain value type — safe to copy, compare, and print. Directory creation is
//! delegated to [`std::fs::create_dir_all`] on every [`Dir::path`] call, which
//! is idempotent, so repeated calls cost one stat syscall when the directory
//! already exists. An empty path is returned as-is with no error.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Default permissions (`0o777` before umask), matching `os.ModePerm`.
const DEFAULT_PERM: u32 = 0o777;

/// A directory path that is created lazily on [`Dir::path`]. Port of the Go
/// `conf.Dir` value type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dir {
    path: String,
    perm: u32,
}

impl Dir {
    /// Creates a new [`Dir`] with the given path and default permissions.
    pub fn new(path: impl Into<String>) -> Self {
        Dir {
            path: path.into(),
            perm: DEFAULT_PERM,
        }
    }

    /// Creates a new [`Dir`] with the given path and explicit permissions.
    /// A perm of 0 is treated as "default" and resolves to `0o777` at
    /// directory-creation time.
    pub fn with_perm(path: impl Into<String>, perm: u32) -> Self {
        Dir {
            path: path.into(),
            perm: if perm == 0 { DEFAULT_PERM } else { perm },
        }
    }

    /// Returns the raw path without creating the directory.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// True when the path is empty. Empty paths are returned as-is by
    /// [`Dir::path`] with no side effects.
    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }

    /// Ensures the directory exists (permissions applied on unix) and returns
    /// its path. Safe to call repeatedly; an empty path is returned as-is.
    pub fn path(&self) -> std::io::Result<PathBuf> {
        if self.path.is_empty() {
            return Ok(PathBuf::new());
        }
        let path = Path::new(&self.path);
        if !path.exists() {
            fs::create_dir_all(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(self.perm))?;
            }
        }
        Ok(path.to_path_buf())
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

impl Default for Dir {
    fn default() -> Self {
        Dir::new("")
    }
}

impl serde::Serialize for Dir {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.path)
    }
}

impl<'de> serde::Deserialize<'de> for Dir {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Dir::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_returned_as_is() {
        let dir = Dir::default();
        assert_eq!(dir.path().unwrap(), PathBuf::new());
        assert!(dir.is_empty());
    }

    #[test]
    fn path_creates_directory_idempotently() {
        let tmp = std::env::temp_dir().join(format!("nd-conf-dir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = Dir::new(tmp.join("a/b").to_str().unwrap());
        assert_eq!(dir.path().unwrap(), tmp.join("a/b"));
        // Idempotent: second call succeeds without error.
        assert!(dir.path().is_ok());
        assert!(tmp.join("a/b").is_dir());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn serde_round_trips_as_plain_string() {
        // A TOML document is a key/value map, so `Dir` is exercised in the
        // field position that carries it; it must round-trip as a plain string.
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Root {
            p: Dir,
        }

        let dir = Dir::new("/tmp/x");
        let s = toml::to_string(&Root { p: dir.clone() }).unwrap();
        assert!(s.contains("/tmp/x"));
        let back: Root = toml::from_str(&s).unwrap();
        assert_eq!(back.p.as_str(), dir.as_str());
    }
}
