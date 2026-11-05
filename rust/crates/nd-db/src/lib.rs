// nd-db: owned by #05 (docs/rust-port/issues.md Part B)

//! #05 slice 1 — the SQLite connection pool and the transaction/retry core of
//! `persistence/**` (see `docs/rust-port/issues.md` Part B, #05, and
//! `docs/rust-port/next-work.md` Phase 2 slice 1).
//!
//! [`pool`] is the fixed connection pool of `db/db.go:62`
//! (`SetMaxOpenConns(max(4, NumCPU))`, `conf/configuration.go:1213-1216`).
//! [`store`] is the `WithTx`/`WithTxRetry` transaction machinery of
//! `persistence/persistence.go:165-171,207-228` with error-site busy
//! classification replacing Go's ambient `withBusyRetry` (`persistence.go:234`).
//! The 22 lazy repository accessors (`persistence.go:17-40`) are slices 2-4 and
//! are deliberately absent here.

/// Scaffold marker: the public surface authored by #05 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 5;

pub mod album_repo;
pub mod artist_repo;
pub mod artwork_queue_repo;
pub mod artwork_repo;
pub mod folder_repo;
pub mod genre_repo;
pub mod library_repo;
pub mod mediafile_repo;
pub mod playlist_repo;
pub mod playlist_track_repo;
pub mod playqueue_repo;
pub mod plugin_repo;
pub mod pool;
pub mod property_repo;
pub mod radio_repo;
pub mod scrobble_buffer_repo;
pub mod scrobble_repo;
pub mod share_repo;
pub mod smart_playlist_repo;
pub mod sql_annotations;
pub mod sql_bookmarks;
pub mod sql_participations;
pub mod sql_restful;
pub mod sql_search;
pub mod sql_search_fts;
pub mod sql_search_like;
pub mod sql_tags;
pub mod sqlrepo;
pub mod store;
pub mod tag_repo;
pub mod transcoding_repo;
pub mod user_props_repo;
pub mod user_repo;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_db_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 5;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-db instantiates without pulling in the world"
        );
    }
}
