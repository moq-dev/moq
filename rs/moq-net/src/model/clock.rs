//! The model's only reads of the local clock.
//!
//! Everything else takes time from its caller: drivers are polled with an instant,
//! and cache pools collect at the one they are handed. What reads the clock here is
//! the explicit [`Timestamp::now`](crate::Timestamp::now) convenience, the
//! [`anchor`] that maps an instant onto a timestamp, the arrival a group stamps on
//! each frame it caches, and the cache's wall-clock staleness, so a port swaps this
//! module for its platform's clock.

use std::hash::{Hash, Hasher};
#[cfg(any(test, loom))]
use std::sync::LazyLock;
use std::time::Duration;

/// The current instant on the model's clock.
pub(super) fn now() -> crate::time::Instant {
	moq_clock::now()
}

/// The instant a cache judges wall-clock staleness by, before the time its drivers
/// supplied is folded in; see `cache::Pool::now_wall`.
///
/// The ambient runtime's clock where there is one, so a paused test runtime pauses
/// staleness along with the drivers it supplies time to.
#[cfg(not(any(test, loom)))]
pub(super) fn wall() -> crate::time::Instant {
	moq_clock::now()
}

/// Loom permutes handoffs without advancing time, so scheduling cannot age groups.
#[cfg(all(loom, not(test)))]
pub(super) fn wall() -> crate::time::Instant {
	static FROZEN: LazyLock<crate::time::Instant> = LazyLock::new(moq_clock::now);
	*FROZEN
}

/// Unit tests freeze the wall clock, so only time a test supplies moves it: the
/// simulated clock inside `moq_clock::sim`, or a pool stepped by hand outside it.
#[cfg(test)]
pub(super) fn wall() -> crate::time::Instant {
	static FROZEN: LazyLock<crate::time::Instant> = LazyLock::new(moq_clock::now);
	match moq_clock::sim::is_running() {
		true => moq_clock::now(),
		false => *FROZEN,
	}
}

/// Where [`Timestamp`](crate::Timestamp)s count from: an instant, and the
/// timestamp it maps to.
///
/// A timestamp isn't a real clock; it only needs to be non-negative and roughly
/// monotonic with wall time. A random jitter per ambient clock makes it read late,
/// which deters using it as a wall clock and catches implementations that assume
/// unrelated broadcasts are synchronized.
///
/// The jitter is derived from the ambient seed and the clock's origin rather than
/// drawn, so it stays fixed for the clock without a cache that simulated runs, which
/// all share one origin, would leak into each other.
pub(super) fn anchor() -> (crate::time::Instant, Duration) {
	let (instant, wall) = moq_clock::origin();
	let mut hasher = std::hash::DefaultHasher::new();
	(moq_clock::seed(), instant, wall).hash(&mut hasher);
	let jitter = Duration::from_millis(1 + hasher.finish() % 69_419);
	(instant, offset(wall, jitter))
}

/// Wall time since 2020-01-01T00:00:00Z, less the jitter.
///
/// Counting from 50 years after the Unix epoch keeps the value ~1.5e12 ms smaller,
/// trimming a byte or two off the first frame's varint. Saturates to zero on a wall
/// clock set before 2020 (an unsynced clock), since only a non-negative start matters.
fn offset(wall: moq_clock::Wall, jitter: Duration) -> Duration {
	const EPOCH: Duration = Duration::from_secs(1_577_836_800);
	wall.saturating_duration_since(moq_clock::Wall::UNIX_EPOCH)
		.saturating_sub(EPOCH)
		.saturating_sub(jitter)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Outside the simulator the host clock moves on its own, which must not age
	/// cached groups a test never stepped.
	#[test]
	fn unit_tests_freeze_the_cache_clock() {
		let frozen = wall();
		while moq_clock::now() <= frozen {
			std::hint::spin_loop();
		}
		assert_eq!(wall(), frozen);
	}

	#[test]
	fn each_ambient_clock_owns_its_wall_anchor() {
		let first = moq_clock::sim::run(async { anchor() });
		let second = moq_clock::sim::run(async {
			moq_clock::sim::step_wall(moq_clock::Manual::WALL_START + Duration::from_secs(86_400));
			let before = anchor();
			moq_clock::sim::step_wall(moq_clock::Manual::WALL_START - Duration::from_secs(86_400));
			assert_eq!(
				anchor(),
				before,
				"a wall correction does not retime an established anchor"
			);
			before
		});
		assert!(
			second.1 > first.1 + Duration::from_secs(60),
			"the later wall belongs to a new clock"
		);
	}
}
