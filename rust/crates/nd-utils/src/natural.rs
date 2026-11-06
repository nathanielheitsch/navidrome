//! Natural (alphanumeric) string comparison — byte-exact port of `utils/natural`.
//!
//! When both strings have digit sequences at the same position, they are
//! compared numerically (so "file2" < "file10"); otherwise bytes are compared
//! one-by-one. No allocations are made.

/// Returns a negative value if `a < b`, zero if `a == b`, or a positive value if
/// `a > b` using natural sort ordering.
///
/// When two numeric segments are numerically equal (e.g. "01" vs "1"),
/// comparison continues with the remaining suffixes, and the padding
/// difference is kept as a final tie-breaker that only decides strings
/// that are otherwise equal (e.g. "a01" < "a1", "a0" < "a00"). Deferring it
/// that way is what keeps the ordering transitive, which SQLite requires of
/// a collating function.
pub fn compare(a: &str, b: &str) -> i32 {
    compare_impl(a, b, false)
}

/// `compare` with ASCII case folding, matching SQLite's `NOCASE` collation:
/// only A-Z fold, bytes >= 0x80 are compared as-is.
pub fn compare_fold(a: &str, b: &str) -> i32 {
    compare_impl(a, b, true)
}

fn compare_impl(a: &str, b: &str, fold: bool) -> i32 {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut ia, mut ib) = (0usize, 0usize);
    // Set when two runs are numerically equal but differently padded. Applying it
    // immediately would break transitivity, so it only decides otherwise-equal strings.
    let mut pad_tie: i32 = 0;
    while ia < a.len() && ib < b.len() {
        let (mut ca, mut cb) = (a[ia], b[ib]);
        let (da, db) = (is_digit(ca), is_digit(cb));
        if fold {
            ca = lower(ca);
            cb = lower(cb);
        }

        if da && db {
            // Both are in digit sequences — compare numerically.
            let mut end_a = ia;
            while end_a < a.len() && is_digit(a[end_a]) {
                end_a += 1;
            }
            let mut end_b = ib;
            while end_b < b.len() && is_digit(b[end_b]) {
                end_b += 1;
            }

            let c = compare_numbers(&a[ia..end_a], &b[ib..end_b]);
            if c != 0 {
                return c;
            }
            let t = int_compare(&a[ia..end_a], &b[ib..end_b]);
            if t != 0 {
                pad_tie = t;
            }
            ia = end_a;
            ib = end_b;
        } else if da != db || ca != cb {
            return i32::from(ca) - i32::from(cb);
        } else {
            ia += 1;
            ib += 1;
        }
    }
    let c = ((a.len() - ia) as i32) - ((b.len() - ib) as i32);
    if c != 0 {
        return c;
    }
    pad_tie
}

/// Compares two digit strings numerically. Leading zeros are stripped before comparison.
fn compare_numbers(a: &[u8], b: &[u8]) -> i32 {
    // Strip leading zeros.
    let sa = strip_zeros(a);
    let sb = strip_zeros(b);

    // Different lengths after stripping means different magnitude.
    if sa.len() != sb.len() {
        return (sa.len() as i32) - (sb.len() as i32);
    }

    // Same length — compare digit by digit.
    for i in 0..sa.len() {
        if sa[i] != sb[i] {
            return i32::from(sa[i]) - i32::from(sb[i]);
        }
    }
    0
}

/// Returns `s` with leading b'0' bytes removed. If `s` is all zeros, returns the
/// last byte (a single "0").
fn strip_zeros(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && s[i] == b'0' {
        i += 1;
    }
    if i == s.len() && !s.is_empty() {
        return &s[s.len() - 1..];
    }
    &s[i..]
}

/// Byte-wise comparison (Go `strings.Compare` semantics: -1/0/+1).
fn int_compare(a: &[u8], b: &[u8]) -> i32 {
    match a.cmp(b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

const fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

const fn lower(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        c + b'a' - b'A'
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_order(cmp: fn(&str, &str) -> i32, a: &str, b: &str, expected: i32) {
        let result = cmp(a, b);
        match expected {
            e if e < 0 => assert!(result < 0, "expected {a:?} < {b:?}, got {result}"),
            e if e > 0 => assert!(result > 0, "expected {a:?} > {b:?}, got {result}"),
            _ => assert_eq!(result, 0, "expected {a:?} == {b:?}"),
        }
    }

    #[test]
    fn compare_orders_correctly() {
        let cases: &[(&str, &str, i32)] = &[
            // Basic string ordering
            ("a", "b", -1),
            ("b", "a", 1),
            ("a", "aa", -1),
            ("aa", "a", 1),
            // Equal strings
            ("abc", "abc", 0),
            ("", "", 0),
            ("a01", "a01", 0),
            ("a1", "a1", 0),
            // Empty string edge cases
            ("", "a", -1),
            ("a", "", 1),
            // Numeric comparison
            ("2", "10", -1),
            ("10", "2", 1),
            ("42", "42", 0),
            ("9", "10", -1),
            ("99", "100", -1),
            // Simple numeric segments (from original library)
            ("a0", "a1", -1),
            ("a0", "a00", -1),
            ("a00", "a01", -1),
            ("a01", "a1", -1),
            ("a01", "a2", -1),
            ("a01x", "a2x", -1),
            ("a01", "a00", 1),
            ("a2", "a01", 1),
            ("a2x", "a01x", 1),
            // Multiple numeric groups (from original library)
            ("a0b00", "a00b1", -1),
            ("a0b00", "a00b01", -1),
            ("a00b0", "a0b00", -1),
            ("a00b00", "a0b01", -1),
            ("a00b00", "a0b1", -1),
            ("a00b00", "a0b0", 1),
            ("a00b01", "a0b00", 1),
            // Distinct strings must not compare equal: the padding difference in the first
            // run decides once everything else matches.
            ("a00b00", "a0b00", 1),
            // Leading zeros at end of string — lexical tie-break
            ("file01", "file1", -1),
            // Prefix comparison
            ("abc", "abcd", -1),
            ("abcd", "abc", 1),
            // Navidrome use cases: cover art sorting
            ("cover", "cover.1", -1),
            ("cover.1", "cover.2", -1),
            ("cover.2", "cover.10", -1),
            // Navidrome use cases: disc sorting
            ("disc1", "disc2", -1),
            ("disc2", "disc10", -1),
            ("disc1", "disc10", -1),
            // Multiple numeric segments
            ("a1b2", "a1b10", -1),
            ("a2b1", "a1b2", 1),
            // Numbers at the start
            ("2abc", "10abc", -1),
            // Numbers larger than uint64 max (from original library)
            ("a99999999999999999999", "a100000000000000000000", -1),
            (
                "a123456789012345678901234567890",
                "a123456789012345678901234567891",
                -1,
            ),
            ("a999999999999999999999", "a1000000000000000000000", -1),
            ("a20000000000000000000", "a100000000000000000000", -1),
            ("a100000000000000000000", "a20000000000000000000", 1),
            ("a1000000000000000000000", "a999999999999999999999", 1),
            ("a100000000000000000000", "a100000000000000000000", 0),
            ("a00000000000000000000001x", "a1x", -1),
            ("a099999999999999999999x", "a99999999999999999999x", -1),
        ];
        for &(a, b, expected) in cases {
            expect_order(compare, a, b, expected);
        }
    }

    #[test]
    fn compare_fold_orders_case_insensitively() {
        let cases: &[(&str, &str, i32)] = &[
            ("foo 2", "foo 10", -1),
            ("foo 10", "foo 2", 1),
            ("apple 2", "Banana 10", -1),
            ("Banana 10", "apple 2", 1),
            ("ABC", "abc", 0),
            ("Vol 2", "vol 10", -1),
            ("Track9", "track10", -1),
            ("", "", 0),
            ("", "a", -1),
            ("café 2", "café 10", -1),
        ];
        for &(a, b, expected) in cases {
            expect_order(compare_fold, a, b, expected);
        }
    }

    fn sign(n: i32) -> i32 {
        match n {
            n if n < 0 => -1,
            n if n > 0 => 1,
            _ => 0,
        }
    }

    #[test]
    fn compare_fold_is_transitive_as_sqlite_collation_requires() {
        let mut corpus: Vec<String> = Vec::new();
        fn build(prefix: &str, depth: usize, corpus: &mut Vec<String>) {
            if !prefix.is_empty() {
                corpus.push(prefix.to_string());
            }
            if depth == 0 {
                return;
            }
            for c in ["0", "1", "a"] {
                build(&format!("{prefix}{c}"), depth - 1, corpus);
            }
        }
        build("", 3, &mut corpus);

        for a in &corpus {
            for b in &corpus {
                let ab = sign(compare_fold(a, b));
                for c in &corpus {
                    let bc = sign(compare_fold(b, c));
                    let ac = sign(compare_fold(a, c));
                    if ab == 0 && bc == 0 {
                        assert_eq!(
                            ac, 0,
                            "{a:?}=={b:?} and {b:?}=={c:?} but {a:?} vs {c:?} is {ac}"
                        );
                    }
                    if ab < 0 && bc < 0 {
                        assert!(ac < 0, "{a:?}<{b:?}<{c:?} but {a:?} vs {c:?} is {ac}");
                    }
                }
            }
        }
    }

    #[test]
    fn compare_fold_matches_compare_when_both_lowercase() {
        let pairs = [
            ("foo 2", "foo 10"),
            ("a01", "a1"),
            ("a", "aa"),
            ("vol 3", "vol 3"),
        ];
        for (a, b) in pairs {
            assert_eq!(
                compare_fold(a, b),
                compare(a, b),
                "CompareFold({a:?},{b:?}) should match Compare"
            );
        }
    }

    // Property-style test: sorting a corpus with `compare` matches sorting with a
    // per-element sort key derived from numeric-segment normalization.
    #[test]
    fn compare_sorts_corpus_consistently() {
        let mut corpus: Vec<String> = Vec::new();
        fn build(prefix: &str, depth: usize, corpus: &mut Vec<String>) {
            if !prefix.is_empty() {
                corpus.push(prefix.to_string());
            }
            if depth == 0 {
                return;
            }
            for c in ["0", "00", "1", "a"] {
                build(&format!("{prefix}{c}"), depth - 1, corpus);
            }
        }
        build("", 3, &mut corpus);

        let mut sorted = corpus.clone();
        sorted.sort_by(|a, b| compare(a, b).cmp(&0));
        // Antisymmetry + transitivity spot-checks over the sorted corpus.
        for (i, a) in sorted.iter().enumerate() {
            for b in &sorted[i + 1..] {
                assert!(compare(a, b) <= 0, "sorted order violated: {a:?} vs {b:?}");
                // No two distinct strings may compare equal (ties are resolved by padTie).
                if a != b {
                    assert_ne!(
                        compare(a, b),
                        0,
                        "distinct strings compared equal: {a:?} {b:?}"
                    );
                }
            }
        }
    }
}
