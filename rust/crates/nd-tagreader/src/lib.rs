// nd-tagreader: owned by #08 (docs/rust-port/issues.md Part B)

//! #08 — Tag extractor: `core/storage/**` registry + `adapters/gotaglib/**`
//! transforms (see `docs/rust-port/issues.md` Part B).
//!
//! Backend decision (issue #08 says "choose once here"): pure-Rust TagLib is
//! unavailable (no crate in the registry, no libtag on the system) and the C
//! binding would fork the pinned `github.com/deluan/go-taglib` raw-tag surface
//! (`go.mod:5-6`, `docs/rust-port/component-map.md:146`). We chose the **shim
//! backend**: the extractor consumes the raw TagLib surface (PropertyMap tags,
//! raw frames/atoms/attrs, stream properties) as data — produced today by the
//! pinned fork, tomorrow by whatever binary the operator wires — and the
//! Navidrome-side contract below is ported byte-exactly: registration, the
//! `Info` map shape, and every gotaglib post-transform. The raw-tag surface
//! itself stays owned by the pinned fork boundary, not reimplemented here.

pub mod info;
pub mod storage;
pub mod extractors;
pub mod gotaglib;

pub use info::{Info, AudioProperties};
pub use storage::{storage_for, local_path_to_url, register, Storage, LOCAL_SCHEMA_ID};
pub use extractors::{Extractor, register_extractor, get_extractor};
pub use gotaglib::{RawTags, ExtractMetadataResult};
