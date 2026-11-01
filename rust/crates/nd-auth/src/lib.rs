// nd-auth: owned by #13 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-auth: this crate instantiates empty so later issues
//! (#13) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #13 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 13;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_auth_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 13;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-auth instantiates without pulling in the world"
        );
    }
}
