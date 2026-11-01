// nd-scanner: owned by #17 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-scanner: this crate instantiates empty so later issues
//! (#17) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #17 replaces this constant.
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
