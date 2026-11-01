// nd-ffmpeg: owned by #07 (docs/rust-port/issues.md Part B)

//! Scaffold stage for nd-ffmpeg: this crate instantiates empty so later issues
//! (#07) can fill it without pulling the world in (#S1 test contract).

/// Scaffold marker: the public surface authored by #07 replaces this constant.
pub const OWNED_BY_ISSUE: u8 = 7;

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    #[test]
    fn nd_ffmpeg_instantiates() {
        // Arrange: the owning-issue number this crate must expose.
        let expected: u8 = 7;
        // Act: read the crate's own scaffold marker.
        let actual: u8 = OWNED_BY_ISSUE;
        // Assert
        assert_eq!(
            actual, expected,
            "nd-ffmpeg instantiates without pulling in the world"
        );
    }
}
