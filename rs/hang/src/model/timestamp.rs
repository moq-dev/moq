use std::time::Duration;

use moq_lite::coding::VarInt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("timestamp overflow")]
pub struct TimestampOverflow;

/// A timestamp representing the presentation time of a media frame in microseconds.
///
/// Values are constrained to fit within a QUIC VarInt (< 2^62 microseconds, ~146,000 years).
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(VarInt);

impl Timestamp {
	/// The maximum representable timestamp.
	pub const MAX: Self = Self(VarInt::MAX);

	/// The zero timestamp.
	pub const ZERO: Self = Self(VarInt::ZERO);

	pub const fn from_secs(seconds: u64) -> Result<Self, TimestampOverflow> {
		match seconds.checked_mul(1_000_000) {
			Some(micros) => Self::from_micros(micros),
			None => Err(TimestampOverflow),
		}
	}

	pub const fn from_millis(millis: u64) -> Result<Self, TimestampOverflow> {
		match millis.checked_mul(1000) {
			Some(micros) => Self::from_micros(micros),
			None => Err(TimestampOverflow),
		}
	}

	pub const fn from_nanos(nanos: u64) -> Result<Self, TimestampOverflow> {
		Self::from_micros(nanos / 1000)
	}

	pub const fn from_micros(micros: u64) -> Result<Self, TimestampOverflow> {
		// ? isn't allowed in const yet
		match VarInt::from_u64(micros) {
			Some(varint) => Ok(Self(varint)),
			None => Err(TimestampOverflow),
		}
	}

	/// Get the timestamp as microseconds.
	pub const fn as_micros(self) -> u64 {
		self.0.into_inner()
	}

	/// Get the timestamp as milliseconds.
	pub const fn as_millis(self) -> u64 {
		self.as_micros() / 1000
	}

	/// Get the timestamp as nanoseconds.
	pub const fn as_nanos(self) -> u128 {
		self.as_micros() as u128 * 1000
	}

	/// Get the timestamp as seconds.
	pub const fn as_secs(self) -> u64 {
		self.as_micros() / 1_000_000
	}

	/// Get the maximum of two timestamps.
	pub fn max(self, other: Self) -> Self {
		Self(self.0.max(other.0))
	}

	pub fn checked_add(self, rhs: Self) -> Option<Self> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_micros();
		Self::from_micros(lhs.checked_add(rhs)?).ok()
	}

	pub fn checked_sub(self, rhs: Self) -> Option<Self> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_micros();
		Self::from_micros(lhs.checked_sub(rhs)?).ok()
	}
}

impl TryFrom<Duration> for Timestamp {
	type Error = TimestampOverflow;

	fn try_from(duration: Duration) -> Result<Self, Self::Error> {
		Self::from_micros(duration.as_micros().try_into().map_err(|_| TimestampOverflow)?)
	}
}

impl From<Timestamp> for Duration {
	fn from(timestamp: Timestamp) -> Self {
		Duration::from_micros(timestamp.0.into_inner())
	}
}

impl std::fmt::Debug for Timestamp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}µs", self.0)
	}
}

impl std::fmt::Display for Timestamp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}µs", self.0)
	}
}

impl std::ops::Add<Timestamp> for Timestamp {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		self.checked_add(rhs).expect("timestamp overflow")
	}
}

impl std::ops::AddAssign<Timestamp> for Timestamp {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}

impl std::ops::Sub for Timestamp {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		self.checked_sub(rhs).expect("timestamp overflow")
	}
}

impl std::ops::SubAssign<Timestamp> for Timestamp {
	fn sub_assign(&mut self, rhs: Self) {
		*self = *self - rhs;
	}
}
