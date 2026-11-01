//! Gravatar avatar URLs — port of `utils/gravatar`.

use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://www.gravatar.com/avatar";
const DEFAULT_SIZE: u32 = 80;
const MAX_SIZE: u32 = 2048;

/// Returns the gravatar avatar URL for `email` at `size`
/// (`utils/gravatar.Url`).
#[must_use]
pub fn url(email: &str, size: u32) -> String {
    let email = email.trim().to_lowercase();
    let hash = Sha256::digest(email.as_bytes());
    let size = if size < 1 { DEFAULT_SIZE } else { size };
    let size = size.min(MAX_SIZE);
    format!("{BASE_URL}/{}?s={size}", hex::encode(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_well_formatted_gravatar_url() {
        assert_eq!(
            url("my@email.com", 100),
            "https://www.gravatar.com/avatar/cf3d8259741b19a2b09e17d4fa9a97c63adc44bf2a5fa075cdcb5491f525feaa?s=100"
        );
    }

    #[test]
    fn sets_the_default_size() {
        assert_eq!(
            url("my@email.com", 0),
            "https://www.gravatar.com/avatar/cf3d8259741b19a2b09e17d4fa9a97c63adc44bf2a5fa075cdcb5491f525feaa?s=80"
        );
    }

    #[test]
    fn caps_maximum_size() {
        assert_eq!(
            url("my@email.com", 3000),
            "https://www.gravatar.com/avatar/cf3d8259741b19a2b09e17d4fa9a97c63adc44bf2a5fa075cdcb5491f525feaa?s=2048"
        );
    }

    #[test]
    fn ignores_case() {
        assert_eq!(url("MY@email.com", 0), url("my@email.com", 0));
    }

    #[test]
    fn ignores_spaces() {
        assert_eq!(url("  my@email.com  ", 0), url("my@email.com", 0));
    }
}
