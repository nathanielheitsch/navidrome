// nd-model: owned by #06 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-model: this crate instantiates empty so later issues
//! (#06) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #06 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 6;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_model_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 6;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-model instantiates without pulling in the world"
        );
    }
}
pub mod album;
pub mod annotation;
pub mod entity;
pub mod wire;
