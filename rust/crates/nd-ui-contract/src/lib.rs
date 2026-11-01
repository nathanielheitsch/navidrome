// nd-ui-contract: owned by #28 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-ui-contract: this crate instantiates empty so later issues
//! (#28) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #28 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 28;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_ui_contract_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 28;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-ui-contract instantiates without pulling in the world"
        );
    }
}
