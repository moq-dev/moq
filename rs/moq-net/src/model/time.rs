use rand::Rng;
use std::num::NonZeroU64;

use crate::coding::VarInt;

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Returned when a [`Timestamp`] operation would exceed the QUIC VarInt range
/// (`2^62 - 1`), overflow during scale conversion or arithmetic, or attempt
/// arithmetic between timestamps with mismatched scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("time overflow")]
pub struct TimeOverflow;

/// Units per second used by a track for frame timestamps.
///
/// Always non-zero, so divide-by-zero in scale conversion is impossible by
/// construction. Use the named constants ([`Self::SECOND`], [`Self::MILLI`],
/// [`Self::MICRO`], [`Self::NANO`]) where applicable, or [`Self::new`] for a
/// custom value. The wire encoding is a plain QUIC varint.
///
/// "No timescale negotiated" is expressed via [`Option<Timescale>`], not via
/// a sentinel value inside this type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timescale(NonZeroU64);

impl Timescale {
	/// One unit per second.
	pub const SECOND: Self = Self(match NonZeroU64::new(1) {
		Some(n) => n,
		None => unreachable!(),
	});
	/// 1,000 units per second.
	pub const MILLI: Self = Self(match NonZeroU64::new(1_000) {
		Some(n) => n,
		None => unreachable!(),
	});
	/// 1,000,000 units per second. Common default for media tracks.
	pub const MICRO: Self = Self(match NonZeroU64::new(1_000_000) {
		Some(n) => n,
		None => unreachable!(),
	});
	/// 1,000,000,000 units per second.
	pub const NANO: Self = Self(match NonZeroU64::new(1_000_000_000) {
		Some(n) => n,
		None => unreachable!(),
	});

	/// Construct a timescale from a raw value. Returns `None` for `0`.
	pub const fn new(units_per_second: u64) -> Option<Self> {
		match NonZeroU64::new(units_per_second) {
			Some(n) => Some(Self(n)),
			None => None,
		}
	}

	/// The raw units-per-second value (always non-zero).
	pub const fn get(self) -> u64 {
		self.0.get()
	}
}

impl std::fmt::Debug for Timescale {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match *self {
			Self::SECOND => write!(f, "Timescale::SECOND"),
			Self::MILLI => write!(f, "Timescale::MILLI"),
			Self::MICRO => write!(f, "Timescale::MICRO"),
			Self::NANO => write!(f, "Timescale::NANO"),
			Self(n) => write!(f, "Timescale({n})"),
		}
	}
}

impl std::fmt::Display for Timescale {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

/// A timestamp in a track's timescale (units per second).
///
/// Always carries a non-zero scale, so unit conversions and arithmetic can't
/// hit divide-by-zero. The underlying value is bounded by the QUIC VarInt
/// range (`2^62 - 1`). The scale itself is conveyed out-of-band (via
/// [`crate::Track::timescale`] or per-protocol negotiation), so the wire
/// encoding is just the raw varint value.
///
/// "No timestamp on this frame" is expressed as [`Option<Timestamp>`], not via
/// a sentinel value inside this type. See [`crate::Frame::timestamp`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timestamp {
	value: VarInt,
	scale: Timescale,
}

impl Timestamp {
	/// Construct a timestamp directly from a raw value at the given scale.
	pub const fn new(value: u64, scale: Timescale) -> Result<Self, TimeOverflow> {
		match VarInt::from_u64(value) {
			Some(value) => Ok(Self { value, scale }),
			None => Err(TimeOverflow),
		}
	}

	/// Convert `value` measured at `source` to a timestamp at `target`. Returns
	/// [`TimeOverflow`] if the rescaled value exceeds `2^62 - 1`.
	pub const fn from_scale(value: u64, source: Timescale, target: Timescale) -> Result<Self, TimeOverflow> {
		match (value as u128).checked_mul(target.get() as u128) {
			Some(scaled) => match VarInt::from_u128(scaled / source.get() as u128) {
				Some(value) => Ok(Self { value, scale: target }),
				None => Err(TimeOverflow),
			},
			None => Err(TimeOverflow),
		}
	}

	/// Convert a number of seconds to a timestamp at [`Timescale::SECOND`].
	pub const fn from_secs(seconds: u64) -> Result<Self, TimeOverflow> {
		Self::new(seconds, Timescale::SECOND)
	}

	/// Like [`Self::from_secs`] but panics on overflow.
	pub const fn from_secs_unchecked(seconds: u64) -> Self {
		match Self::from_secs(seconds) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of milliseconds to a timestamp at [`Timescale::MILLI`].
	pub const fn from_millis(millis: u64) -> Result<Self, TimeOverflow> {
		Self::new(millis, Timescale::MILLI)
	}

	/// Like [`Self::from_millis`] but panics on overflow.
	pub const fn from_millis_unchecked(millis: u64) -> Self {
		match Self::from_millis(millis) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of microseconds to a timestamp at [`Timescale::MICRO`].
	pub const fn from_micros(micros: u64) -> Result<Self, TimeOverflow> {
		Self::new(micros, Timescale::MICRO)
	}

	/// Like [`Self::from_micros`] but panics on overflow.
	pub const fn from_micros_unchecked(micros: u64) -> Self {
		match Self::from_micros(micros) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// Convert a number of nanoseconds to a timestamp at [`Timescale::NANO`].
	pub const fn from_nanos(nanos: u64) -> Result<Self, TimeOverflow> {
		Self::new(nanos, Timescale::NANO)
	}

	/// Like [`Self::from_nanos`] but panics on overflow.
	pub const fn from_nanos_unchecked(nanos: u64) -> Self {
		match Self::from_nanos(nanos) {
			Ok(time) => time,
			Err(_) => panic!("time overflow"),
		}
	}

	/// The raw value in the timestamp's own scale.
	pub const fn value(self) -> u64 {
		self.value.into_inner()
	}

	/// The scale attached to this timestamp (always non-zero).
	pub const fn scale(self) -> Timescale {
		self.scale
	}

	/// Whether the raw value is zero.
	pub const fn is_zero(self) -> bool {
		self.value.into_inner() == 0
	}

	/// Re-express this timestamp at a new scale. Returns [`TimeOverflow`] if the
	/// new value would exceed `2^62 - 1`.
	pub const fn convert(self, new_scale: Timescale) -> Result<Self, TimeOverflow> {
		if self.scale.get() == new_scale.get() {
			return Ok(self);
		}
		Self::from_scale(self.value.into_inner(), self.scale, new_scale)
	}

	/// The value re-expressed at `target` as a `u128`.
	pub const fn as_scale(self, target: Timescale) -> u128 {
		self.value.into_inner() as u128 * target.get() as u128 / self.scale.get() as u128
	}

	/// The value re-expressed in seconds.
	pub const fn as_secs(self) -> u64 {
		self.value.into_inner() / self.scale.get()
	}

	/// The value re-expressed in milliseconds.
	pub const fn as_millis(self) -> u128 {
		self.as_scale(Timescale::MILLI)
	}

	/// The value re-expressed in microseconds.
	pub const fn as_micros(self) -> u128 {
		self.as_scale(Timescale::MICRO)
	}

	/// The value re-expressed in nanoseconds.
	pub const fn as_nanos(self) -> u128 {
		self.as_scale(Timescale::NANO)
	}

	/// Return the larger of two timestamps.
	///
	/// Panics if the scales differ. Use [`Self::convert`] first if you need to compare
	/// across scales.
	pub const fn max(self, other: Self) -> Self {
		assert!(self.scale.get() == other.scale.get(), "mismatched timestamp scales");
		if self.value.into_inner() > other.value.into_inner() {
			self
		} else {
			other
		}
	}

	/// Add two timestamps. Returns [`TimeOverflow`] if the sum exceeds `2^62 - 1` or
	/// if the scales differ.
	pub const fn checked_add(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale.get() != rhs.scale.get() {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_add(rhs.value.into_inner()) {
			Some(result) => Self::new(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Subtract `rhs` from `self`. Returns [`TimeOverflow`] if `rhs > self` or if the
	/// scales differ.
	pub const fn checked_sub(self, rhs: Self) -> Result<Self, TimeOverflow> {
		if self.scale.get() != rhs.scale.get() {
			return Err(TimeOverflow);
		}
		match self.value.into_inner().checked_sub(rhs.value.into_inner()) {
			Some(result) => Self::new(result, self.scale),
			None => Err(TimeOverflow),
		}
	}

	/// Apply a signed delta in this timestamp's scale, returning the new timestamp.
	///
	/// Used by the moq-lite per-frame delta decoder: timestamps are encoded as zigzag
	/// signed deltas (negative for B-frames). Returns [`TimeOverflow`] if the result
	/// would underflow zero or overflow `2^62 - 1`.
	pub const fn checked_add_delta(self, delta: i64) -> Result<Self, TimeOverflow> {
		let current = self.value.into_inner() as i128;
		let next = current + delta as i128;
		if next < 0 {
			return Err(TimeOverflow);
		}
		match VarInt::from_u128(next as u128) {
			Some(value) => Ok(Self {
				value,
				scale: self.scale,
			}),
			None => Err(TimeOverflow),
		}
	}

	/// The signed delta from `prev` to `self` in their shared scale. Returns
	/// [`TimeOverflow`] on scale mismatch or if the delta is outside `i64::MIN..=i64::MAX`.
	pub const fn checked_delta_from(self, prev: Self) -> Result<i64, TimeOverflow> {
		if self.scale.get() != prev.scale.get() {
			return Err(TimeOverflow);
		}
		let a = self.value.into_inner() as i128;
		let b = prev.value.into_inner() as i128;
		let delta = a - b;
		if delta < i64::MIN as i128 || delta > i64::MAX as i128 {
			return Err(TimeOverflow);
		}
		Ok(delta as i64)
	}

	/// Current time, expressed in microseconds ([`Timescale::MICRO`]). Uses
	/// [`tokio::time::Instant::now`] so it honors `tokio::time::pause` in tests.
	pub fn now() -> Self {
		tokio::time::Instant::now().into()
	}
}

impl TryFrom<std::time::Duration> for Timestamp {
	type Error = TimeOverflow;

	/// Convert a [`std::time::Duration`] into a nanosecond-scale timestamp.
	fn try_from(duration: std::time::Duration) -> Result<Self, Self::Error> {
		match VarInt::from_u128(duration.as_nanos()) {
			Some(value) => Ok(Self {
				value,
				scale: Timescale::NANO,
			}),
			None => Err(TimeOverflow),
		}
	}
}

impl From<Timestamp> for std::time::Duration {
	fn from(time: Timestamp) -> Self {
		let secs = time.as_secs();
		let nanos = time.as_nanos();
		std::time::Duration::new(secs, (nanos % 1_000_000_000) as u32)
	}
}

impl std::fmt::Debug for Timestamp {
	#[allow(clippy::manual_is_multiple_of)] // is_multiple_of is unstable in Rust 1.85
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Choose the largest unit where we don't need decimal places. We use
		// u128 to capture nanos exactly even for nanosecond-scale timestamps
		// at the VarInt max.
		let nanos = self.as_nanos();
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

impl PartialOrd for Timestamp {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Timestamp {
	/// Compare by raw value. Debug-asserts that scales match. In release,
	/// cross-scale comparisons fall back to raw-value comparison, which is
	/// meaningless. Convert to a common scale first if you need to mix.
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		debug_assert_eq!(
			self.scale.get(),
			other.scale.get(),
			"comparing timestamps with mismatched scales: {} vs {}",
			self.scale,
			other.scale,
		);
		self.value.cmp(&other.value)
	}
}

impl std::ops::Add for Timestamp {
	type Output = Self;

	fn add(self, rhs: Self) -> Self {
		self.checked_add(rhs).expect("time overflow or scale mismatch")
	}
}

impl std::ops::AddAssign for Timestamp {
	fn add_assign(&mut self, rhs: Self) {
		*self = *self + rhs;
	}
}

impl std::ops::Sub for Timestamp {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		self.checked_sub(rhs).expect("time overflow or scale mismatch")
	}
}

impl std::ops::SubAssign for Timestamp {
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

impl From<std::time::Instant> for Timestamp {
	/// Convert an [`std::time::Instant`] into a microsecond-scale timestamp anchored to a
	/// jittered wall-clock reference (see `TIME_ANCHOR`).
	fn from(instant: std::time::Instant) -> Self {
		let (anchor_instant, anchor_system) = *TIME_ANCHOR;

		let system = match instant.checked_duration_since(anchor_instant) {
			Some(forward) => anchor_system + forward,
			None => anchor_system - anchor_instant.duration_since(instant),
		};

		let duration = system
			.duration_since(UNIX_EPOCH)
			.expect("dude your clock is earlier than 1970");

		Self::from_micros(duration.as_micros() as u64).expect("dude your clock is later than 2116")
	}
}

impl From<tokio::time::Instant> for Timestamp {
	fn from(instant: tokio::time::Instant) -> Self {
		instant.into_std().into()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_from_secs() {
		let time = Timestamp::from_secs(5).unwrap();
		assert_eq!(time.scale(), Timescale::SECOND);
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
		assert_eq!(time.as_micros(), 5_000_000);
		assert_eq!(time.as_nanos(), 5_000_000_000);
	}

	#[test]
	fn test_from_millis() {
		let time = Timestamp::from_millis(5000).unwrap();
		assert_eq!(time.scale(), Timescale::MILLI);
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_millis(), 5000);
	}

	#[test]
	fn test_from_micros() {
		let time = Timestamp::from_micros(5_000_000).unwrap();
		assert_eq!(time.scale(), Timescale::MICRO);
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_micros(), 5_000_000);
	}

	#[test]
	fn test_from_nanos() {
		let time = Timestamp::from_nanos(5_000_000_000).unwrap();
		assert_eq!(time.scale(), Timescale::NANO);
		assert_eq!(time.as_secs(), 5);
		assert_eq!(time.as_nanos(), 5_000_000_000);
	}

	#[test]
	fn test_zero_value() {
		let time = Timestamp::from_millis(0).unwrap();
		assert!(time.is_zero());
		assert_eq!(time.as_millis(), 0);
	}

	#[test]
	fn test_convert_to_finer() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_us = time_ms.convert(Timescale::MICRO).unwrap();
		assert_eq!(time_us.scale(), Timescale::MICRO);
		assert_eq!(time_us.as_micros(), 5_000_000);
	}

	#[test]
	fn test_convert_to_coarser() {
		let time_ms = Timestamp::from_millis(5000).unwrap();
		let time_s = time_ms.convert(Timescale::SECOND).unwrap();
		assert_eq!(time_s.scale(), Timescale::SECOND);
		assert_eq!(time_s.as_secs(), 5);
	}

	#[test]
	fn test_convert_precision_loss() {
		// 1234 ms = 1.234 s, rounds down to 1 s
		let time_ms = Timestamp::from_millis(1234).unwrap();
		let time_s = time_ms.convert(Timescale::SECOND).unwrap();
		assert_eq!(time_s.as_secs(), 1);
	}

	#[test]
	fn test_convert_roundtrip() {
		let original = Timestamp::from_millis(5000).unwrap();
		let as_micros = original.convert(Timescale::MICRO).unwrap();
		let back = as_micros.convert(Timescale::MILLI).unwrap();
		assert_eq!(original.value(), back.value());
		assert_eq!(original.scale(), back.scale());
	}

	#[test]
	fn test_convert_same_scale() {
		let time = Timestamp::from_millis(5000).unwrap();
		let converted = time.convert(Timescale::MILLI).unwrap();
		assert_eq!(time, converted);
	}

	#[test]
	fn test_add_same_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		let c = a.checked_add(b).unwrap();
		assert_eq!(c.as_millis(), 3000);
		assert_eq!(c.scale(), Timescale::MILLI);
	}

	#[test]
	fn test_add_mismatched_scale() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_micros(1000).unwrap();
		assert!(a.checked_add(b).is_err());
	}

	#[test]
	fn test_sub_underflow() {
		let a = Timestamp::from_millis(1000).unwrap();
		let b = Timestamp::from_millis(2000).unwrap();
		assert!(a.checked_sub(b).is_err());
	}

	#[test]
	fn test_max_same_scale() {
		let a = Timestamp::from_secs(5).unwrap();
		let b = Timestamp::from_secs(10).unwrap();
		assert_eq!(a.max(b), b);
		assert_eq!(b.max(a), b);
	}

	#[test]
	#[should_panic(expected = "mismatched timestamp scales")]
	fn test_max_mismatched_scale_panics() {
		let a = Timestamp::from_millis(1).unwrap();
		let b = Timestamp::from_secs(1).unwrap();
		let _ = a.max(b);
	}

	#[test]
	fn test_ordering_same_scale() {
		let a = Timestamp::from_secs(1).unwrap();
		let b = Timestamp::from_secs(2).unwrap();
		assert!(a < b);
		assert!(b > a);
		assert_eq!(a, a);
	}

	#[test]
	fn test_delta_positive() {
		let prev = Timestamp::from_millis(100).unwrap();
		let curr = Timestamp::from_millis(150).unwrap();
		assert_eq!(curr.checked_delta_from(prev).unwrap(), 50);
	}

	#[test]
	fn test_delta_negative() {
		let prev = Timestamp::from_millis(150).unwrap();
		let curr = Timestamp::from_millis(100).unwrap();
		assert_eq!(curr.checked_delta_from(prev).unwrap(), -50);
	}

	#[test]
	fn test_delta_mismatched_scale() {
		let prev = Timestamp::from_millis(100).unwrap();
		let curr = Timestamp::from_micros(150).unwrap();
		assert!(curr.checked_delta_from(prev).is_err());
	}

	#[test]
	fn test_add_delta_positive() {
		let t = Timestamp::from_millis(100).unwrap();
		let next = t.checked_add_delta(50).unwrap();
		assert_eq!(next.as_millis(), 150);
	}

	#[test]
	fn test_add_delta_negative() {
		let t = Timestamp::from_millis(150).unwrap();
		let next = t.checked_add_delta(-50).unwrap();
		assert_eq!(next.as_millis(), 100);
	}

	#[test]
	fn test_add_delta_underflow() {
		let t = Timestamp::from_millis(50).unwrap();
		assert!(t.checked_add_delta(-100).is_err());
	}

	#[test]
	fn test_duration_conversion() {
		let duration = std::time::Duration::from_secs(5);
		let time: Timestamp = duration.try_into().unwrap();
		assert_eq!(time.scale(), Timescale::NANO);
		assert_eq!(time.as_secs(), 5);

		let duration_back: std::time::Duration = time.into();
		assert_eq!(duration_back.as_secs(), 5);
	}

	#[test]
	fn test_debug_format_units() {
		let t = Timestamp::from_millis(100_000).unwrap();
		assert_eq!(format!("{:?}", t), "100s");

		let t = Timestamp::from_millis(100).unwrap();
		assert_eq!(format!("{:?}", t), "100ms");

		let t = Timestamp::from_micros(1500).unwrap();
		assert_eq!(format!("{:?}", t), "1500µs");

		let t = Timestamp::from_micros(1000).unwrap();
		assert_eq!(format!("{:?}", t), "1ms");
	}

	#[test]
	fn test_new() {
		let t = Timestamp::new(5000, Timescale::MILLI).unwrap();
		assert_eq!(t.value(), 5000);
		assert_eq!(t.scale(), Timescale::MILLI);
		assert_eq!(t.as_millis(), 5000);
	}

	#[test]
	fn test_from_scale_custom() {
		// 120 units at 60Hz = 2 seconds, expressed at 1000Hz = 2000 ms.
		let scale60 = Timescale::new(60).unwrap();
		let t = Timestamp::from_scale(120, scale60, Timescale::MILLI).unwrap();
		assert_eq!(t.scale(), Timescale::MILLI);
		assert_eq!(t.as_millis(), 2000);
	}

	#[test]
	fn test_timescale_new_zero_rejected() {
		assert!(Timescale::new(0).is_none());
		assert_eq!(Timescale::new(48_000).unwrap().get(), 48_000);
	}
}
