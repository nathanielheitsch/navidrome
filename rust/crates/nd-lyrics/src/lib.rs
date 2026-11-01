// nd-lyrics: owned by #09 / #22 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-lyrics: this crate instantiates empty so later issues
//! (#09) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #09 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 9;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_lyrics_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 9;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-lyrics instantiates without pulling in the world"
        );
    }
}
