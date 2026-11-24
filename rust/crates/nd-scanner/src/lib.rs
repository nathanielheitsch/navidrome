// nd-scanner: owned by #17 (docs/rust-port/issues.md Part B)

//! Scanner slice of the Rust port: local storage facts, `.ndignore` policy, folder
//! fingerprints and a bounded cancellable postorder walk.
//!
//! Scope actually implemented here — the **nondestructive SC-02 remainder**
//! (`docs/rust-port/subplans/scanner.md` SC-02):
//! - [`storage`] — real local `stat`/`read`/`list`/`read_link`/`resolve`, logical paths,
//!   `file://` URI parsing, symlink classification, plus the additive
//!   [`storage::LocalDir::list_annotated`] seam that keeps an entry's own `lstat` facts
//!   distinct from its resolved endpoint's facts.
//! - [`ignore`] — `.ndignore` loading and a pinned translation of the Go matcher
//!   (`sabhiram/go-gitignore` at `go.mod:52`), including its stack semantics.
//! - [`folder`] — the immutable folder snapshot and the Go-compatible MD5 fingerprint,
//!   with the Go `time.Time.String` UTC spelling ([`folder::go_time_string`]).
//! - [`walk`] — iterative postorder walk over configured library roots, emitting
//!   [`walk::FolderEvent`] through a **capacity-1** bounded stream with wakeable
//!   cancellation and a separately awaited terminal outcome ([`walk::WalkHandle`]).
//! - [`contract`] — request/target validation, typed errors and warnings, and
//!   [`contract::CancelToken`].
//! - [`aggregate`] — SC-04's pure `MediaFiles.ToAlbum` loop over an album group: path-
//!   sorted aggregation of duration/size/discs/tags/participants/artwork/times, with no
//!   I/O and no database access.
//!
//! ## Explicitly not here
//!
//! No database access, no tag reading, no persistence, no controller/phases, no
//! missing-state or destructive finalization, no watcher, no external progress IPC, no
//! registry. **This is not SC-02 completion and not MusicFS readiness**: album/artist
//! projection, `ReadTags`, and DB writes belong to SC-03..SC-06. Nothing in this crate
//! opens a database resource.
//!
//! ## Platform honesty
//!
//! Verified on Unix (macOS/Linux). Windows path/volume/junction behaviour is neither
//! implemented nor exercised, and is a declared coverage gap rather than parity.
//! Non-UTF-8 entry names are reported as typed warnings and excluded, never lossily
//! rendered into an identity.

pub mod contract;
// SC-04: pure multi-track album aggregation (Go `MediaFiles.ToAlbum`) and the semantic
// equality the refresh phases need. No I/O, no DB writes.
pub mod aggregate;
pub mod folder;
pub mod ignore;
// SC-03: the tag→record bridge and the singleton folder projection.
pub mod mapping_bridge;
// SC-05: the phase-1 ingest vertical for one folder (walk -> tag -> bridge -> batch ->
// DB-14 persist). Consumes the SC-02 storage facts, SC-03's bridge/projection, and the
// ratified DB-11/DB-14 seams; SC-06 owns the rest of phase 1.
pub mod phase1;
pub mod projection;
pub mod storage;
pub mod walk;

/// Scaffold marker: kept so the #S1 crate-graph contract still has something to assert.
/// The scanner's real public surface lives in the modules above.
pub const OWNED_BY_ISSUE: u8 = 17;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_scanner_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 17;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-scanner instantiates without pulling in the world"
        );
    }
}
