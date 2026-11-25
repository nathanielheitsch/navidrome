// nd-cli: owned by #33 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-cli: this crate instantiates empty so later issues
//! (#33) can fill it without pulling the world in (#S1 test contract).
//!
//! BOOT-01 increment A adds the callable startup preflight
//! ([`preflight`]), which reproduces `boot-api.md` §BOOT-01 steps 1–2
//! (config-path resolution incl. `ND_CONFIGFILE`, root-user restriction,
//! boot-value validation, startup-cwd capture, music-root validation) with no
//! side effects. `main.rs` stays empty: the binary entrypoint belongs to
//! BOOT-03/BOOT-04.

pub mod preflight;

/// Scaffold marker: the public surface authored by #33 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 33;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_cli_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 33;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-cli instantiates without pulling in the world"
        );
    }
}
