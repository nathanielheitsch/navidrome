// nd-sched: owned by #12 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-sched: this crate instantiates empty so later issues
//! (#12) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #12 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 12;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_sched_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 12;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-sched instantiates without pulling in the world"
        );
    }
}
