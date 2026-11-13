//! `core/storage/local/extractors.go` — extractor registry.
//! Port of `type Extractor interface { Parse; Version }` +
//! `RegisterExtractor(id, constructor)`; same RWMutex-guarded map pattern as
//! [`crate::storage`].

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::backend::{read_metadata, BACKEND_VERSION};
use crate::gotaglib::ExtractMetadataResult;

/// `extractors.go:13-16` — map id -> extractorConstructor, RWMutex-guarded.
static EXTRACTORS: OnceLock<RwLock<HashMap<String, ExtractorConstructor>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, ExtractorConstructor>> {
    EXTRACTORS.get_or_init(|| {
        // Go registers the default in adapters/gotaglib/gotaglib.go:294-297.
        let constructor: ExtractorConstructor = |fs_root, base_dir| Extractor { fs_root, base_dir };
        RwLock::new(HashMap::from([
            ("taglib".to_owned(), constructor),
            ("lofty".to_owned(), constructor),
        ]))
    })
}

/// Constructor: Go's `func(fs fs.FS, baseDir string) Extractor`
/// (`extractors.go:17`). The real backend takes the filesystem root and base
/// directory as plain paths.
pub type ExtractorConstructor = fn(fs_root: String, base_dir: String) -> Extractor;

/// `RegisterExtractor` (`extractors.go:23-29`).
pub fn register_extractor(id: &str, f: ExtractorConstructor) {
    registry()
        .write()
        .expect("extractor registry lock poisoned")
        .insert(id.to_string(), f);
}

/// Looks up a registered extractor by id — used by the local storage to pick
/// the one configured by `Scanner.Extractor` (`local.go` `MusicFS::ReadTags`).
pub fn get_extractor(id: &str) -> Result<ExtractorConstructor, String> {
    let reg = registry().read().expect("extractor registry lock poisoned");
    match reg.get(&id.to_string()) {
        Some(c) => Ok(*c),
        None => Err(format!("extractor '{}' not registered", id)),
    }
}

/// The extractor interface — `extractors.go:8-11`
/// `Parse(files ...string) (map[string]Info, error)` + `Version()`.
#[derive(Debug, Clone)]
pub struct Extractor {
    /// Filesystem root the extractor opens files under (`fs fs.FS`).
    pub fs_root: String,
    /// Base directory for relative paths (`baseDir`).
    pub base_dir: String,
}

impl Extractor {
    /// `adapters/gotaglib/gotaglib.go:34-43`: successful paths map to Info;
    /// missing, malformed or unsupported media are skipped, not batch errors.
    pub fn parse(self: &Extractor, files: Vec<String>) -> HashMap<String, ExtractMetadataResult> {
        let mut results = HashMap::new();
        for path in files {
            // Go opens through fs.FS (gotaglib.go:127-143): names must be
            // relative and cannot escape through '..'. Keep that boundary;
            // read_metadata itself intentionally accepts arbitrary OS paths.
            if path.is_empty() || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
                continue;
            }
            let full_path = std::path::Path::new(&self.fs_root)
                .join(&self.base_dir)
                .join(&path);
            match read_metadata(full_path) {
                Err(_) => continue,
                Ok(info) => {
                    results.insert(path, info);
                }
            }
        }
        results
    }

    /// Actual backend version (the registry's legacy `taglib` name is retained).
    pub fn version(self: Extractor) -> String {
        BACKEND_VERSION.to_string()
    }
}
