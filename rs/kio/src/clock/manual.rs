use std::time::Duration;

use crate::clock::{Instant, Wall};

/// A clock that moves only when a test moves it.
///
/// For caller-driven code, which takes `now` rather than reading a clock: hand it
/// [`Manual::now`] and step with [`Manual::advance`]. It starts at
/// [`Instant::ZERO`] and [`Manual::WALL_START`], with the wall clock running at the
/// instant's rate until [`Manual::set_drift`] skews it.
#[derive(Debug, Clone)]
pub struct Manual {
	now: Instant,
	/// The wall time at `since`, from which it runs at `drift`.
	wall: Wall,
	since: Instant,
	/// Parts per million the wall clock gains on the instant, losing when negative.
	drift: i32,
	/// The time spent suspended, which the instant does not count.
	suspended: Duration,
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
			since: Instant::ZERO,
			drift: 0,
			suspended: Duration::ZERO,
		}
	}

	/// The current instant.
	pub fn now(&self) -> Instant {
		self.now
	}

	/// The current wall time.
	pub fn wall(&self) -> Wall {
		let elapsed = self.now.saturating_duration_since(self.since).as_nanos() as i128;
		let skew = elapsed * self.drift as i128 / 1_000_000;
		let nanos = u64::try_from(elapsed + skew).unwrap_or(0);
		self.wall.saturating_add(Duration::from_nanos(nanos))
	}

	/// How long the clock has spent [suspended](Self::suspend), which the instant does not count.
	pub fn suspended(&self) -> Duration {
		self.suspended
	}

	/// Let `duration` pass, moving the instant and the wall time.
	pub fn advance(&mut self, duration: Duration) {
		self.now = self.now.saturating_add(duration);
	}

	/// Set the wall time, forward or back, as a clock correction would. The instant stays put.
	pub fn step_wall(&mut self, wall: Wall) {
		self.wall = wall;
		self.since = self.now;
	}

	/// Run the wall clock `ppm` parts per million fast of the instant from now on, slow when
	/// negative, as a host's oscillator runs off the UTC its wall clock follows.
	pub fn set_drift(&mut self, ppm: i32) {
		self.step_wall(self.wall());
		self.drift = ppm;
	}

	/// Sleep the host for `duration`: the wall time moves and the instant does not, as a
	/// monotonic clock stops while the host is suspended.
	pub fn suspend(&mut self, duration: Duration) {
		self.step_wall(self.wall().saturating_add(duration));
		self.suspended = self.suspended.saturating_add(duration);
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
		assert_eq!(clock.now(), Instant::ZERO.saturating_add(Duration::from_secs(5)));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(5));

		clock.step_wall(Manual::WALL_START + Duration::from_secs(2));
		assert_eq!(
			clock.now(),
			Instant::ZERO.saturating_add(Duration::from_secs(5)),
			"a step back passes no time"
		);
		clock.step_wall(Manual::WALL_START + Duration::from_secs(3600));
		assert_eq!(
			clock.now(),
			Instant::ZERO.saturating_add(Duration::from_secs(5)),
			"nor does a step forward"
		);

		clock.advance(Duration::from_secs(1));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(3601));
	}

	/// A drifting wall clock gains or loses its rate exactly, from where it was set.
	#[test]
	fn a_drifting_wall_runs_off_the_instant() {
		let mut clock = Manual::new();
		clock.advance(Duration::from_secs(10));
		clock.set_drift(500);
		clock.advance(Duration::from_secs(1000));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_millis(1_010_500));

		clock.set_drift(-250);
		clock.advance(Duration::from_secs(1000));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_millis(2_010_250));
		assert_eq!(clock.now(), Instant::ZERO.saturating_add(Duration::from_secs(2010)));
	}

	/// A suspend moves the wall time and counts as suspended, while the instant stands still.
	#[test]
	fn a_suspend_stops_the_instant() {
		let mut clock = Manual::new();
		clock.advance(Duration::from_secs(1));
		clock.suspend(Duration::from_secs(3600));
		assert_eq!(clock.now(), Instant::ZERO.saturating_add(Duration::from_secs(1)));
		assert_eq!(clock.wall(), Manual::WALL_START + Duration::from_secs(3601));
		assert_eq!(clock.suspended(), Duration::from_secs(3600));
	}
}
