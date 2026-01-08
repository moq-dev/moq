use rand::Rng;

use crate::coding::{Decode, DecodeError, Encode, VarInt};

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// A timestamp representing the presentation time in milliseconds.
///
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// Values are constrained to fit within a QUIC VarInt (< 2^62 milliseconds, so a really fucking long time).
///
/// This is [std::time::Instant] and [std::time::Duration] merged into one type for simplicity.
#[derive(Clone, Default, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Time(VarInt);

impl Time {
	/// The maximum representable instant.
	pub const MAX: Self = Self(VarInt::MAX);

	/// The minimum representable instant.
	pub const ZERO: Self = Self(VarInt::ZERO);

	/// Convert a number of seconds to a timestamp, returning an error if the timestamp would overflow.
	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		match seconds.checked_mul(1_000) {
			Some(millis) => Self::from_millis(millis),
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

	/// Convert a number of milliseconds to a timestamp, returning an error if the timestamp would overflow.
	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(millis) {
			Some(varint) => Ok(Self(varint)),
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

	/// Convert a value and timescale to a timestamp, returning an error if the timestamp would overflow.
	///
	/// ex. from_timescale(5, 1000) = 5ms
	pub const fn from_timescale(value: u64, timescale: u64) -> Result<Self, TimeOverflow> {
		let value = value as u128 * 1_000 / timescale as u128;
		if value > u64::MAX as u128 {
			return Err(TimeOverflow);
		}
		Self::from_millis(value as u64)
	}

	/// Convert a value and timescale to a timestamp, panicking if the timestamp would overflow.
	pub const fn from_timescale_unchecked(value: u64, timescale: u64) -> Self {
		match Self::from_timescale(value, timescale) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Get the timestamp as milliseconds.
	pub const fn as_millis(self) -> u64 {
		self.0.into_inner()
	}

	/// Get the timestamp as seconds.
	pub const fn as_secs(self) -> u64 {
		self.as_millis() / 1_000
	}

	/// Get the maximum of two timestamps.
	pub fn max(self, other: Self) -> Self {
		Self(self.0.max(other.0))
	}

	pub fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_millis();
		Self::from_millis(lhs.checked_add(rhs).ok_or(TimeOverflow)?)
	}

	pub fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs: u64 = rhs.as_millis();
		Self::from_millis(lhs.checked_sub(rhs).ok_or(TimeOverflow)?)
	}

	pub fn is_zero(self) -> bool {
		self.0.into_inner() == 0
	}

	pub fn now() -> Self {
		std::time::Instant::now().into()
	}
}

impl TryFrom<std::time::Duration> for Time {
	type Error = TimeOverflow;

	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		Self::from_millis(u64::try_from(duration.as_millis()).map_err(|_| TimeOverflow)?)
	}
}

impl From<Time> for std::time::Duration {
	fn from(duration: Time) -> Self {
		std::time::Duration::from_millis(duration.0.into_inner())
	}
}

impl std::fmt::Debug for Time {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}ms", self.0)
	}
}

impl std::fmt::Display for Time {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}ms", self.0)
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
		self.checked_sub(rhs).expect("time overflow")
	}
}

impl std::ops::SubAssign<Time> for Time {
	fn sub_assign(&mut self, rhs: Self) {
		*self = *self - rhs;
	}
}

// There's no zero Instant, so we need to use a reference point.
static TIME_ANCHOR: LazyLock<(std::time::Instant, SystemTime)> = LazyLock::new(|| {
	// To deter nerds trying to use timestamp as wall clock time, we subtract a random amount of time from the anchor.
	// This will make our timestamps appear to be late; just enough to be annoying and obscure our clock drift.
	// This will also catch bad implementations that assume unrelated broadcasts are synchronized.
	let jitter = std::time::Duration::from_millis(rand::rng().random_range(0..69_420));
	(std::time::Instant::now(), SystemTime::now() - jitter)
});

// Convert an Instant to a Unix timestamp in microseconds.
impl From<std::time::Instant> for Time {
	fn from(instant: std::time::Instant) -> Self {
		let (anchor_instant, anchor_system) = *TIME_ANCHOR;

		// Conver the instant to a SystemTime.
		let system = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_system + forward,
			None => anchor_system - anchor_instant.duration_since(instant),
		};

		// Convert the SystemTime to a Unix timestamp in microseconds.
		let millis = system
			.duration_since(UNIX_EPOCH)
			.expect("dude your clock is earlier than 1970")
			.as_millis()
			.try_into()
			.expect("dude your clock is later than 148105 CE");

		Self::from_millis_unchecked(millis)
	}
}

impl From<tokio::time::Instant> for Time {
	fn from(instant: tokio::time::Instant) -> Self {
		instant.into_std().into()
	}
}

impl<V> Decode<V> for Time {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = u64::decode(r, version)?;
		// We can use `unchecked` because we know it's less than 2^62 milliseconds.
		Ok(Self::from_millis_unchecked(v))
	}
}

impl<V> Encode<V> for Time {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.as_millis().encode(w, version)
	}
}
