//! Seedable random number generator for deterministic testing.
//!
//! When a seed is provided via [`Config::rng_seed`], all non-cryptographic
//! randomness will be deterministic. This is useful for testing and debugging.

use rand::distr::{Distribution, StandardUniform};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// A random number generator that can be seeded for deterministic behavior.
///
/// When created with a seed, it produces deterministic random values.
/// When created without a seed, it uses the thread-local random generator.
pub struct SeededRng {
    inner: Option<StdRng>,
}

impl SeededRng {
    /// Create a new RNG with an optional seed.
    ///
    /// If `seed` is `Some`, the RNG will produce deterministic values.
    /// If `seed` is `None`, it will use the thread-local random generator.
    pub fn new(seed: Option<u64>) -> Self {
        let inner = seed.map(StdRng::seed_from_u64);
        Self { inner }
    }

    /// Generate a random value of type T.
    pub fn random<T>(&mut self) -> T
    where
        StandardUniform: Distribution<T>,
    {
        match self.inner.as_mut() {
            Some(rng) => rng.random(),
            None => rand::random(),
        }
    }
}

impl std::fmt::Debug for SeededRng {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let is_seeded = self.inner.is_some();
        f.debug_struct("SeededRng")
            .field("seeded", &is_seeded)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_rng_is_deterministic() {
        let mut rng1 = SeededRng::new(Some(12345));
        let mut rng2 = SeededRng::new(Some(12345));

        let values1: [u8; 10] = [
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
            rng1.random(),
        ];

        let values2: [u8; 10] = [
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
            rng2.random(),
        ];

        assert_eq!(values1, values2, "Same seed should produce same values");
    }

    #[test]
    fn different_seeds_produce_different_values() {
        let mut rng1 = SeededRng::new(Some(12345));
        let mut rng2 = SeededRng::new(Some(54321));

        let value1: u64 = rng1.random();
        let value2: u64 = rng2.random();

        assert_ne!(
            value1, value2,
            "Different seeds should produce different values"
        );
    }
}
