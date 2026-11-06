//! Cryptographically-secure random primitives — port of `utils/random`.

use rand::{Rng, RngCore};

/// Returns a random `i64` in `[0, max)` — Go's `random.Int64N`.
///
/// The Go original samples via `crypto/rand` + `big.Int`; this uses
/// `rand::rng().random_range`, backed by the OS-seeded RNG.
#[must_use]
pub fn int64_n(max: i64) -> i64 {
    if max <= 0 {
        return 0;
    }
    rand::rng().random_range(0..max)
}

/// Returns a random `u64` — Go's `random.Uint64` (8 bytes from the OS RNG,
/// big-endian).
#[must_use]
pub fn uint64() -> u64 {
    let mut buffer = [0u8; 8];
    rand::rng().fill_bytes(&mut buffer);
    u64::from_be_bytes(buffer)
}

/// Randomly chooses an entry based on weights (higher weight = higher chance).
/// Port of `utils/random/weighted_random_chooser.go`, based on the subtraction
/// method described in
/// <https://eli.thegreenplace.net/2010/01/22/weighted-random-generation-in-python/>.
#[derive(Debug)]
pub struct WeightedChooser<T> {
    entries: Vec<T>,
    weights: Vec<i64>,
    total_weight: i64,
}

impl<T> Default for WeightedChooser<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            weights: Vec::new(),
            total_weight: 0,
        }
    }
}

impl<T> WeightedChooser<T> {
    /// Creates an empty chooser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value with the given weight.
    pub fn add(&mut self, value: T, weight: i64) {
        self.entries.push(value);
        self.weights.push(weight);
        self.total_weight += weight;
    }

    /// Chooses a random entry based on the weights, and removes it from the list.
    ///
    /// # Errors
    /// - zero total weight
    /// - empty list
    pub fn pick(&mut self) -> Result<T, &'static str> {
        if self.total_weight == 0 {
            return Err("cannot choose from zero weight");
        }
        let i = self.weighted_choice()?;
        self.total_weight -= self.weights[i];
        self.weights.remove(i);
        let entry = self.entries.remove(i);
        Ok(entry)
    }

    fn weighted_choice(&self) -> Result<usize, &'static str> {
        if self.entries.is_empty() {
            return Err("cannot choose from empty list");
        }
        let mut rnd = int64_n(self.total_weight);
        for (i, weight) in self.weights.iter().enumerate() {
            rnd -= *weight;
            if rnd < 0 {
                return Ok(i);
            }
        }
        Err("internal error - code should not reach this point")
    }

    /// Removes the entry at index `i`.
    ///
    /// # Errors
    /// `index out of bounds` when `i` is invalid.
    pub fn remove(&mut self, i: usize) -> Result<(), &'static str> {
        if i >= self.entries.len() {
            return Err("index out of bounds");
        }
        self.total_weight -= self.weights[i];
        self.weights.remove(i);
        self.entries.remove(i);
        Ok(())
    }

    /// Number of entries in the list.
    #[must_use]
    pub fn size(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int64_n_returns_a_random_int64_below_max() {
        for _ in 0..10_000 {
            let v = int64_n(100);
            assert!((0..100).contains(&v));
        }
    }

    #[test]
    fn uint64_returns_random_values() {
        let a = uint64();
        let b = uint64();
        // Collision chance is negligible; allow one retry to avoid flakes.
        if a == b {
            assert_ne!(a, uint64(), "uint64 must not be constant");
        }
    }

    #[test]
    fn weighted_chooser_selects_and_removes_a_random_item() {
        let mut w = WeightedChooser::<i64>::new();
        for i in 0..10 {
            w.add(i, i + 1);
        }
        assert_eq!(w.size(), 10);
        let picked = w.pick().expect("pick succeeds");
        assert!(picked < 10);
        assert_eq!(w.size(), 9);
    }

    #[test]
    fn weighted_chooser_removes_items() {
        let mut w = WeightedChooser::<i64>::new();
        for i in 0..10 {
            w.add(i, i + 1);
        }
        assert_eq!(w.size(), 10);
        for _ in 0..10 {
            w.remove(0).expect("remove succeeds");
        }
        assert_eq!(w.size(), 0);
    }

    #[test]
    fn weighted_chooser_rejects_invalid_index() {
        let mut w = WeightedChooser::<i64>::new();
        for i in 0..10 {
            w.add(i, i + 1);
        }
        assert!(w.remove(10).is_err());
        assert_eq!(w.size(), 10);
    }

    #[test]
    fn weighted_chooser_returns_the_sole_item() {
        let mut w = WeightedChooser::<&str>::new();
        w.add("a", 1);
        assert_eq!(w.pick().expect("sole item"), "a");
    }

    #[test]
    fn weighted_chooser_chooses_based_on_weights() {
        let mut w = WeightedChooser::<i64>::new();
        for i in 0..10 {
            w.add(i, i + 1);
        }
        let mut counts = [0usize; 10];
        for _ in 0..200_000 {
            let c = w.weighted_choice().expect("choice");
            counts[c] += 1;
        }
        for i in 0..9 {
            assert!(
                counts[i] < counts[i + 1],
                "weights must skew picks: {counts:?}"
            );
        }
    }
}
