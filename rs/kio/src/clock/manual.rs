use std::time::Duration;

use crate::clock::{Instant, Wall};

/// A clock that moves only when a test moves it.
///
/// For caller-driven code, which takes `now` rather than reading a clock: hand it
/// [`Manual::now`] and step with [`Manual::advance`]. It starts at
/// [`Instant::ZERO`] and [`Manual::WALL_START`].
#[derive(Debug, Clone)]
pub struct Manual {
	now: Instant,
	wall: Wall,
}

impl Manual {
	/// The wall time a new clock, and the simulator, starts at: 2025-01-01T00:00:00Z.
	///
	/// Fixed so tests are reproducible, and after 2020 so code that treats an
	/// earlier wall clock as unset sees a set one.
	pub const WALL_START: Wall = Wall::from_unix_secs(1_735_689_600);

	/// A clock at [`Instant::ZERO`] and [`Self::WALL_START`].
	pub fn new() -> Self {
		Self {
			now: Instant::ZERO,
			wall: Self::WALL_START,
		}
	}

	/// The current instant.
	pub fn now(&self) -> Instant {
		self.now
	}

	/// The current wall time.
	pub fn wall(&self) -> Wall {
		self.wall
	}

	/// Let `duration` pass, moving both the instant and the wall time.
	pub fn advance(&mut self, duration: Duration) {
		self.now += duration;
		self.wall = self.wall + duration;
	}

	/// Set the wall time, forward or back, as a clock correction would. The instant stays put.
	pub fn step_wall(&mut self, wall: Wall) {
		self.wall = wall;
	}
}

impl Default for Manual {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_wall_step_passes_no_time() {
		let mut clock = Manual::new();
		clock.advance(Duration::from_secs(5));
		assert_eq!(clock.now(), Instant::ZERO + Duration::from_secs(5));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(5));

		clock.step_wall(Manual::WALL_START + Duration::from_secs(2));
		assert_eq!(
			clock.now(),
			Instant::ZERO + Duration::from_secs(5),
			"a step back passes no time"
		);
		clock.step_wall(Manual::WALL_START + Duration::from_secs(3600));
		assert_eq!(
			clock.now(),
			Instant::ZERO + Duration::from_secs(5),
			"nor does a step forward"
		);

		clock.advance(Duration::from_secs(1));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(3601));
	}
}
