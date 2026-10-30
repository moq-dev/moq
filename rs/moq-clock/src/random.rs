use std::{convert::Infallible, sync::LazyLock};

/// The ambient random number generator: the simulator's seeded one inside `sim::run`,
/// the OS-seeded thread generator otherwise.
///
/// Meant for behavior such as jitter and identifiers, not secrets, since a simulated
/// run's draws follow from its seed.
pub fn rng() -> Rng {
	#[cfg(feature = "sim")]
	if let Some(rng) = crate::sim::Rng::new() {
		return Rng(Source::Sim(rng));
	}
	Rng(Source::Thread(rand::rng()))
}

/// The ambient seed: the simulator's inside `sim::run`, a random value fixed for the
/// process otherwise.
///
/// Derive a value from it that must stay fixed for as long as the ambient clock, which
/// a draw from [`rng`] cannot.
pub fn seed() -> u64 {
	#[cfg(feature = "sim")]
	if let Some(seed) = crate::sim::seed() {
		return seed;
	}
	static SEED: LazyLock<u64> = LazyLock::new(rand::random);
	*SEED
}

/// The ambient random number generator, from [`rng`].
pub struct Rng(Source);

enum Source {
	Thread(rand::rngs::ThreadRng),
	#[cfg(feature = "sim")]
	Sim(crate::sim::Rng),
}

impl rand::TryRng for Rng {
	type Error = Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Infallible> {
		match &mut self.0 {
			Source::Thread(rng) => rng.try_next_u32(),
			#[cfg(feature = "sim")]
			Source::Sim(rng) => rng.with(|rng| rng.try_next_u32()),
		}
	}

	fn try_next_u64(&mut self) -> Result<u64, Infallible> {
		match &mut self.0 {
			Source::Thread(rng) => rng.try_next_u64(),
			#[cfg(feature = "sim")]
			Source::Sim(rng) => rng.with(|rng| rng.try_next_u64()),
		}
	}

	fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
		match &mut self.0 {
			Source::Thread(rng) => rng.try_fill_bytes(dst),
			#[cfg(feature = "sim")]
			Source::Sim(rng) => rng.with(|rng| rng.try_fill_bytes(dst)),
		}
	}
}
