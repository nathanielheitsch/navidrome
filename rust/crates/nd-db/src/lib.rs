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

pub mod pool;
pub mod store;

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
