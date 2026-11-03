// nd-dbschema: owned by #04 (docs/rust-port/issues.md Part B)

//! #04 — Rust port of `db/**` schema/migration plumbing (see
//! `docs/rust-port/issues.md` Part B and `docs/rust-port/schema-contract.md`).
//!
//! Schema half (this pass): [`schema`] — open/init of the SQLite DB with the
//! per-connection SQL extensions Go registers in its driver `ConnectHook`
//! (`db/db.go:44-51`), plus the single fresh-schema migration that installs the
//! 31-table set (`schema-contract.md` §1) with the §3 FTS5 tables always present.
//!
//! Out of scope for this pass (later run owns them, so they are not even
//! declared here): optimize/backup/repair, the goose offset semantics of
//! `db/db.go`, and the incremental migration machinery.

/// Scaffold marker: the public surface authored by #04 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 4;

pub mod migrate;
pub mod schema;
pub mod schema_ddl;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_dbschema_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 4;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-dbschema instantiates without pulling in the world"
        );
    }
}
