use std::{
	fmt,
	ops::{Add, AddAssign, Sub, SubAssign},
	time::Duration,
};

/// A point on the monotonic clock, in nanoseconds from the clock's origin.
///
/// The methods mirror [`std::time::Instant`]'s, so swapping one for the other is an
/// import change. Instants are only comparable within one clock: the simulator
/// starts at [`Instant::ZERO`], and the runtime backends count from an origin they
/// pick once, reading far past zero there so earlier moments stay representable.
/// On native, [`Instant::into_std`] and [`Instant::from_std`] convert at a third-party
/// API that takes std's instants.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
	/// The clock's origin.
	pub const ZERO: Self = Self(0);

	/// The last instant the clock can represent, which a deadline past its range saturates to.
	pub(crate) const MAX: Self = Self(u64::MAX);

	/// The current instant on the ambient clock.
	pub fn now() -> Self {
		crate::clock::now()
	}

	/// The time elapsed since this instant on the ambient clock, zero if it is in the future.
	pub fn elapsed(&self) -> Duration {
		Self::now().saturating_duration_since(*self)
	}

	/// The time from `earlier` to this instant, zero if `earlier` is later.
	pub fn duration_since(&self, earlier: Self) -> Duration {
		self.saturating_duration_since(earlier)
	}

	/// The time from `earlier` to this instant, or `None` if `earlier` is later.
	pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
		self.0.checked_sub(earlier.0).map(Duration::from_nanos)
	}

	/// The time from `earlier` to this instant, zero if `earlier` is later.
	pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
		self.checked_duration_since(earlier).unwrap_or_default()
	}

	/// This instant moved forward by `duration`, or `None` past the clock's range.
	pub fn checked_add(&self, duration: Duration) -> Option<Self> {
		let nanos = u64::try_from(duration.as_nanos()).ok()?;
		self.0.checked_add(nanos).map(Self)
	}

	/// This instant moved back by `duration`, or `None` before the clock's origin.
	pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
		let nanos = u64::try_from(duration.as_nanos()).ok()?;
		self.0.checked_sub(nanos).map(Self)
	}

	/// The instant `since` past the origin. Panics past the clock's range (584 years).
	pub(crate) fn from_origin(since: Duration) -> Self {
		Self::ZERO.checked_add(since).expect("instant past the clock's range")
	}

	/// The time from the origin to this instant.
	pub(crate) fn since_origin(self) -> Duration {
		Duration::from_nanos(self.0)
	}

	/// The instant as nanoseconds from the origin, for lock-free storage.
	pub(crate) const fn to_nanos(self) -> u64 {
		self.0
	}

	/// The instant `nanos` past the origin.
	pub(crate) const fn from_nanos(nanos: u64) -> Self {
		Self(nanos)
	}

	/// This instant on std's clock, for a third-party API that takes one.
	///
	/// Maps through the runtime backend's origin, as its timers do. That is exact on
	/// a running clock. On paused tokio or the simulator it keeps the time between
	/// instants, but not their distance from [`std::time::Instant::now`].
	///
	/// # Panics
	///
	/// When std's clock cannot represent it, as with a simulated instant near zero
	/// on a platform whose std clock starts at boot.
	pub fn into_std(self) -> std::time::Instant {
		crate::clock::runtime::into_std(self).expect("instant outside std's clock range")
	}

	/// The instant `at` names on std's clock, the inverse of [`Instant::into_std`].
	///
	/// Saturates at the clock's range, like a deadline past it.
	pub fn from_std(at: std::time::Instant) -> Self {
		crate::clock::runtime::from_std(at)
	}
}

impl Add<Duration> for Instant {
	type Output = Self;

	/// Panics past the clock's range; see [`Instant::checked_add`].
	fn add(self, duration: Duration) -> Self {
		self.checked_add(duration)
			.expect("overflow when adding duration to instant")
	}
}

impl AddAssign<Duration> for Instant {
	fn add_assign(&mut self, duration: Duration) {
		*self = *self + duration;
	}
}

impl Sub<Duration> for Instant {
	type Output = Self;

	/// Panics before the clock's origin; see [`Instant::checked_sub`].
	fn sub(self, duration: Duration) -> Self {
		self.checked_sub(duration)
			.expect("overflow when subtracting duration from instant")
	}
}

impl SubAssign<Duration> for Instant {
	fn sub_assign(&mut self, duration: Duration) {
		*self = *self - duration;
	}
}

impl Sub for Instant {
	type Output = Duration;

	/// The time from `earlier` to this instant, zero if `earlier` is later.
	fn sub(self, earlier: Self) -> Duration {
		self.duration_since(earlier)
	}
}

impl fmt::Debug for Instant {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Instant({:?})", self.since_origin())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn arithmetic_mirrors_std() {
		let start = Instant::ZERO + Duration::from_secs(2);
		let later = start + Duration::from_millis(1500);
		assert_eq!(later - start, Duration::from_millis(1500));
		assert_eq!(start - later, Duration::ZERO, "an earlier instant saturates");
		assert_eq!(start.checked_duration_since(later), None);
		assert_eq!(later - Duration::from_millis(1500), start);
		assert_eq!(Instant::ZERO.checked_sub(Duration::from_nanos(1)), None);
		assert_eq!(Instant(u64::MAX).checked_add(Duration::from_nanos(1)), None);
		assert_eq!(Instant::ZERO.checked_add(Duration::MAX), None);
	}

	#[test]
	#[should_panic(expected = "overflow")]
	fn add_past_the_range_panics() {
		let _ = Instant::ZERO + Duration::MAX;
	}
}
