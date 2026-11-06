//! Cryptographically-secure random string generation — port of `utils/nanoid`.

use rand::RngCore;

/// Returns a cryptographically secure random string of `size` characters
/// drawn from `alphabet`. It uses a bitmask with rejection sampling to avoid
/// modulo bias.
///
/// The alphabet must be non-empty, contain at most 255 bytes, and consist only
/// of ASCII characters. Non-ASCII alphabets (e.g. multi-byte UTF-8) are not
/// supported.
///
/// # Errors
/// - alphabet empty or longer than 255 bytes
/// - `size` not positive
pub fn generate(alphabet: &str, size: usize) -> Result<String, String> {
    let alphabet = alphabet.as_bytes();
    if alphabet.is_empty() || alphabet.len() > 255 {
        return Err("alphabet must be non-empty and at most 255 characters".to_string());
    }
    if size == 0 {
        return Err("size must be a positive integer".to_string());
    }

    let mask = get_mask(alphabet.len());
    let step = (1.6_f64 * f64::from(u32::from(mask)) * size as f64 / alphabet.len() as f64).ceil()
        as usize;

    let mut id = vec![0u8; size];
    let mut bytes = vec![0u8; step];
    let mut j = 0usize;
    let mut rng = rand::rng();
    loop {
        rng.fill_bytes(&mut bytes);
        for &b in &bytes {
            let idx = (b & mask) as usize;
            if idx < alphabet.len() {
                id[j] = alphabet[idx];
                j += 1;
                if j == size {
                    return String::from_utf8(id).map_err(|e| e.to_string());
                }
            }
        }
    }
}

/// Returns the smallest bitmask >= alphabet_size-1.
#[must_use]
pub fn get_mask(alphabet_size: usize) -> u8 {
    for i in 1u32..=8 {
        let mask = (2u32 << i) - 1;
        if mask >= (alphabet_size as u32).saturating_sub(1) {
            return u8::try_from(mask).unwrap_or(255);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    #[test]
    fn generates_a_string_of_the_requested_length() {
        let id = generate(ALPHABET, 22).expect("generate");
        assert_eq!(id.len(), 22);
    }

    #[test]
    fn generates_a_short_string_of_the_requested_length() {
        let id = generate(ALPHABET, 10).expect("generate");
        assert_eq!(id.len(), 10);
    }

    #[test]
    fn only_contains_characters_from_the_alphabet() {
        let id = generate(ALPHABET, 100).expect("generate");
        for c in id.chars() {
            assert!(ALPHABET.contains(c), "char {c} not in alphabet");
        }
    }

    #[test]
    fn generates_unique_ids() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let id = generate(ALPHABET, 22).expect("generate");
            assert!(seen.insert(id), "duplicate id");
        }
    }

    #[test]
    fn works_with_a_single_character_alphabet() {
        let id = generate("a", 5).expect("generate");
        assert_eq!(id, "aaaaa");
    }

    #[test]
    fn works_with_a_small_alphabet() {
        let id = generate("ab", 10).expect("generate");
        assert_eq!(id.len(), 10);
        for c in id.chars() {
            assert!(c == 'a' || c == 'b');
        }
    }

    #[test]
    fn returns_error_on_empty_alphabet() {
        assert!(generate("", 10).is_err());
    }

    #[test]
    fn returns_error_on_alphabet_larger_than_255_characters() {
        let big_alphabet: String = (0..=255u8).map(|b| b as char).collect();
        assert!(generate(&big_alphabet, 10).is_err());
    }

    #[test]
    fn returns_error_on_non_positive_size() {
        assert!(generate(ALPHABET, 0).is_err());
    }
}
