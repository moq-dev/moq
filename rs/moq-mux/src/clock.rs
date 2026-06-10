//! A shared clock for aligning concurrently-produced tracks.

use std::time::Instant;

/// A monotonic clock for stamping media frames so that tracks produced
/// concurrently, e.g. an audio and a video capture running on separate
/// threads, land on a single timeline.
///
/// Create one [`Clock`] and sample it ([`micros`](Self::micros)) from each
/// producer: because they share an epoch, frames captured at the same instant
/// get the same timestamp, keeping the tracks in sync. It is `Copy`, so it's
/// cheap to hand to several producers.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
	epoch: Instant,
}

impl Clock {
	/// Start a clock anchored at the current instant.
	pub fn new() -> Self {
		Self { epoch: Instant::now() }
	}

	/// Microseconds elapsed since the clock's epoch.
	pub fn micros(&self) -> u64 {
		// u128 -> u64 truncation is unreachable: u64 microseconds is ~584,000 years.
		self.epoch.elapsed().as_micros() as u64
	}
}

impl Default for Clock {
	fn default() -> Self {
		Self::new()
	}
}
