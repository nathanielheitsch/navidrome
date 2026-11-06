// nd-players: owned by #26 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-players: this crate instantiates empty so later issues
//! (#26) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #26 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 26;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_players_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 26;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-players instantiates without pulling in the world"
        );
    }
}
