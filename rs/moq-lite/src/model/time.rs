use crate::coding::{Decode, DecodeError, Encode, VarInt};

use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// A timestamp representing the presentation time in microseconds.
///
/// All timestamps within a broadcast are relative.
/// A publisher may convert wall clock time to a timestamp via `Instant::now()::into()`.
/// However, a subscriber MUST NOT assume clock synchronization; there is no way to reverse a timestamp.
///
/// Values are constrained to fit within a QUIC VarInt (< 2^62 microseconds, ~146,000 years).
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Time(VarInt);

impl Time {
	/// The maximum representable timestamp.
	pub const MAX: Self = Self(VarInt::MAX);

	/// The zero timestamp.
	pub const ZERO: Self = Self(VarInt::ZERO);

	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		match seconds.checked_mul(1_000_000) {
			Some(micros) => Self::from_micros(micros),
			None => Err(TimeOverflow),
		}
	}

	/// A helper because const doesn't support Result::unwrap() yet.
	pub const fn from_secs_unchecked(seconds: u64) -> Self {
		match Self::from_secs(seconds) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		match millis.checked_mul(1000) {
			Some(micros) => Self::from_micros(micros),
			None => Err(TimeOverflow),
		}
	}

	/// A helper because const doesn't support Result::unwrap() yet.
	pub const fn from_millis_unchecked(millis: u64) -> Self {
		match Self::from_millis(millis) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	pub const fn from_micros(micros: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(micros) {
			Some(varint) => Ok(Self(varint)),
			None => Err(TimeOverflow),
		}
	}

	pub const fn from_micros_unchecked(micros: u64) -> Self {
		match Self::from_micros(micros) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	pub const fn from_nanos(nanos: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(nanos / 1000) {
			Some(varint) => Ok(Self(varint)),
			None => Err(TimeOverflow),
		}
	}

	pub const fn from_nanos_unchecked(nanos: u64) -> Self {
		match Self::from_nanos(nanos) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	pub const fn from_timescale(value: u64, timescale: u64) -> Result<Self, TimeOverflow> {
		let value = value as u128 * 1_000_000 / timescale as u128;
		if value > u64::MAX as u128 {
			return Err(TimeOverflow);
		}
		Self::from_micros(value as u64)
	}

	pub const fn from_timescale_unchecked(value: u64, timescale: u64) -> Self {
		match Self::from_timescale(value, timescale) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
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

	pub fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_micros();
		Self::from_micros(lhs.checked_add(rhs).ok_or(TimeOverflow)?)
	}

	pub fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_micros();
		Self::from_micros(lhs.checked_sub(rhs).ok_or(TimeOverflow)?)
	}

	pub fn is_zero(self) -> bool {
		self.0.into_inner() == 0
	}
}

impl TryFrom<std::time::Duration> for Time {
	type Error = TimeOverflow;

	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		Self::from_micros(u64::try_from(duration.as_micros()).map_err(|_| TimeOverflow)?)
	}
}

impl From<Time> for std::time::Duration {
	fn from(duration: Time) -> Self {
		std::time::Duration::from_micros(duration.0.into_inner())
	}
}

impl std::fmt::Debug for Time {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}µs", self.0)
	}
}

impl std::fmt::Display for Time {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}µs", self.0)
	}
}

impl std::ops::Add<Time> for Time {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		self.checked_add(rhs).expect("time overflow")
	}
}

impl std::ops::AddAssign<Time> for Time {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}

impl std::ops::Sub for Time {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		self.checked_sub(rhs).expect("timeoverflow")
	}
}

impl std::ops::SubAssign<Time> for Time {
	fn sub_assign(&mut self, rhs: Self) {
		*self = *self - rhs;
	}
}

// There's no zero Instant, so we need to use a reference point.
static TIME_ANCHOR: LazyLock<(Instant, SystemTime)> = LazyLock::new(|| (Instant::now(), SystemTime::now()));

// Convert an Instant to a Unix timestamp in microseconds.
impl From<Instant> for Time {
	fn from(instant: Instant) -> Self {
		let (anchor_instant, anchor_system) = *TIME_ANCHOR;

		// Conver the instant to a SystemTime.
		let system = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_system + forward,
			None => anchor_system - anchor_instant.duration_since(instant),
		};

		// Convert the SystemTime to a Unix timestamp in microseconds.
		let micros = system
			.duration_since(UNIX_EPOCH)
			.expect("dude your clock is earlier than 1970")
			.as_micros() as u64;

		Self::from_micros(micros).expect("dude your clock is later than 148105 CE")
	}
}

impl From<tokio::time::Instant> for Time {
	fn from(instant: tokio::time::Instant) -> Self {
		instant.into_std().into()
	}
}

impl<V> Decode<V> for Time {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		Ok(Self(VarInt::decode(r, version)?))
	}
}

impl<V> Encode<V> for Time {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.encode(w, version);
	}
}
