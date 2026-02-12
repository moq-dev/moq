use rand::Rng;

use crate::Error;
use crate::coding::{Decode, DecodeError, Encode, VarInt};

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// A timestamp representing the presentation time in milliseconds.
///
/// The underlying implementation supports any scale, but everything uses milliseconds by default.
pub type Time = Timescale<1_000>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// A timestamp representing the presentation time in a given scale. ex. 1000 for milliseconds.
///
/// All timestamps within a track are relative, so zero for one track is not zero for another.
/// Values are constrained to fit within a QUIC VarInt (2^62) so they can be encoded and decoded easily.
///
/// This is [std::time::Instant] and [std::time::Duration] merged into one type for simplicity.
#[derive(Clone, Default, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timescale<const SCALE: u64>(VarInt);

impl<const SCALE: u64> Timescale<SCALE> {
	/// The maximum representable instant.
	pub const MAX: Self = Self(VarInt::MAX);

	/// The minimum representable instant.
	pub const ZERO: Self = Self(VarInt::ZERO);

	pub const fn new(value: u32) -> Self {
		Self(VarInt::from_u32(value))
	}

	pub const fn new_u64(value: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(value) {
			Some(varint) => Ok(Self(varint)),
			None => Err(TimeOverflow),
		}
	}

	/// Convert a number of seconds to a timestamp, returning an error if the timestamp would overflow.
	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		// Not using from_scale because it'll be slightly faster
		match seconds.checked_mul(SCALE) {
			Some(value) => Self::new_u64(value),
			None => Err(TimeOverflow),
		}
	}

	pub const fn from_secs_unchecked(seconds: u64) -> Self {
		match Self::from_secs(seconds) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of milliseconds to a timestamp, returning an error if the timestamp would overflow.
	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		Self::from_scale(millis, 1000)
	}

	pub const fn from_millis_unchecked(millis: u64) -> Self {
		Self::from_scale_unchecked(millis, 1000)
	}

	pub const fn from_micros(micros: u64) -> Result<Self, TimeOverflow> {
		Self::from_scale(micros, 1_000_000)
	}

	pub const fn from_micros_unchecked(micros: u64) -> Self {
		Self::from_scale_unchecked(micros, 1_000_000)
	}

	pub const fn from_nanos(nanos: u64) -> Result<Self, TimeOverflow> {
		Self::from_scale(nanos, 1_000_000_000)
	}

	pub const fn from_nanos_unchecked(nanos: u64) -> Self {
		Self::from_scale_unchecked(nanos, 1_000_000_000)
	}

	pub const fn from_scale(value: u64, scale: u64) -> Result<Self, TimeOverflow> {
		match VarInt::from_u128(value as u128 * SCALE as u128 / scale as u128) {
			Some(varint) => Ok(Self(varint)),
			None => Err(TimeOverflow),
		}
	}

	pub const fn from_scale_u128(value: u128, scale: u64) -> Result<Self, TimeOverflow> {
		match value.checked_mul(SCALE as u128) {
			Some(value) => match VarInt::from_u128(value / scale as u128) {
				Some(varint) => Ok(Self(varint)),
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	pub const fn from_scale_unchecked(value: u64, scale: u64) -> Self {
		match Self::from_scale(value, scale) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Get the timestamp as seconds.
	pub const fn as_secs(self) -> u64 {
		self.0.into_inner() / SCALE
	}

	/// Get the timestamp as milliseconds.
	//
	// This returns a u128 to avoid a possible overflow when SCALE < 250
	pub const fn as_millis(self) -> u128 {
		self.as_scale(1000)
	}

	/// Get the timestamp as microseconds.
	pub const fn as_micros(self) -> u128 {
		self.as_scale(1_000_000)
	}

	/// Get the timestamp as nanoseconds.
	pub const fn as_nanos(self) -> u128 {
		self.as_scale(1_000_000_000)
	}

	pub const fn as_scale(self, scale: u64) -> u128 {
		self.0.into_inner() as u128 * scale as u128 / SCALE as u128
	}

	/// Get the maximum of two timestamps.
	pub const fn max(self, other: Self) -> Self {
		if self.0.into_inner() > other.0.into_inner() {
			self
		} else {
			other
		}
	}

	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs = rhs.0.into_inner();
		match lhs.checked_add(rhs) {
			Some(result) => Self::new_u64(result),
			None => Err(TimeOverflow),
		}
	}

	pub const fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		let lhs = self.0.into_inner();
		let rhs = rhs.0.into_inner();
		match lhs.checked_sub(rhs) {
			Some(result) => Self::new_u64(result),
			None => Err(TimeOverflow),
		}
	}

	pub const fn is_zero(self) -> bool {
		self.0.into_inner() == 0
	}

	pub fn now() -> Self {
		// We use tokio so it can be stubbed for testing.
		tokio::time::Instant::now().into()
	}

	/// Convert this timestamp to a different scale.
	///
	/// This allows converting between different TimeScale types, for example from milliseconds to microseconds.
	/// Note that converting to a coarser scale may lose precision due to integer division.
	pub const fn convert<const NEW_SCALE: u64>(self) -> Result<Timescale<NEW_SCALE>, TimeOverflow> {
		let value = self.0.into_inner();
		// Convert from SCALE to NEW_SCALE: value * NEW_SCALE / SCALE
		match (value as u128).checked_mul(NEW_SCALE as u128) {
			Some(v) => match v.checked_div(SCALE as u128) {
				Some(v) => match VarInt::from_u128(v) {
					Some(varint) => Ok(Timescale(varint)),
					None => Err(TimeOverflow),
				},
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	pub fn encode<W: bytes::BufMut>(&self, w: &mut W) {
		self.0.encode(w, ());
	}

	pub fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, Error> {
		let v = VarInt::decode(r, ())?;
		Ok(Self(v))
	}
}

impl<const SCALE: u64> TryFrom<std::time::Duration> for Timescale<SCALE> {
	type Error = TimeOverflow;

	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		Self::from_scale_u128(duration.as_nanos(), 1_000_000_000)
	}
}

impl<const SCALE: u64> From<Timescale<SCALE>> for std::time::Duration {
	fn from(time: Timescale<SCALE>) -> Self {
		std::time::Duration::new(time.as_secs(), (time.as_nanos() % 1_000_000_000) as u32)
	}
}

impl<const SCALE: u64> std::fmt::Debug for Timescale<SCALE> {
	#[allow(clippy::manual_is_multiple_of)] // is_multiple_of is unstable in Rust 1.85
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let nanos = self.as_nanos();

		// Choose the largest unit where we don't need decimal places
		// Check from largest to smallest unit
		if nanos % 1_000_000_000 == 0 {
			write!(f, "{}s", nanos / 1_000_000_000)
		} else if nanos % 1_000_000 == 0 {
			write!(f, "{}ms", nanos / 1_000_000)
		} else if nanos % 1_000 == 0 {
			write!(f, "{}µs", nanos / 1_000)
		} else {
			write!(f, "{}ns", nanos)
		}
	}
}

impl<const SCALE: u64> std::ops::Add for Timescale<SCALE> {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		self.checked_add(rhs).expect("time overflow")
	}
}

impl<const SCALE: u64> std::ops::AddAssign for Timescale<SCALE> {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}

impl<const SCALE: u64> std::ops::Sub for Timescale<SCALE> {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		self.checked_sub(rhs).expect("time overflow")
	}
}

impl<const SCALE: u64> std::ops::SubAssign for Timescale<SCALE> {
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

// Convert an Instant to a Unix timestamp
impl<const SCALE: u64> From<std::time::Instant> for Timescale<SCALE> {
	fn from(instant: std::time::Instant) -> Self {
		let (anchor_instant, anchor_system) = *TIME_ANCHOR;

		// Conver the instant to a SystemTime.
		let system = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_system + forward,
			None => anchor_system - anchor_instant.duration_since(instant),
		};

		// Convert the SystemTime to a Unix timestamp in nanoseconds.
		// We'll then convert that to the desired scale.
		system
			.duration_since(UNIX_EPOCH)
			.expect("dude your clock is earlier than 1970")
			.try_into()
			.expect("dude your clock is later than 2116")
	}
}

impl<const SCALE: u64> From<tokio::time::Instant> for Timescale<SCALE> {
	fn from(instant: tokio::time::Instant) -> Self {
		instant.into_std().into()
	}
}

impl<const SCALE: u64, V> Decode<V> for Timescale<SCALE> {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = VarInt::decode(r, version)?;
		Ok(Self(v))
	}
}

impl<const SCALE: u64, V> Encode<V> for Timescale<SCALE> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.encode(w, version)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_from_secs() {
		let time = Time::from_secs(5).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_micros(), 5_000_000);
		assert_eq!(time.as_nanos(), 5_000_000_000);
	}

	#[test]
	fn test_from_millis() {
		let time = Time::from_millis(5000).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
	}

	#[test]
	fn test_from_micros() {
		let time = Time::from_micros(5_000_000).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_micros(), 5_000_000);
	}

	#[test]
	fn test_from_nanos() {
		let time = Time::from_nanos(5_000_000_000).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_micros(), 5_000_000);
		assert_eq!(time.as_nanos(), 5_000_000_000);
	}

	#[test]
	fn test_zero() {
		let time = Time::ZERO;
		assert_eq!(time.as_secs(), 0);
		assert_eq!(time.as_millis(), 0);
		assert_eq!(time.as_micros(), 0);
		assert_eq!(time.as_nanos(), 0);
		assert!(time.is_zero());
	}

	#[test]
	fn test_roundtrip_millis() {
		let values = [0, 1, 100, 1000, 999999, 1_000_000_000];
		for &val in &values {
			let time = Time::from_millis(val).unwrap();
			assert_eq!(time.as_millis(), val as u128);
		}
	}

	#[test]
	fn test_roundtrip_micros() {
		// Note: values < 1000 will lose precision when converting to milliseconds (SCALE=1000)
		let values = [0, 1000, 1_000_000, 1_000_000_000];
		for &val in &values {
			let time = Time::from_micros(val).unwrap();
			assert_eq!(time.as_micros(), val as u128);
		}
	}

	#[test]
	fn test_different_scale_seconds() {
		type TimeInSeconds = Timescale<1>;
		let time = TimeInSeconds::from_secs(5).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
	}

	#[test]
	fn test_different_scale_microseconds() {
		type TimeInMicros = Timescale<1_000_000>;
		let time = TimeInMicros::from_micros(5_000_000).unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_micros(), 5_000_000);
	}

	#[test]
	fn test_scale_conversion() {
		// Converting 5000 milliseconds at scale 1000 to scale 1000 (should be identity)
		let time = Time::from_scale(5000, 1000).unwrap();
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_secs(), 5);

		// Converting 5 seconds at scale 1 to scale 1000
		let time = Time::from_scale(5, 1).unwrap();
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_secs(), 5);
	}

	#[test]
	fn test_add() {
		let a = Time::from_secs(3).unwrap();
		let b = Time::from_secs(2).unwrap();
		let c = a + b;
		assert_eq!(c.as_secs(), 5);
		assert_eq!(c.as_millis(), 5000);
	}

	#[test]
	fn test_sub() {
		let a = Time::from_secs(5).unwrap();
		let b = Time::from_secs(2).unwrap();
		let c = a - b;
		assert_eq!(c.as_secs(), 3);
		assert_eq!(c.as_millis(), 3000);
	}

	#[test]
	fn test_checked_add() {
		let a = Time::from_millis(1000).unwrap();
		let b = Time::from_millis(2000).unwrap();
		let c = a.checked_add(b).unwrap();
		assert_eq!(c.as_millis(), 3000);
	}

	#[test]
	fn test_checked_sub() {
		let a = Time::from_millis(5000).unwrap();
		let b = Time::from_millis(2000).unwrap();
		let c = a.checked_sub(b).unwrap();
		assert_eq!(c.as_millis(), 3000);
	}

	#[test]
	fn test_checked_sub_underflow() {
		let a = Time::from_millis(1000).unwrap();
		let b = Time::from_millis(2000).unwrap();
		assert!(a.checked_sub(b).is_err());
	}

	#[test]
	fn test_max() {
		let a = Time::from_secs(5).unwrap();
		let b = Time::from_secs(10).unwrap();
		assert_eq!(a.max(b), b);
		assert_eq!(b.max(a), b);
	}

	#[test]
	fn test_duration_conversion() {
		let duration = std::time::Duration::from_secs(5);
		let time: Time = duration.try_into().unwrap();
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);

		let duration_back: std::time::Duration = time.into();
		assert_eq!(duration_back.as_secs(), 5);
	}

	#[test]
	fn test_duration_with_nanos() {
		let duration = std::time::Duration::new(5, 500_000_000); // 5.5 seconds
		let time: Time = duration.try_into().unwrap();
		assert_eq!(time.as_millis(), 5500);

		let duration_back: std::time::Duration = time.into();
		assert_eq!(duration_back.as_millis(), 5500);
	}

	#[test]
	fn test_fractional_conversion() {
		// Test that 1500 millis = 1.5 seconds
		let time = Time::from_millis(1500).unwrap();
		assert_eq!(time.as_secs(), 1); // Integer division
		assert_eq!(time.as_millis(), 1500);
		assert_eq!(time.as_micros(), 1_500_000);
	}

	#[test]
	fn test_precision_loss() {
		// When converting from a finer scale to coarser, we lose precision
		// 1234 micros = 1.234 millis, which rounds down to 1 millisecond internally
		// When converting back, we get 1000 micros, not the original 1234
		let time = Time::from_micros(1234).unwrap();
		assert_eq!(time.as_millis(), 1); // 1234 micros = 1.234 millis, rounds to 1
		assert_eq!(time.as_micros(), 1000); // Precision lost: 1 milli = 1000 micros
	}

	#[test]
	fn test_scale_boundaries() {
		// Test values near scale boundaries
		let time = Time::from_millis(999).unwrap();
		assert_eq!(time.as_secs(), 0);
		assert_eq!(time.as_millis(), 999);

		let time = Time::from_millis(1000).unwrap();
		assert_eq!(time.as_secs(), 1);
		assert_eq!(time.as_millis(), 1000);

		let time = Time::from_millis(1001).unwrap();
		assert_eq!(time.as_secs(), 1);
		assert_eq!(time.as_millis(), 1001);
	}

	#[test]
	fn test_large_values() {
		// Test with large but valid values
		let large_secs = 1_000_000_000u64; // ~31 years
		let time = Time::from_secs(large_secs).unwrap();
		assert_eq!(time.as_secs(), large_secs);
	}

	#[test]
	fn test_new() {
		let time = Time::new(5000); // 5000 in the current scale (millis)
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_secs(), 5);
	}

	#[test]
	fn test_new_u64() {
		let time = Time::new_u64(5000).unwrap();
		assert_eq!(time.as_millis(), 5000);
	}

	#[test]
	fn test_ordering() {
		let a = Time::from_secs(1).unwrap();
		let b = Time::from_secs(2).unwrap();
		assert!(a < b);
		assert!(b > a);
		assert_eq!(a, a);
	}

	#[test]
	fn test_unchecked_variants() {
		let time = Time::from_secs_unchecked(5);
		assert_eq!(time.as_secs(), 5);

		let time = Time::from_millis_unchecked(5000);
		assert_eq!(time.as_millis(), 5000);

		let time = Time::from_micros_unchecked(5_000_000);
		assert_eq!(time.as_micros(), 5_000_000);

		let time = Time::from_nanos_unchecked(5_000_000_000);
		assert_eq!(time.as_nanos(), 5_000_000_000);

		let time = Time::from_scale_unchecked(5000, 1000);
		assert_eq!(time.as_millis(), 5000);
	}

	#[test]
	fn test_as_scale() {
		let time = Time::from_secs(1).unwrap();
		// 1 second in scale 1000 = 1000
		assert_eq!(time.as_scale(1000), 1000);
		// 1 second in scale 1 = 1
		assert_eq!(time.as_scale(1), 1);
		// 1 second in scale 1_000_000 = 1_000_000
		assert_eq!(time.as_scale(1_000_000), 1_000_000);
	}

	#[test]
	fn test_convert_to_finer() {
		// Convert from milliseconds to microseconds (coarser to finer)
		type TimeInMillis = Timescale<1_000>;
		type TimeInMicros = Timescale<1_000_000>;

		let time_millis = TimeInMillis::from_millis(5000).unwrap();
		let time_micros: TimeInMicros = time_millis.convert().unwrap();

		assert_eq!(time_micros.as_millis(), 5000);
		assert_eq!(time_micros.as_micros(), 5_000_000);
	}

	#[test]
	fn test_convert_to_coarser() {
		// Convert from milliseconds to seconds (finer to coarser)
		type TimeInMillis = Timescale<1_000>;
		type TimeInSeconds = Timescale<1>;

		let time_millis = TimeInMillis::from_millis(5000).unwrap();
		let time_secs: TimeInSeconds = time_millis.convert().unwrap();

		assert_eq!(time_secs.as_secs(), 5);
		assert_eq!(time_secs.as_millis(), 5000);
	}

	#[test]
	fn test_convert_precision_loss() {
		// Converting 1234 millis to seconds loses precision
		type TimeInMillis = Timescale<1_000>;
		type TimeInSeconds = Timescale<1>;

		let time_millis = TimeInMillis::from_millis(1234).unwrap();
		let time_secs: TimeInSeconds = time_millis.convert().unwrap();

		// 1234 millis = 1.234 seconds, rounds down to 1 second
		assert_eq!(time_secs.as_secs(), 1);
		assert_eq!(time_secs.as_millis(), 1000); // Lost 234 millis
	}

	#[test]
	fn test_convert_roundtrip() {
		// Converting to finer and back should preserve value
		type TimeInMillis = Timescale<1_000>;
		type TimeInMicros = Timescale<1_000_000>;

		let original = TimeInMillis::from_millis(5000).unwrap();
		let as_micros: TimeInMicros = original.convert().unwrap();
		let back_to_millis: TimeInMillis = as_micros.convert().unwrap();

		assert_eq!(original.as_millis(), back_to_millis.as_millis());
	}

	#[test]
	fn test_convert_same_scale() {
		// Converting to the same scale should be identity
		type TimeInMillis = Timescale<1_000>;

		let time = TimeInMillis::from_millis(5000).unwrap();
		let converted: TimeInMillis = time.convert().unwrap();

		assert_eq!(time.as_millis(), converted.as_millis());
	}

	#[test]
	fn test_convert_microseconds_to_nanoseconds() {
		type TimeInMicros = Timescale<1_000_000>;
		type TimeInNanos = Timescale<1_000_000_000>;

		let time_micros = TimeInMicros::from_micros(5_000_000).unwrap();
		let time_nanos: TimeInNanos = time_micros.convert().unwrap();

		assert_eq!(time_nanos.as_micros(), 5_000_000);
		assert_eq!(time_nanos.as_nanos(), 5_000_000_000);
	}

	#[test]
	fn test_convert_custom_scales() {
		// Test with unusual custom scales
		type TimeScale60 = Timescale<60>; // 60Hz
		type TimeScale90 = Timescale<90>; // 90Hz

		let time60 = TimeScale60::from_scale(120, 60).unwrap(); // 2 seconds at 60Hz
		let time90: TimeScale90 = time60.convert().unwrap();

		// Both should represent 2 seconds
		assert_eq!(time60.as_secs(), 2);
		assert_eq!(time90.as_secs(), 2);
	}

	#[test]
	fn test_debug_format_units() {
		// Test that Debug chooses appropriate units based on value

		// Milliseconds that are clean seconds
		let t = Time::from_millis(100000).unwrap();
		assert_eq!(format!("{:?}", t), "100s");

		let t = Time::from_millis(1000).unwrap();
		assert_eq!(format!("{:?}", t), "1s");

		// Milliseconds that are clean milliseconds
		let t = Time::from_millis(100).unwrap();
		assert_eq!(format!("{:?}", t), "100ms");

		let t = Time::from_millis(5500).unwrap();
		assert_eq!(format!("{:?}", t), "5500ms");

		// Zero
		let t = Time::ZERO;
		assert_eq!(format!("{:?}", t), "0s");

		// Test with microsecond-scale time
		type TimeMicros = Timescale<1_000_000>;
		let t = TimeMicros::from_micros(1500).unwrap();
		assert_eq!(format!("{:?}", t), "1500µs");

		let t = TimeMicros::from_micros(1000).unwrap();
		assert_eq!(format!("{:?}", t), "1ms");
	}
}
