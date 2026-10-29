//! The entropy behind keys, connection IDs, and seeds that no configuration supplies
//!
//! Tests replace it with a per-thread seeded generator, so one seed reproduces a whole run.

#[cfg(not(test))]
pub(crate) fn rng() -> rand::rngs::ThreadRng {
	rand::rng()
}

/// A generator seeded straight from the OS, not the thread rng, so forked processes diverge
///
/// Panics if the OS random source fails.
#[cfg(all(not(test), any(feature = "aws-lc-rs", feature = "ring")))]
pub(crate) fn std_rng() -> rand::rngs::StdRng {
	use rand::SeedableRng;
	rand::rngs::StdRng::try_from_rng(&mut rand::rngs::SysRng)
		.expect("failed to seed random number generator from system")
}

#[cfg(test)]
pub(crate) use seeded::{rng, std_rng};

// Only the crate's `tests` module reseeds, and it needs a rustls provider.
#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
pub(crate) use seeded::reseed;

#[cfg(test)]
mod seeded {
	use std::{cell::RefCell, convert::Infallible};

	use rand::{Rng, SeedableRng, TryCryptoRng, TryRng, rngs::StdRng};

	thread_local! {
		// Every test thread starts from the same state, so a test that never reseeds still
		// replays identically.
		static RNG: RefCell<StdRng> = RefCell::new(StdRng::seed_from_u64(0));
	}

	/// Restarts this thread's entropy from `seed`
	pub(crate) fn reseed(seed: u64) {
		RNG.with_borrow_mut(|rng| *rng = StdRng::seed_from_u64(seed));
	}

	pub(crate) fn rng() -> Thread {
		Thread
	}

	pub(crate) fn std_rng() -> StdRng {
		StdRng::from_rng(&mut Thread)
	}

	/// A handle to this thread's seeded generator
	pub(crate) struct Thread;

	impl TryRng for Thread {
		type Error = Infallible;

		fn try_next_u32(&mut self) -> Result<u32, Infallible> {
			Ok(RNG.with_borrow_mut(|rng| rng.next_u32()))
		}

		fn try_next_u64(&mut self) -> Result<u64, Infallible> {
			Ok(RNG.with_borrow_mut(|rng| rng.next_u64()))
		}

		fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
			RNG.with_borrow_mut(|rng| rng.fill_bytes(dst));
			Ok(())
		}
	}

	// Stands in for the thread's CSPRNG; only tests see it.
	impl TryCryptoRng for Thread {}
}
