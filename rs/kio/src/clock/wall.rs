use std::{
	fmt,
	ops::{Add, Sub},
	time::Duration,
};

/// A wall-clock time, in nanoseconds since the Unix epoch.
///
/// Read from the same clock as [`Instant`](crate::clock::Instant) but kept apart from it:
/// wall time can be stepped, forward or back, without any monotonic time passing,
/// so it never measures elapsed time. Times before 1970 are unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wall(u64);

impl Wall {
	/// 1970-01-01T00:00:00Z.
	pub const UNIX_EPOCH: Self = Self(0);

	/// `secs` whole seconds after the Unix epoch, for constants.
	pub(crate) const fn from_unix_secs(secs: u64) -> Self {
		Self(secs * 1_000_000_000)
	}

	/// The current wall time on the ambient clock.
	pub fn now() -> Self {
		crate::clock::wall()
	}

	/// The time from `earlier` to this one, or `None` if `earlier` is later.
	pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
		self.0.checked_sub(earlier.0).map(Duration::from_nanos)
	}

	/// The time from `earlier` to this one, zero if `earlier` is later.
	pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
		self.checked_duration_since(earlier).unwrap_or_default()
	}

	/// This time moved forward by `duration`, or `None` past 2554.
	pub fn checked_add(&self, duration: Duration) -> Option<Self> {
		let nanos = u64::try_from(duration.as_nanos()).ok()?;
		self.0.checked_add(nanos).map(Self)
	}

	/// This time moved forward by `duration`, stopping at the last time in 2554.
	pub fn saturating_add(&self, duration: Duration) -> Self {
		let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
		Self(self.0.saturating_add(nanos))
	}

	/// This time moved back by `duration`, or `None` before 1970.
	pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
		let nanos = u64::try_from(duration.as_nanos()).ok()?;
		self.0.checked_sub(nanos).map(Self)
	}
}

impl Add<Duration> for Wall {
	type Output = Self;

	/// Panics past 2554; see [`Wall::checked_add`].
	fn add(self, duration: Duration) -> Self {
		self.checked_add(duration)
			.expect("overflow when adding duration to wall time")
	}
}

impl Sub<Duration> for Wall {
	type Output = Self;

	/// Panics before 1970; see [`Wall::checked_sub`].
	fn sub(self, duration: Duration) -> Self {
		self.checked_sub(duration)
			.expect("overflow when subtracting duration from wall time")
	}
}

impl From<Wall> for std::time::SystemTime {
	fn from(wall: Wall) -> Self {
		Self::UNIX_EPOCH + Duration::from_nanos(wall.0)
	}
}

/// A [`std::time::SystemTime`] before 1970 or past 2554, outside [`Wall`]'s range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange;

impl fmt::Display for OutOfRange {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("system time out of the wall time range")
	}
}

impl std::error::Error for OutOfRange {}

impl TryFrom<std::time::SystemTime> for Wall {
	type Error = OutOfRange;

	fn try_from(time: std::time::SystemTime) -> Result<Self, OutOfRange> {
		let since = time
			.duration_since(std::time::SystemTime::UNIX_EPOCH)
			.map_err(|_| OutOfRange)?;
		Self::UNIX_EPOCH.checked_add(since).ok_or(OutOfRange)
	}
}

impl fmt::Debug for Wall {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Wall({:?} since 1970)", Duration::from_nanos(self.0))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn steps_either_way_without_wrapping() {
		let wall = Wall::UNIX_EPOCH + Duration::from_secs(10);
		let back = wall - Duration::from_secs(4);
		assert_eq!(wall.checked_duration_since(back), Some(Duration::from_secs(4)));
		assert_eq!(back.checked_duration_since(wall), None);
		assert_eq!(back.saturating_duration_since(wall), Duration::ZERO);
		assert_eq!(Wall::UNIX_EPOCH.checked_sub(Duration::from_nanos(1)), None);
		let last = Wall::UNIX_EPOCH.saturating_add(Duration::MAX);
		assert_eq!(last, Wall::UNIX_EPOCH + Duration::from_nanos(u64::MAX));
		assert_eq!(last.saturating_add(Duration::from_nanos(1)), last);
		assert_eq!(
			wall.saturating_add(Duration::from_secs(1)),
			wall + Duration::from_secs(1)
		);
	}

	#[test]
	fn converts_with_system_time() {
		let wall = Wall::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
		let system = std::time::SystemTime::from(wall);
		assert_eq!(Wall::try_from(system), Ok(wall));
		let before = std::time::SystemTime::UNIX_EPOCH - Duration::from_secs(1);
		assert_eq!(Wall::try_from(before), Err(OutOfRange));
	}
}
