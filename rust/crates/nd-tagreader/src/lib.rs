// nd-tagreader: owned by #08 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-tagreader: this crate instantiates empty so later issues
//! (#08) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #08 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 8;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_tagreader_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 8;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-tagreader instantiates without pulling in the world"
        );
    }
}
