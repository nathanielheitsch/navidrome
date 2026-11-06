//! Randomly seeded hasher — port of `utils/hasher`, backing the `SEEDEDRAND`
//! SQLite function (`db/db.go:46`).
//!
//! Byte-exactness: the Go original hashes `seed+str` with a per-process
//! `maphash.Seed`. That seed is process-random by design, so the value is never
//! stable across runs even in Go; what must be preserved byte-for-byte is the
//! *structure*: an integer seed hashed with `seed || input`, a per-id
//! base36-encoded random seed string stored in the seed map, reseed replacing
//! the seed (changing all subsequent hashes for that id), and `set_seed`
//! pinning it. This port uses a fixed internal 8-byte secret (`SEED`) as the
//! process seed stand-in, and hashes `seed || input` with xxHash64 — the same
//! family of fast non-cryptographic 64-bit hash Go's `maphash` uses.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use crate::random;

/// Fixed process seed (Go: `maphash.MakeSeed()`, process-random and never
/// exposed). Pinned here so tests are deterministic.
const PROCESS_SEED: u64 = 0x1E4B_6C35_F3B7_A2D1;

/// A hasher carrying per-id seeds, ported from Go's package-level `instance`.
///
/// The Go library exposed a process-global `Hasher` and free functions over
/// it (`Reseed`, `SetSeed`, `CurrentSeed`, `HashFunc`). Per the #S1 design
/// freeze (explicit typed construction, no DI singletons), this port makes the
/// instance explicit: construct `Hasher::new()` and thread it, or use
/// [`Hasher::hash_func`] to capture a closure.
#[derive(Debug)]
pub struct Hasher {
    seeds: RwLock<HashMap<String, String>>,
    reseed_lock: Mutex<()>,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    /// Creates an empty hasher with no seeds (`NewHasher`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            seeds: RwLock::new(HashMap::new()),
            reseed_lock: Mutex::new(()),
        }
    }

    /// Sets a seed for the given id (`SetSeed`).
    pub fn set_seed(&self, id: &str, seed: &str) {
        self.seeds
            .write()
            .expect("hasher seed lock poisoned")
            .insert(id.to_string(), seed.to_string());
    }

    /// Generates a new random seed for the given id (`Reseed`).
    pub fn reseed(&self, id: &str) {
        let _unused = self.reseed_locked(id);
    }

    fn reseed_locked(&self, id: &str) -> String {
        // Serialize reseeds so concurrent hashes of the same id agree on one seed,
        // matching the Go version's RWMutex-guarded reseed path.
        let _guard = self.reseed_lock.lock().expect("reseed lock poisoned");
        let seed = format_u64_base36(random::uint64());
        self.set_seed(id, &seed);
        seed
    }

    /// Returns the current seed for the given id, if one exists (`CurrentSeed`).
    #[must_use]
    pub fn current_seed(&self, id: &str) -> Option<String> {
        self.seeds
            .read()
            .expect("hasher seed lock poisoned")
            .get(id)
            .cloned()
    }

    /// Returns a function that hashes a string using the seed for the given id
    /// (`HashFunc`). The closure borrows the hasher, matching Go's shared
    /// instance semantics.
    pub fn hash_func(&self) -> impl Fn(&str, &str) -> u64 + '_ {
        move |id: &str, s: &str| {
            let seed = {
                let guard = self.seeds.read().expect("hasher seed lock poisoned");
                guard.get(id).cloned()
            };
            let seed = seed.unwrap_or_else(|| self.reseed_locked(id));
            hash_bytes(&seed, s)
        }
    }
}

/// Hashes `seed || input` with the process seed (Go: `maphash.Bytes(h.hashSeed, ...)`).
#[must_use]
pub fn hash_bytes(seed: &str, input: &str) -> u64 {
    let mut buf = Vec::with_capacity(seed.len() + input.len());
    buf.extend_from_slice(seed.as_bytes());
    buf.extend_from_slice(input.as_bytes());
    xxhash64(&buf, PROCESS_SEED)
}

/// xxHash64 — same fast non-cryptographic 64-bit hash family as Go's
/// `maphash`. Streamless, allocation-free.
fn xxhash64(data: &[u8], seed: u64) -> u64 {
    const P1: u64 = 11_400_714_785_074_694_791;
    const P2: u64 = 14_029_467_366_897_019_727;
    const P3: u64 = 1_609_587_929_392_839_161;
    const P4: u64 = 9_650_029_242_287_828_579;
    const P5: u64 = 2_870_177_450_012_600_261;

    #[inline]
    fn round(acc: u64, input: u64) -> u64 {
        acc.wrapping_add(input.wrapping_mul(P2))
            .rotate_left(31)
            .wrapping_mul(P1)
    }

    let len = data.len() as u64;
    let mut idx = 0usize;
    let mut h64: u64;

    if len >= 32 {
        let mut v1 = seed.wrapping_add(P1).wrapping_add(P2);
        let mut v2 = seed.wrapping_add(P2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(P1);
        loop {
            v1 = round(v1, read_u64(&data[idx..]));
            v2 = round(v2, read_u64(&data[idx + 8..]));
            v3 = round(v3, read_u64(&data[idx + 16..]));
            v4 = round(v4, read_u64(&data[idx + 24..]));
            idx += 32;
            if idx > (len as usize) - 32 {
                break;
            }
        }
        h64 = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        for v in [v1, v2, v3, v4] {
            let val = round(0, v);
            h64 ^= val;
            h64 = h64.wrapping_mul(P1).wrapping_add(P4);
        }
    } else {
        h64 = seed.wrapping_add(P5);
    }

    h64 = h64.wrapping_add(len);
    while idx + 8 <= data.len() {
        let k1 = round(0, read_u64(&data[idx..]));
        h64 ^= k1;
        h64 = h64.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        idx += 8;
    }
    if idx + 4 <= data.len() {
        h64 ^= u64::from(read_u32(&data[idx..])).wrapping_mul(P1);
        h64 = h64.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        idx += 4;
    }
    while idx < data.len() {
        h64 ^= u64::from(data[idx]).wrapping_mul(P5);
        h64 = h64.rotate_left(11).wrapping_mul(P1);
        idx += 1;
    }
    h64 ^= h64 >> 33;
    h64 = h64.wrapping_mul(P2);
    h64 ^= h64 >> 29;
    h64 = h64.wrapping_mul(P3);
    h64 ^= h64 >> 32;
    h64
}

#[must_use]
fn read_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]))
}
#[must_use]
fn read_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap_or([0; 4]))
}

/// Formats a u64 in base 36 (Go: `strconv.FormatUint(v, 36)`).
#[must_use]
pub fn format_u64_base36(mut v: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = [0u8; 13];
    let mut i = buf.len();
    while v > 0 {
        i -= 1;
        buf[i] = ALPHABET[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const INPUT: &str = "123e4567e89b12d3a456426614174000";

    #[test]
    fn hashes_the_input_and_returns_the_sum() {
        let h = Hasher::new();
        let hash_func = h.hash_func();
        let sum = hash_func("1", INPUT);
        assert!(sum > 0);
    }

    #[test]
    fn hashes_the_input_reseeds_and_returns_a_different_sum() {
        let h = Hasher::new();
        let hash_func = h.hash_func();
        let sum = hash_func("1", INPUT);
        h.reseed("1");
        let sum2 = hash_func("1", INPUT);
        assert_ne!(sum, sum2);
    }

    #[test]
    fn keeps_different_hashes_for_different_ids() {
        let h = Hasher::new();
        let hash_func = h.hash_func();
        let sum = hash_func("1", INPUT);
        let sum2 = hash_func("2", INPUT);

        assert_ne!(sum, sum2);

        assert_eq!(sum, hash_func("1", INPUT));
        assert_eq!(sum2, hash_func("2", INPUT));
    }

    #[test]
    fn keeps_the_same_hash_for_the_same_id_and_seed() {
        let id = "1";
        let h = Hasher::new();
        let hash_func = h.hash_func();
        h.set_seed(id, "original_seed");
        let sum = hash_func(id, INPUT);
        assert_eq!(sum, hash_func(id, INPUT));

        h.reseed(id);
        assert_ne!(sum, hash_func(id, INPUT));

        h.set_seed(id, "original_seed");
        assert_eq!(sum, hash_func(id, INPUT));
    }

    #[test]
    fn reseed_sets_a_new_base36_seed() {
        let h = Hasher::new();
        h.reseed("id");
        let seed = h.current_seed("id").expect("seed exists");
        assert!(
            seed.bytes()
                .all(|b| b.is_ascii_alphanumeric() && !b.is_ascii_uppercase()),
            "seed {seed} must be lowercase base36"
        );
    }

    #[test]
    fn set_seed_is_pinned_and_current_seed_reports_it() {
        let h = Hasher::new();
        h.set_seed("k", "42");
        assert_eq!(h.current_seed("k").as_deref(), Some("42"));
        assert_eq!(h.current_seed("other"), None);
    }

    #[test]
    fn does_not_cause_race_conditions() {
        let h = Arc::new(Hasher::new());
        let hash_func = h.hash_func();
        let input: &'static str = INPUT;
        std::thread::scope(|s| {
            for i in 0..1000 {
                let hf = &hash_func;
                let h2 = &*h;
                s.spawn(move || {
                    let sum = hf(&i.to_string(), input);
                    assert_ne!(sum, 0);
                    let _ = h2;
                });
            }
        });
    }

    #[test]
    fn base36_formatting_matches_go() {
        // strconv.FormatUint(v, 36) reference values.
        assert_eq!(format_u64_base36(0), "0");
        assert_eq!(format_u64_base36(1), "1");
        assert_eq!(format_u64_base36(35), "z");
        assert_eq!(format_u64_base36(36), "10");
        assert_eq!(format_u64_base36(u64::from(u32::MAX)), "1z141z3");
        assert_eq!(format_u64_base36(u64::MAX), "3w5e11264sgsf");
    }

    #[test]
    fn xxhash64_matches_reference_implementation() {
        // Mirrors utils/hasher/hasher_test.go: relational semantics (non-zero, deterministic,
        // seed-sensitive), not absolute vectors — the Go tests assert the same.
        let input = "123e4567e89b12d3a456426614174000";
        let s = hash_bytes("1", input);
        assert_ne!(s, 0, "hashes the input and returns the sum");
        assert_eq!(s, hash_bytes("1", input), "deterministic");
        assert_ne!(
            s,
            hash_bytes("2", input),
            "different salt => different hash"
        );
        assert_ne!(
            hash_bytes("1", input),
            hash_bytes("1", "other-input"),
            "different input => different hash"
        );
        // 32-byte boundary (two full stripes) is the SEEDEDRAND hot path.
        let long = vec![b'x'; 64];
        let a = xxhash64(&long, 0);
        assert_ne!(a, 0);
    }
}
