//! `core/storage/local/extractors.go` — extractor registry.
//! Port of `type Extractor interface { Parse; Version }` +
//! `RegisterExtractor(id, constructor)`; same RWMutex-guarded map pattern as
//! [`crate::storage`].

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::gotaglib::{
    extract_metadata, load_shim_raw_tags, ExtractMetadataResult, PINNED_TAGLIB_VERSION,
};

/// `extractors.go:13-16` — map id -> extractorConstructor, RWMutex-guarded.
static EXTRACTORS: OnceLock<RwLock<HashMap<String, ExtractorConstructor>>> = OnceLock::new();

/// Constructor: Go's `func(fs fs.FS, baseDir string) Extractor`
/// (`extractors.go:17`). The shim backend takes the filesystem root and base
/// directory as plain paths.
pub type ExtractorConstructor = fn(fs_root: String, base_dir: String) -> Extractor;

/// `RegisterExtractor` (`extractors.go:23-29`).
pub fn register_extractor(id: &str, f: ExtractorConstructor) {
    EXTRACTORS
        .get_or_init(|| RwLock::new(HashMap::new()))
        .write()
        .expect("extractor registry lock poisoned")
        .insert(id.to_string(), f);
}

/// Looks up a registered extractor by id — used by the local storage to pick
/// the one configured by `Scanner.Extractor` (`local.go` `MusicFS::ReadTags`).
pub fn get_extractor(id: &str) -> Result<ExtractorConstructor, String> {
    let reg = EXTRACTORS
        .get_or_init(|| RwLock::new(HashMap::new()))
        .read()
        .expect("extractor registry lock poisoned");
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
    /// `Parse` (`extractors.go:10-20`): maps each path to its Info; files that
    /// fail are skipped (the Go loop `continue`s on error, Parse itself never
    /// errors).
    pub fn parse(self: &Extractor, files: Vec<String>) -> HashMap<String, ExtractMetadataResult> {
        let mut results = HashMap::new();
        for path in files {
            match load_shim_raw_tags(&self.fs_root, &self.base_dir, &path) {
                Err(_) => continue,
                Ok(raw) => {
                    let info = extract_metadata(&raw);
                    results.insert(path.to_string(), info);
                }
            }
        }
        results
    }

    /// `Version` (`extractors.go:26-36`): Go reads build info for
    /// go.senan.xyz/taglib; the pinned fork version (go.mod:6).
    pub fn version(self: Extractor) -> String {
        PINNED_TAGLIB_VERSION.to_string()
    }
}
