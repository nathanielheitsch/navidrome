// nd-utils: owned by #01 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-utils: this crate instantiates empty so later issues
//! (#01) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #01 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 1;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_utils_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 1;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-utils instantiates without pulling in the world"
        );
    }
}
