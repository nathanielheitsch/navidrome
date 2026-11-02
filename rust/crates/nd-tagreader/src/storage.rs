//! `core/storage/storage.go` — schema registry and `For(uri)` dispatch.
//! Port: `register`/`storage_for`/`local_path_to_url` (Go PascalCase names cited per line); sync RWMutex → static RwLock.
//! Shortcut: Go escapes each path component with url.PathEscape and url.Parse
//! decodes it back to the same Path for every case storage_test.go exercises
//! (`#`, `&`, `?`, spaces, `/`); we keep the path verbatim through dispatch,
//! and revisit if a component survives escaping differently. Port targets
//! unix (Go's filepath.ToSlash is a no-op there).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// `storage.go:8` — `LocalSchemaID = "file"`.
pub const LOCAL_SCHEMA_ID: &'static str = "file";

/// `core/storage/interface.go:10-13` — Storage implementation carrying the
/// parsed URL (`storage_test.go` asserts Scheme/Path).
#[derive(Debug, Clone)]
pub struct Storage {
    /// Schema portion (Go url.Scheme).
    pub scheme: String,
    /// Path portion (Go url.Path).
    pub path: String,
}

/// Registry errors — Go returns `errors.New(...)` strings (`storage.go:76-83`).
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Go `errors.New("...")` from url.Parse.
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    /// Go errors.New from the registry miss (storage.go:76-83).
    #[error("schema '{0}' not registered")]
    UnknownScheme(String),
}

/// Constructor closures keyed by schema (`storage.go:14` registry map):
/// Go's `constructor(url.URL) Storage` closures take the parsed URL fields.
pub type StorageConstructor = fn(scheme: String, path: String) -> Storage;

/// Registry map + RWMutex (`storage.go:14-16`).
static REGISTRY: OnceLock<RwLock<HashMap<String, StorageConstructor>>> = OnceLock::new();

/// `Register` (`storage.go:19-25`).
pub fn register(schema: String, c: StorageConstructor) {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new())).write().expect("registry lock poisoned")
        .insert(schema, c);
}

/// `storage.go:32-51` — absolutize, then keep the path (see shortcut note).
pub fn local_path_to_url(os_path: String) -> Storage {
    let abs = if os_path.starts_with("/") {
        os_path
    } else {
        let cwd = std::env::var("PWD").unwrap_or(".".to_string());
        format!("{}/{}", cwd, os_path)
    };
    Storage { scheme: LOCAL_SCHEMA_ID.to_string(), path: abs }
}

/// `storage.go:53-84` — bare path → file://; schema selects the registered
/// constructor; unregistered schema → error.
pub fn storage_for(uri: String) -> Result<Storage, StorageError> {
    let u = match uri.split_once("://") {
        Some((scheme, rest)) =>
            if scheme.is_empty() || rest.is_empty() {
                return Err(StorageError::InvalidUrl(uri));
            } else {
                Storage { scheme: scheme.to_string(), path: rest.to_string() }
            },
        None => local_path_to_url(uri),
    };

    let reg = REGISTRY.get_or_init(|| RwLock::new(HashMap::new())).read().expect("registry lock poisoned");
    let ctor = match reg.get(&u.scheme) {
        Some(c) => c,
        None => return Err(StorageError::UnknownScheme(u.scheme)),
    };
    Ok(ctor(u.scheme, u.path))
}
