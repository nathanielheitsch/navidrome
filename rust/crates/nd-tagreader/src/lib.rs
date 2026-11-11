// nd-tagreader: owned by #08 (docs/rust-port/issues.md Part B)

//! #08 — Tag extractor: `core/storage/**` registry + `adapters/gotaglib/**`
//! transforms (see `docs/rust-port/issues.md` Part B).
//!
//! Real media files are read by pinned pure-Rust Lofty: no C toolchain, no
//! subprocess and no runtime sidecar producer required. The legacy `taglib`
//! registry ID is retained; `lofty` is an alias. Go-emitted sidecars remain
//! test oracles for the post-transform, never an implicit runtime fallback.
//! See the crate README for supported fields and explicit parity gaps.

pub mod backend;

pub mod extractors;
pub mod gotaglib;
pub mod info;
pub mod storage;

pub use backend::read_metadata;
pub use extractors::{get_extractor, register_extractor, Extractor};
pub use gotaglib::{ExtractMetadataResult, RawTags};
pub use info::{AudioProperties, Info};
pub use storage::{local_path_to_url, register, storage_for, Storage, LOCAL_SCHEMA_ID};
